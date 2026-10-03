use anyhow::{ensure, Context, Result};
use openraid::{config::Config, runtime::Harness, storage::Store};
use serde_json::{json, Value};
use std::{collections::HashSet, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
};

async fn wait_for_round(store: &Store, count: usize) -> Result<()> {
    loop {
        if store
            .read_board(0, 10_000)
            .await?
            .iter()
            .filter(|message| message.body == "all workers drained; swarm complete")
            .count()
            >= count
        {
            return Ok(());
        }
        tokio::task::yield_now().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_idle_mutations_are_durable_and_persistent_rounds_use_current_roster() -> Result<()>
{
    tokio::time::timeout(Duration::from_secs(20), async {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("parallel.sqlite3");
        let harness = Harness::new(Config {
            agents: 2,
            workspace: directory.path().to_owned(),
            database: database.clone(),
            mock: true,
            interactive_session: true,
            objective: String::new(),
            grace_period: Duration::ZERO,
            ..Config::default()
        })
        .await?;
        let control = harness.control.clone();
        let store = harness.store.clone();
        let (first, second) = tokio::join!(control.add_agents(2), control.add_agents(3));
        let first = first?;
        let second = second?;
        let allocated: HashSet<_> = first.iter().chain(&second).cloned().collect();
        assert_eq!(
            allocated.len(),
            5,
            "parallel additions must allocate unique IDs"
        );
        assert_eq!(control.members().len(), 7);
        let (removed_first, removed_second) = tokio::join!(
            control.remove_agents(first.clone()),
            control.remove_agents(second[..2].to_vec())
        );
        removed_first?;
        removed_second?;
        let mut expected: HashSet<_> = [
            "agent-001".to_owned(),
            "agent-002".to_owned(),
            second[2].clone(),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            control.members().into_iter().collect::<HashSet<_>>(),
            expected
        );
        let notices = store.read_board(0, 10_000).await?;
        assert!(
            notices.is_empty(),
            "pre-objective roster controls must not pollute the messageboard"
        );
        for id in first.iter().chain(&second[..2]) {
            assert!(
                store.vote(id).await?.is_none(),
                "retired votes cannot survive removal"
            );
        }
        assert!(
            store.prompts().await?.is_empty(),
            "membership controls are not task prompts"
        );
        store.flush().await?;
        let reopened = Store::open(&database).await?;
        assert_eq!(
            reopened.read_board(0, 10_000).await?,
            notices,
            "pre-objective board silence survives an independent database reopen"
        );
        assert_eq!(
            reopened
                .membership()
                .await?
                .agent_ids
                .into_iter()
                .collect::<HashSet<_>>(),
            expected,
            "silent pre-objective roster changes are still durable"
        );
        let run = tokio::spawn(harness.run());
        for (index, prompt) in ["first roster-aware round", "second roster-aware round"]
            .into_iter()
            .enumerate()
        {
            control.post_prompt(prompt.into()).await?;
            wait_for_round(&store, index + 1).await?;
            while control.is_busy() {
                tokio::task::yield_now().await;
            }
            assert!(
                !run.is_finished(),
                "persistent console stays alive between rounds"
            );
            assert_eq!(
                control.members().into_iter().collect::<HashSet<_>>(),
                expected
            );
        }
        let idle_addition = control.add_agents(1).await?;
        assert_eq!(idle_addition.len(), 1);
        assert!(expected.insert(idle_addition[0].clone()));
        control.detach();
        let summary = run.await??;
        assert_eq!(
            summary.agents, 4,
            "idle membership changes must update the final active-roster summary"
        );
        assert_eq!(summary.finished_agents, 3);
        assert!(
            control.draining_members().is_empty(),
            "completed idle console has no stale drain signals"
        );
        assert_eq!(store.prompts().await?.len(), 2);
        let board = store.read_board(0, 10_000).await?;
        for id in first.iter().chain(&second[..2]) {
            assert!(
                !board.iter().any(|message| message.sender == *id
                    && message.body == "offline smoke: present and cooperating"),
                "removed idle member {id} must not spawn in later rounds"
            );
        }
        let resumed = Harness::new(Config {
            agents: 2,
            workspace: directory.path().to_owned(),
            database,
            mock: true,
            interactive_session: true,
            objective: String::new(),
            resume: true,
            grace_period: Duration::ZERO,
            ..Config::default()
        })
        .await?;
        let resumed_control = resumed.control.clone();
        assert_eq!(
            resumed_control
                .members()
                .into_iter()
                .collect::<HashSet<_>>(),
            expected,
            "resume restores the durable dynamic roster rather than the original CLI count"
        );
        resumed_control
            .post_prompt("third round after reopening the session".into())
            .await?;
        let resumed_run = tokio::spawn(resumed.run());
        wait_for_round(&store, 3).await?;
        while resumed_control.is_busy() {
            tokio::task::yield_now().await;
        }
        resumed_control.detach();
        assert_eq!(resumed_run.await??.finished_agents, 4);
        assert_eq!(store.prompts().await?.len(), 3);
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test]
async fn rejected_membership_changes_do_not_publish_notices_or_change_roster() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let harness = Harness::new(Config {
        agents: 2,
        workspace: directory.path().to_owned(),
        database: directory.path().join("invalid.sqlite3"),
        mock: true,
        interactive_session: true,
        objective: String::new(),
        ..Config::default()
    })
    .await?;
    let before = harness.control.members();
    let cursor = harness.store.latest_seq().await?;
    harness
        .store
        .set_vote(
            &before[0],
            true,
            "existing evidence must survive invalid membership controls",
        )
        .await?;
    let votes = harness.store.votes().await?;
    assert!(harness.control.add_agents(0).await.is_err());
    assert!(harness.control.add_agents(499).await.is_err());
    assert!(harness.control.add_agents(usize::MAX).await.is_err());
    assert!(
        harness.control.remove_agents(before.clone()).await.is_err(),
        "removing the last active member is rejected atomically"
    );
    assert!(
        harness
            .control
            .remove_agents(vec![before[0].clone(), "agent-missing".into()])
            .await
            .is_err(),
        "a mixed valid/unknown batch must not partially remove workers"
    );
    assert_eq!(harness.control.members(), before);
    assert_eq!(harness.store.latest_seq().await?, cursor);
    assert_eq!(
        harness.store.votes().await?,
        votes,
        "rejected batches must not revoke or rewrite existing votes"
    );
    Ok(())
}

async fn read_request(socket: &mut TcpStream) -> Result<Value> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    let end = loop {
        let count = socket.read(&mut chunk).await?;
        ensure!(count > 0, "request closed before headers");
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let length = std::str::from_utf8(&bytes[..end])?
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .context("missing request body length")?;
    while bytes.len() < end + length {
        let count = socket.read(&mut chunk).await?;
        ensure!(count > 0, "request body closed early");
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(serde_json::from_slice(&bytes[end..end + length])?)
}

struct PendingRequest {
    id: String,
    body: Value,
    release: oneshot::Sender<()>,
}

#[derive(Clone, Copy)]
enum Probe {
    None,
    Retirement,
    RunningTool,
}

async fn fixture(
    listener: TcpListener,
    events: mpsc::Sender<PendingRequest>,
    gate_followups: bool,
    probe: Probe,
) -> Result<()> {
    let mut requests = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            incoming = listener.accept() => {
                let (mut socket, _) = incoming?;
                let events = events.clone();
                requests.spawn(async move {
                    let body = read_request(&mut socket).await?;
                    let messages = body["messages"].as_array().context("missing messages")?;
                    let id = messages.iter().find_map(|message| {
                        message["content"].as_str()?.strip_prefix("you are ")?
                            .split_once('.').map(|(id, _)| id.to_owned())
                    }).context("missing worker identity")?;
                    let first = !messages.iter().any(|message| message["role"] == "assistant");
                    if first || gate_followups {
                        let (release, wait) = oneshot::channel();
                        events.send(PendingRequest {id: id.clone(), body, release}).await?;
                        wait.await?;
                    }
                    let name = if first {"board_read"} else {"vote_done"};
                    let args = if first {json!({"after":0,"limit":100})}
                        else {json!({"done":true,"evidence":"verified current global board and membership"})};
                    let mut tool_calls = vec![json!({"id":format!("{id}-{name}"),"type":"function","function":{
                        "name":name,"arguments":args.to_string()
                    }})];
                    if first && matches!(probe, Probe::Retirement) && id == "agent-001" {
                        for (name, args) in [
                            ("apply_patch", json!({"patch":"*** Begin Patch\n*** Add File: should_not_exist.txt\n+retired worker launched a new mutation\n*** End Patch"})),
                            ("run_command", json!({"command":"echo retired > should_not_run.txt"})),
                        ] {
                            tool_calls.push(json!({"id":format!("{id}-{name}"),"type":"function","function":{
                                "name":name,"arguments":args.to_string()
                            }}));
                        }
                    }
                    if first && matches!(probe, Probe::RunningTool) && id == "agent-001" {
                        let command = if cfg!(windows) {
                            "[System.IO.File]::AppendAllText((Join-Path $PWD 'tool-started.txt'),'started'); while (-not (Test-Path -LiteralPath 'tool-release.txt')) { Start-Sleep -Milliseconds 10 }; 'tool drained'"
                        } else {
                            "printf started >> tool-started.txt; while [ ! -f tool-release.txt ]; do sleep 0.01; done; printf 'tool drained'"
                        };
                        for (name, args) in [
                            ("run_command", json!({"command":command})),
                            ("apply_patch", json!({"patch":"*** Begin Patch\n*** Add File: should_not_exist.txt\n+retired worker launched a new mutation\n*** End Patch"})),
                        ] {
                            tool_calls.push(json!({"id":format!("{id}-{name}"),"type":"function","function":{
                                "name":name,"arguments":args.to_string()
                            }}));
                        }
                    }
                    let body = json!({"choices":[{"finish_reason":"stop","message":{
                        "role":"assistant","content":format!("completed request for {id}"),
                        "tool_calls":tool_calls
                    }}]}).to_string();
                    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await?;
                    socket.shutdown().await?;
                    Ok::<_, anyhow::Error>(())
                });
            },
            result = requests.join_next(), if !requests.is_empty() => { result.unwrap()??; }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removed_worker_drains_http_and_checkpoint_while_added_worker_reads_full_global_board(
) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(20), async {
        let directory = tempfile::tempdir()?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (events_tx, mut events) = mpsc::channel(10);
        let server = tokio::spawn(fixture(listener, events_tx, false, Probe::Retirement));
        let harness = Harness::new(Config {
            agents: 2,
            workspace: directory.path().to_owned(),
            database: directory.path().join("draining.sqlite3"),
            objective: "verify changing membership without cancelling provider work".into(),
            base_url: format!("http://{address}/v1"),
            max_in_flight: 3,
            grace_period: Duration::ZERO,
            ..Config::default()
        })
        .await?;
        let control = harness.control.clone();
        let store = harness.store.clone();
        let run = tokio::spawn(harness.run());
        let mut initial = vec![
            events.recv().await.context("first request absent")?,
            events.recv().await.context("second request absent")?,
        ];
        initial.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(initial[0].id, "agent-001");
        assert_eq!(initial[1].id, "agent-002");
        store
            .append(
                "agent-002",
                "global history sentinel before late join",
                false,
            )
            .await?;
        let (removed, added) = tokio::join!(
            control.remove_agents(vec!["agent-001".into()]),
            control.add_agents(1)
        );
        removed?;
        assert_eq!(added?, vec!["agent-003"]);
        assert!(!control.members().contains(&"agent-001".into()));
        assert!(
            !run.is_finished(),
            "removal cannot finish a blocked provider request"
        );
        assert!(
            store
                .read_board(0, 100)
                .await?
                .iter()
                .all(|entry| entry.sender != "agent-001" || !entry.body.contains("quitting")),
            "removed worker must not announce drained exit before its HTTP response"
        );
        let late = events.recv().await.context("late worker request absent")?;
        assert_eq!(late.id, "agent-003");
        let wire = late.body["messages"].to_string();
        assert!(wire.contains("global history sentinel before late join"));
        assert!(wire.contains("verify changing membership without cancelling provider work"));
        assert!(
            wire.contains("agent-001"),
            "late worker receives retirement notice on global board"
        );
        initial.remove(0).release.send(()).unwrap();
        loop {
            if store
                .read_board(0, 100)
                .await?
                .iter()
                .any(|entry| entry.sender == "agent-001" && entry.body.contains("quitting"))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        let checkpoint = store
            .load_checkpoint("agent-001")
            .await?
            .context("retired checkpoint absent")?;
        let messages = checkpoint["messages"]
            .as_array()
            .context("checkpoint messages absent")?;
        assert!(messages.iter().any(|message| message["role"] == "assistant"
            && message["content"] == "completed request for agent-001"));
        assert!(
            messages.iter().any(|message| message["role"] == "tool"
                && message["tool_call_id"] == "agent-001-board_read"),
            "drain persists the HTTP response and native tool result before exit"
        );
        for name in ["board_read", "apply_patch", "run_command"] {
            let result = messages
                .iter()
                .find(|message| {
                    message["role"] == "tool"
                        && message["tool_call_id"] == format!("agent-001-{name}")
                })
                .context("retired protocol group missing a tool result")?;
            let content: Value = serde_json::from_str(
                result["content"]
                    .as_str()
                    .context("tool result missing content")?,
            )?;
            assert_eq!(
                content["skipped"], true,
                "new {name} tool must be skipped after retirement rather than dispatched"
            );
        }
        assert!(!directory.path().join("should_not_exist.txt").exists());
        assert!(!directory.path().join("should_not_run.txt").exists());
        assert!(store.vote("agent-001").await?.is_none());
        assert!(
            !run.is_finished(),
            "remaining blocked workers still lack quorum"
        );
        initial.remove(0).release.send(()).unwrap();
        late.release.send(()).unwrap();
        let summary = run.await??;
        assert_eq!(summary.agents, 2);
        assert_eq!(
            summary.finished_agents, 3,
            "summary includes the retired in-flight worker"
        );
        assert_eq!(
            control.members().into_iter().collect::<HashSet<_>>(),
            ["agent-002".to_owned(), "agent-003".to_owned()]
                .into_iter()
                .collect()
        );
        let board = store.read_board(0, 100).await?;
        assert_eq!(
            board
                .iter()
                .filter(|entry| entry.sender == "agent-001" && entry.body.contains("quitting"))
                .count(),
            1,
            "a gracefully removed worker never restarts or exits twice"
        );
        assert_eq!(
            board
                .iter()
                .filter(|entry| entry.body == "all workers drained; swarm complete")
                .count(),
            1
        );
        server.abort();
        let _ = server.await;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn additions_count_removed_workers_until_their_inflight_http_requests_drain() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(20), async {
        let directory = tempfile::tempdir()?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (events_tx, mut events) = mpsc::channel(10);
        let server = tokio::spawn(fixture(listener, events_tx, false, Probe::None));
        let harness = Harness::new(Config {
            agents: 2,
            workspace: directory.path().to_owned(),
            database: directory.path().join("capacity.sqlite3"),
            objective: "verify additions respect active and draining worker capacity".into(),
            base_url: format!("http://{address}/v1"),
            max_in_flight: 2,
            grace_period: Duration::ZERO,
            ..Config::default()
        })
        .await?;
        let control = harness.control.clone();
        let store = harness.store.clone();
        let run = tokio::spawn(harness.run());
        let first = events.recv().await.context("first held request absent")?;
        let second = events.recv().await.context("second held request absent")?;
        control.remove_agents(vec!["agent-001".into()]).await?;
        let cursor = store.latest_seq().await?;
        assert!(
            control.add_agents(499).await.is_err(),
            "capacity includes the still-draining worker, not only the active roster"
        );
        assert_eq!(
            store.latest_seq().await?,
            cursor,
            "capacity rejection must not publish a partial addition"
        );
        assert_eq!(control.members(), vec!["agent-002"]);
        first.release.send(()).unwrap();
        second.release.send(()).unwrap();
        let summary = run.await??;
        assert_eq!(summary.agents, 1);
        assert_eq!(summary.finished_agents, 2);
        assert!(
            control.draining_members().is_empty(),
            "final drain clears retirement signals"
        );
        let final_cursor = store.latest_seq().await?;
        let (_, independent_false_watch) = tokio::sync::watch::channel(false);
        assert!(
            store
                .add_members(1, independent_false_watch, |_| {})
                .await
                .is_err(),
            "headless supervisor exit stays durably closed even for an independent caller"
        );
        assert_eq!(store.latest_seq().await?, final_cursor);
        server.abort();
        let _ = server.await;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_running_additions_and_removals_drain_every_worker_once() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(20), async {
        let directory = tempfile::tempdir()?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (events_tx, mut events) = mpsc::channel(32);
        let server = tokio::spawn(fixture(listener, events_tx, false, Probe::None));
        let harness = Harness::new(Config {
            agents: 2,
            workspace: directory.path().to_owned(),
            database: directory.path().join("parallel-running.sqlite3"),
            objective: "verify parallel running agent controls and complete all existing work"
                .into(),
            base_url: format!("http://{address}/v1"),
            max_in_flight: 12,
            grace_period: Duration::ZERO,
            ..Config::default()
        })
        .await?;
        let control = harness.control.clone();
        let store = harness.store.clone();
        let run = tokio::spawn(harness.run());
        let original = vec![
            events
                .recv()
                .await
                .context("first original request absent")?,
            events
                .recv()
                .await
                .context("second original request absent")?,
        ];
        let mut changes = tokio::task::JoinSet::new();
        for _ in 0..10 {
            let control = control.clone();
            changes.spawn(async move { control.add_agents(1).await });
        }
        let mut added_ids = HashSet::new();
        while let Some(change) = changes.join_next().await {
            let ids = change??;
            assert_eq!(
                ids.len(),
                1,
                "each parallel caller receives only its own added worker"
            );
            assert!(added_ids.insert(ids[0].clone()));
        }
        let mut added_requests = Vec::new();
        for _ in 0..10 {
            let request = events
                .recv()
                .await
                .context("parallel addition did not join ongoing work")?;
            assert!(added_ids.contains(&request.id));
            assert!(request.body["messages"]
                .to_string()
                .contains("complete all existing work"));
            added_requests.push(request);
        }
        let mut removals = tokio::task::JoinSet::new();
        for id in &added_ids {
            let control = control.clone();
            let id = id.clone();
            removals.spawn(async move { control.remove_agents(vec![id]).await });
        }
        while let Some(removal) = removals.join_next().await {
            removal??;
        }
        assert_eq!(control.members(), vec!["agent-001", "agent-002"]);
        for request in added_requests {
            request.release.send(()).unwrap();
        }
        loop {
            let board = store.read_board(0, 100).await?;
            if added_ids.iter().all(|id| {
                board
                    .iter()
                    .any(|entry| entry.sender == *id && entry.body.contains("quitting"))
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            !run.is_finished(),
            "original workers remain active while additions retire"
        );
        for request in original {
            request.release.send(()).unwrap();
        }
        let summary = run.await??;
        assert_eq!(summary.agents, 2);
        assert_eq!(summary.finished_agents, 12);
        let board = store.read_board(0, 100).await?;
        for id in &added_ids {
            assert_eq!(
                board
                    .iter()
                    .filter(|entry| entry.sender == *id && entry.body.contains("quitting"))
                    .count(),
                1
            );
            assert!(store.vote(id).await?.is_none());
        }
        server.abort();
        let _ = server.await;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removal_drains_an_already_running_command_and_skips_remaining_tool_calls() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(20), async {
        let directory = tempfile::tempdir()?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (events_tx, mut events) = mpsc::channel(10);
        let server = tokio::spawn(fixture(listener, events_tx, false, Probe::RunningTool));
        let harness = Harness::new(Config {
            agents: 2,
            workspace: directory.path().to_owned(),
            database: directory.path().join("tool-drain.sqlite3"),
            objective: "verify removal drains already-running native tools".into(),
            base_url: format!("http://{address}/v1"),
            max_in_flight: 2,
            grace_period: Duration::ZERO,
            ..Config::default()
        })
        .await?;
        let control = harness.control.clone();
        let store = harness.store.clone();
        let run = tokio::spawn(harness.run());
        let mut pending = vec![
            events.recv().await.context("first request absent")?,
            events.recv().await.context("second request absent")?,
        ];
        pending.sort_by(|a, b| a.id.cmp(&b.id));
        pending.remove(0).release.send(()).unwrap();
        while !directory.path().join("tool-started.txt").exists() {
            tokio::task::yield_now().await;
        }
        control.remove_agents(vec!["agent-001".into()]).await?;
        let cursor = store.latest_seq().await?;
        assert!(
            store
                .read_board(0, 100)
                .await?
                .iter()
                .all(|entry| entry.sender != "agent-001" || !entry.body.contains("quitting")),
            "retirement waits for the running command's result"
        );
        assert!(!run.is_finished());
        tokio::fs::write(directory.path().join("tool-release.txt"), b"release").await?;
        loop {
            if store
                .read_board(cursor, 100)
                .await?
                .iter()
                .any(|entry| entry.sender == "agent-001" && entry.body.contains("quitting"))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        let checkpoint = store
            .load_checkpoint("agent-001")
            .await?
            .context("retired tool checkpoint absent")?;
        let messages = checkpoint["messages"]
            .as_array()
            .context("checkpoint messages absent")?;
        let command = messages
            .iter()
            .find(|message| {
                message["role"] == "tool" && message["tool_call_id"] == "agent-001-run_command"
            })
            .context("running command result not checkpointed")?;
        let output: Value = serde_json::from_str(
            command["content"]
                .as_str()
                .context("missing command result")?,
        )?;
        assert!(
            output.get("skipped").is_none(),
            "already running command must actually finish"
        );
        assert!(
            output.to_string().contains("tool drained"),
            "command finishes naturally after release"
        );
        let mutation = messages
            .iter()
            .find(|message| {
                message["role"] == "tool" && message["tool_call_id"] == "agent-001-apply_patch"
            })
            .context("skipped remaining call missing protocol result")?;
        let output: Value = serde_json::from_str(
            mutation["content"]
                .as_str()
                .context("missing skipped result")?,
        )?;
        assert_eq!(output["skipped"], true);
        assert!(!directory.path().join("should_not_exist.txt").exists());
        pending.remove(0).release.send(()).unwrap();
        let summary = run.await??;
        assert_eq!(summary.agents, 1);
        assert_eq!(summary.finished_agents, 2);
        server.abort();
        let _ = server.await;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unexpected_checkpoint_interruption_restarts_same_agent_without_replaying_side_effects(
) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(20), async {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("automatic-recovery.sqlite3");
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (events_tx, mut events) = mpsc::channel(10);
        let server = tokio::spawn(fixture(listener, events_tx, true, Probe::RunningTool));
        let harness = Harness::new(Config {
            agents: 2,
            workspace: directory.path().to_owned(),
            database: database.clone(),
            objective:
                "automatically continue interrupted sessions without replaying unknown side effects"
                    .into(),
            base_url: format!("http://{address}/v1"),
            max_in_flight: 2,
            grace_period: Duration::ZERO,
            ..Config::default()
        })
        .await?;
        let store = harness.store.clone();
        let run = tokio::spawn(harness.run());
        let mut pending = vec![
            events.recv().await.context("first request absent")?,
            events.recv().await.context("second request absent")?,
        ];
        pending.sort_by(|a, b| a.id.cmp(&b.id));
        pending.remove(0).release.send(()).unwrap();
        while !directory.path().join("tool-started.txt").exists() {
            tokio::task::yield_now().await;
        }
        // Inject a genuine persistence failure exactly after an already-running
        // command finishes. Its admitted protocol group remains in the previous
        // checkpoint, but its side effect cannot be declared safely replayable.
        let connection = rusqlite::Connection::open(&database)?;
        connection.execute_batch(
            "CREATE TRIGGER interrupt_command_checkpoint BEFORE UPDATE OF state_json ON checkpoints
             WHEN NEW.agent_id = 'agent-001' AND EXISTS (
                 SELECT 1 FROM json_each(NEW.state_json, '$.messages') AS message
                 WHERE json_extract(message.value, '$.role') = 'tool'
                   AND json_extract(message.value, '$.tool_call_id') = 'agent-001-run_command'
                   AND instr(json_extract(message.value, '$.content'), 'tool drained') > 0
             ) BEGIN SELECT RAISE(FAIL, 'injected checkpoint interruption'); END;",
        )?;
        tokio::fs::write(directory.path().join("tool-release.txt"), b"release").await?;
        let recovered = events
            .recv()
            .await
            .context("interrupted worker never resumed automatically")?;
        assert_eq!(
            recovered.id, "agent-001",
            "supervisor retains the original worker identity"
        );
        let messages = recovered.body["messages"]
            .as_array()
            .context("recovered messages absent")?;
        assert!(
            messages.iter().any(|message| message["content"]
                .as_str()
                .is_some_and(|text| text.contains("automatically continue interrupted sessions"))),
            "original objective/history retained"
        );
        for name in ["run_command", "apply_patch"] {
            let repaired = messages
                .iter()
                .find(|message| {
                    message["role"] == "tool"
                        && message["tool_call_id"] == format!("agent-001-{name}")
                })
                .context("pending tool group was not repaired on automatic recovery")?;
            assert!(repaired["content"]
                .as_str()
                .is_some_and(|text| text.contains("outcome is unknown")));
        }
        assert!(!directory.path().join("should_not_exist.txt").exists());
        connection.execute_batch("DROP TRIGGER interrupt_command_checkpoint")?;
        drop(connection);
        recovered.release.send(()).unwrap();
        wait_for_votes(&store, 1).await?;
        pending.remove(0).release.send(()).unwrap();
        let final_vote = events
            .recv()
            .await
            .context("remaining worker vote absent")?;
        assert_eq!(final_vote.id, "agent-002");
        final_vote.release.send(()).unwrap();
        let summary = run.await??;
        assert_eq!(summary.agents, 2);
        assert_eq!(summary.finished_agents, 2);
        assert_eq!(
            tokio::fs::read_to_string(directory.path().join("tool-started.txt")).await?,
            "started",
            "automatic recovery must not repeat the completed command side effect"
        );
        assert_eq!(
            store
                .read_board(0, 100)
                .await?
                .iter()
                .filter(|entry| entry.sender == "harness"
                    && entry.body.contains("agent-001")
                    && entry.body.contains("restarting from durable checkpoint"))
                .count(),
            1,
            "unexpected exit posts one durable restart notice and continues automatically"
        );
        server.abort();
        let _ = server.await;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupted_persistent_session_resumes_unfinished_prompt_roster_and_checkpoint_automatically(
) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(20), async {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("unfinished-session.sqlite3");
        let store = Store::open(&database).await?;
        store.initialize_membership(2, false).await?;
        let (_, open_phase) = tokio::sync::watch::channel(false);
        store.add_members(1, open_phase, |_| {}).await?;
        let prompt = store.append("owner", "unfinished task from the interrupted persistent session", true).await?;
        store.save_checkpoint("agent-001", &json!({"cursor":prompt.seq,"messages":[
            {"role":"user","content":"previous session history sentinel"},
            {"role":"assistant","content":"pending side effect before interruption","tool_calls":[{
                "id":"interrupted-side-effect","type":"function","function":{
                    "name":"run_command","arguments":"{\"command\":\"echo replayed > should_not_exist.txt\"}"
                }
            }]}
        ]})).await?;
        store.flush().await?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (events_tx, mut events) = mpsc::channel(10);
        let server = tokio::spawn(fixture(listener, events_tx, true, Probe::None));
        let harness = Harness::new(Config {
            agents: 1,
            workspace: directory.path().to_owned(),
            database,
            interactive_session: true,
            objective: String::new(),
            resume: true,
            base_url: format!("http://{address}/v1"),
            max_in_flight: 3,
            grace_period: Duration::ZERO,
            ..Config::default()
        })
        .await?;
        let control = harness.control.clone();
        assert_eq!(control.members(), vec!["agent-001", "agent-002", "agent-003"]);
        let run = tokio::spawn(harness.run());
        let mut dispatched = HashSet::new();
        for _ in 0..3 {
            let request = events.recv().await.context("unfinished session stayed asleep rather than resuming")?;
            assert!(dispatched.insert(request.id.clone()));
            let wire = request.body["messages"].to_string();
            assert!(wire.contains("unfinished task from the interrupted persistent session"));
            if request.id == "agent-001" {
                assert!(wire.contains("previous session history sentinel"));
                assert!(wire.contains("interrupted-side-effect"));
                assert!(wire.contains("outcome is unknown"));
            }
            request.release.send(()).unwrap();
        }
        // The recovered worker votes directly; the two fresh peers first read
        // the board, then submit completion evidence in these follow-up calls.
        for _ in 0..2 {
            let vote = events.recv().await.context("resumed peer completion vote absent")?;
            assert_ne!(vote.id, "agent-001");
            vote.release.send(()).unwrap();
        }
        wait_for_round(&store, 1).await?;
        while control.is_busy() {
            tokio::task::yield_now().await;
        }
        assert!(!run.is_finished(), "recovered persistent console remains open after completion");
        assert_eq!(store.prompts().await?.len(), 1, "resume must not duplicate the original prompt");
        assert!(!directory.path().join("should_not_exist.txt").exists(), "pending side effect is never replayed");
        control.detach();
        let summary = run.await??;
        assert_eq!(summary.agents, 3);
        assert_eq!(summary.finished_agents, 3);
        server.abort();
        let _ = server.await;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

async fn wait_for_votes(store: &Store, count: usize) -> Result<()> {
    loop {
        let cursor = store.latest_seq().await?;
        if store
            .votes()
            .await?
            .iter()
            .filter(|vote| vote.done && vote.board_seq == cursor)
            .count()
            == count
        {
            return Ok(());
        }
        tokio::task::yield_now().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn changes_from_an_independent_store_handle_wake_and_reconcile_running_workers() -> Result<()>
{
    tokio::time::timeout(Duration::from_secs(20), async {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("external-members.sqlite3");
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (events_tx, mut events) = mpsc::channel(10);
        let server = tokio::spawn(fixture(listener, events_tx, false, Probe::Retirement));
        let harness = Harness::new(Config {
            agents: 2,
            workspace: directory.path().to_owned(),
            database: database.clone(),
            objective: "reconcile external durable membership without sleeping forever".into(),
            base_url: format!("http://{address}/v1"),
            max_in_flight: 3,
            grace_period: Duration::ZERO,
            ..Config::default()
        })
        .await?;
        let control = harness.control.clone();
        let run = tokio::spawn(harness.run());
        let first = events.recv().await.context("first held request absent")?;
        let second = events.recv().await.context("second held request absent")?;
        let external = Store::open(&database).await?;
        let (_, unchanged_shutdown) = tokio::sync::watch::channel(false);
        external
            .add_members(1, unchanged_shutdown.clone(), |_| {})
            .await?;
        let joined = events
            .recv()
            .await
            .context("external join never dispatched")?;
        assert_eq!(joined.id, "agent-003");
        assert!(control.members().contains(&joined.id));
        assert!(joined.body["messages"]
            .to_string()
            .contains("Owner added agent-003"));
        external
            .remove_members(&["agent-001".into()], unchanged_shutdown, |_| {})
            .await?;
        assert!(!run.is_finished());
        // Release immediately after the external transaction, before waiting for
        // local supervisor reconciliation. Tool admission must inspect durable
        // membership rather than depending on the next supervisor tick.
        first.release.send(()).unwrap();
        second.release.send(()).unwrap();
        joined.release.send(()).unwrap();
        let summary = run.await??;
        assert_eq!(summary.agents, 2);
        assert_eq!(summary.finished_agents, 3);
        let checkpoint = external
            .load_checkpoint("agent-001")
            .await?
            .context("external retired checkpoint absent")?;
        let messages = checkpoint["messages"]
            .as_array()
            .context("checkpoint messages absent")?;
        for name in ["board_read", "apply_patch", "run_command"] {
            let result = messages
                .iter()
                .find(|message| {
                    message["role"] == "tool"
                        && message["tool_call_id"] == format!("agent-001-{name}")
                })
                .context("external retirement did not preserve full protocol group")?;
            let output: Value = serde_json::from_str(
                result["content"]
                    .as_str()
                    .context("missing skipped tool content")?,
            )?;
            assert_eq!(
                output["skipped"], true,
                "external retirement must skip unstarted {name}"
            );
        }
        assert!(!directory.path().join("should_not_exist.txt").exists());
        assert!(!directory.path().join("should_not_run.txt").exists());
        let board = external.read_board(0, 100).await?;
        assert_eq!(
            board
                .iter()
                .filter(|entry| entry.sender == "agent-001" && entry.body.contains("quitting"))
                .count(),
            1
        );
        server.abort();
        let _ = server.await;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn waiting_voted_workers_wake_for_owner_peer_and_membership_work_without_waiting_for_other_requests(
) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(20), async {
        let directory = tempfile::tempdir()?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (events_tx, mut events) = mpsc::channel(10);
        let server = tokio::spawn(fixture(listener, events_tx, true, Probe::None));
        let harness = Harness::new(Config {
            agents: 3,
            workspace: directory.path().to_owned(),
            database: directory.path().join("wake.sqlite3"),
            objective: "verify waiting agents notice unfinished global work".into(),
            base_url: format!("http://{address}/v1"),
            max_in_flight: 4,
            grace_period: Duration::ZERO,
            ..Config::default()
        })
        .await?;
        let control = harness.control.clone();
        let store = harness.store.clone();
        let run = tokio::spawn(harness.run());
        let mut initial = Vec::new();
        for _ in 0..3 {
            initial.push(events.recv().await.context("initial request absent")?);
        }
        initial.sort_by(|a, b| a.id.cmp(&b.id));
        let held = initial.pop().unwrap();
        assert_eq!(held.id, "agent-003");
        for request in initial {
            request.release.send(()).unwrap();
        }
        for _ in 0..2 {
            events
                .recv()
                .await
                .context("initial completion vote absent")?
                .release
                .send(())
                .unwrap();
        }
        wait_for_votes(&store, 2).await?;
        // Two votes cannot reach the three-member threshold. The third real HTTP
        // request stays blocked throughout both wakeups; no sleep/grace race is needed.
        for (sender, text, owner) in [
            ("owner", "new owner work wakes every voted worker", true),
            (
                "agent-003",
                "peer found remaining work while its provider request is pending",
                false,
            ),
        ] {
            store.append(sender, text, owner).await?;
            let mut awakened = HashSet::new();
            for _ in 0..2 {
                let request = events.recv().await.context("waiting worker did not wake")?;
                assert_ne!(request.id, "agent-003");
                assert!(awakened.insert(request.id));
                assert!(
                    request.body["messages"].to_string().contains(text),
                    "worker sees the exact unfiltered new instruction before dispatching"
                );
                request.release.send(()).unwrap();
            }
            wait_for_votes(&store, 2).await?;
            assert!(!run.is_finished());
        }
        assert_eq!(control.add_agents(1).await?, vec!["agent-004"]);
        let mut awakened = HashSet::new();
        let mut joined = None;
        for _ in 0..3 {
            let request = events
                .recv()
                .await
                .context("membership notice did not wake worker")?;
            assert!(request.body["messages"]
                .to_string()
                .contains("Owner added agent-004"));
            if request.id == "agent-004" {
                assert!(joined.replace(request).is_none());
            } else {
                assert_ne!(request.id, "agent-003");
                assert!(awakened.insert(request.id));
                request.release.send(()).unwrap();
            }
        }
        assert_eq!(
            awakened.len(),
            2,
            "both already-voted workers must resume when membership changes"
        );
        wait_for_votes(&store, 2).await?;
        assert!(
            !run.is_finished(),
            "two held workers prevent premature dynamic quorum"
        );
        held.release.send(()).unwrap();
        joined
            .context("new member did not dispatch")?
            .release
            .send(())
            .unwrap();
        let mut final_voters = HashSet::new();
        let mut final_requests = Vec::new();
        for _ in 0..2 {
            let last = events
                .recv()
                .await
                .context("held worker completion vote absent")?;
            assert!(matches!(last.id.as_str(), "agent-003" | "agent-004"));
            assert!(final_voters.insert(last.id.clone()));
            assert!(last.body["messages"]
                .to_string()
                .contains("peer found remaining work"));
            final_requests.push(last);
        }
        for request in final_requests {
            request.release.send(()).unwrap();
        }
        let summary = run.await??;
        assert_eq!(summary.agents, 4);
        assert_eq!(summary.finished_agents, 4);
        server.abort();
        let _ = server.await;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}
