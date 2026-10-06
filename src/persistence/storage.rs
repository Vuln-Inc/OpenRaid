//! Durable global board and checkpoints behind a single bounded SQLite actor.
//!
//! Agents never own a connection or a blocking thread. The watch channel carries
//! only the latest durable cursor; readers retrieve every message from SQLite.

use std::{
    path::Path,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch};

const QUEUE_CAPACITY: usize = 1_024;
const MAX_PAGE_SIZE: usize = 10_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BoardMessage {
    pub seq: u64,
    pub sender: String,
    pub body: String,
    pub owner: bool,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Vote {
    pub agent_id: String,
    pub done: bool,
    pub reason: String,
    pub updated_at_ms: u64,
    /// Board cursor at the instant the vote was committed.
    pub board_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoreStats {
    pub board_messages: u64,
    pub checkpoints: u64,
    pub votes: u64,
}

/// Durable active roster. Removed identities are never allocated again by a
/// live membership change; the allocation high-water mark survives reopening.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Membership {
    pub revision: u64,
    pub agent_ids: Vec<String>,
}

type Operation = Box<dyn FnOnce(&mut Connection) + Send + 'static>;

/// Cheap cloneable handle shared by all Tokio workers.
#[derive(Clone)]
pub struct Store {
    commands: mpsc::Sender<Operation>,
    revision: watch::Sender<u64>,
}

impl Store {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let (commands, mut receiver) = mpsc::channel::<Operation>(QUEUE_CAPACITY);
        let (ready_tx, ready_rx) = oneshot::channel();
        let (revision, _) = watch::channel(0);
        let initial_revision = revision.clone();
        thread::Builder::new()
            .name("openraid-sqlite".into())
            .spawn(move || {
                let opened = (|| -> Result<Connection> {
                    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                        std::fs::create_dir_all(parent).with_context(|| {
                            format!("create database directory {}", parent.display())
                        })?;
                    }
                    let connection = Connection::open(&path)
                        .with_context(|| format!("open database {}", path.display()))?;
                    // A busy database is retried indefinitely, never abandoned on
                    // elapsed time. This sleeps only this one dedicated actor.
                    connection.busy_handler(Some(|attempt| {
                        let delay = 1_u64 << (attempt.max(0) as u32).min(7);
                        thread::sleep(Duration::from_millis(delay));
                        true
                    }))?;
                    connection.execute_batch(
                        "PRAGMA journal_mode = WAL;
                         PRAGMA synchronous = NORMAL;
                         PRAGMA foreign_keys = ON;
                         PRAGMA cache_size = -2048;
                         CREATE TABLE IF NOT EXISTS board (
                             seq INTEGER PRIMARY KEY AUTOINCREMENT,
                             sender TEXT NOT NULL,
                             body TEXT NOT NULL,
                             owner INTEGER NOT NULL CHECK(owner IN (0, 1)),
                             created_at_ms INTEGER NOT NULL
                         );
                         CREATE INDEX IF NOT EXISTS board_owner_seq ON board(seq) WHERE owner = 1;
                         CREATE TABLE IF NOT EXISTS checkpoints (
                             agent_id TEXT PRIMARY KEY,
                             state_json TEXT NOT NULL,
                             updated_at_ms INTEGER NOT NULL
                         );
                         CREATE TABLE IF NOT EXISTS votes (
                             agent_id TEXT PRIMARY KEY,
                             done INTEGER NOT NULL CHECK(done IN (0, 1)),
                             reason TEXT NOT NULL,
                             updated_at_ms INTEGER NOT NULL,
                              board_seq INTEGER NOT NULL
                         );
                          CREATE TABLE IF NOT EXISTS prompts (
                             seq INTEGER PRIMARY KEY, body TEXT NOT NULL,
                             created_at_ms INTEGER NOT NULL, snapshot TEXT
                          );
                          CREATE TABLE IF NOT EXISTS membership_state (
                              singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                              revision INTEGER NOT NULL,
                              next_id INTEGER NOT NULL
                          );
                          CREATE TABLE IF NOT EXISTS members (
                              agent_id TEXT PRIMARY KEY,
                              ordinal INTEGER NOT NULL UNIQUE,
                              active INTEGER NOT NULL CHECK(active IN (0, 1))
                          );
                          CREATE TABLE IF NOT EXISTS worker_slots (
                              agent_id TEXT PRIMARY KEY REFERENCES members(agent_id)
                          );
                           CREATE TABLE IF NOT EXISTS membership_phase (
                              singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                              draining INTEGER NOT NULL CHECK(draining IN (0, 1))
                           );
                           CREATE TABLE IF NOT EXISTS session_state (
                               singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                               paused INTEGER NOT NULL DEFAULT 0 CHECK(paused IN (0, 1))
                           );
                           INSERT OR IGNORE INTO session_state(singleton,paused) VALUES (1,0);
                         INSERT OR IGNORE INTO prompts(seq, body, created_at_ms)
                         SELECT seq, body, created_at_ms FROM board WHERE owner = 1 AND sender = 'owner';",
                    )?;
                    initial_revision.send_replace(latest_cursor(&connection)?);
                    Ok(connection)
                })();
                match opened {
                    Ok(mut connection) => {
                        if ready_tx.send(Ok(())).is_err() {
                            return;
                        }
                        while let Some(operation) = receiver.blocking_recv() {
                            operation(&mut connection);
                        }
                        // Every write was already committed. SQLite closes after
                        // draining queued operations when the final handle drops.
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                    }
                }
            })
            .context("start sqlite actor")?;
        ready_rx
            .await
            .context("sqlite actor stopped during initialization")??;
        Ok(Self { commands, revision })
    }

    async fn call<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.commands
            .send(Box::new(move |connection| {
                let result = operation(connection);
                // A cancelled reader must not cancel a submitted durable write.
                let _ = reply_tx.send(result);
            }))
            .await
            .map_err(|_| anyhow!("sqlite actor is closed"))?;
        reply_rx
            .await
            .context("sqlite actor stopped before responding")?
    }

    /// Subscribe to durable cursor changes, not message contents. Coalescing
    /// notifications never discards messages: consumers page from their cursor.
    pub fn subscribe_board(&self) -> watch::Receiver<u64> {
        self.revision.subscribe()
    }

    /// Establish the initial roster before workers start. A resumed session
    /// retains its durable roster, including earlier additions/removals.
    pub async fn initialize_membership(
        &self,
        initial_count: usize,
        resume: bool,
    ) -> Result<Membership> {
        ensure!(
            (1..=500).contains(&initial_count),
            "agent count must be between 1 and 500"
        );
        self.call(move |connection| {
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // New sessions own no workers from the previous process. Live
            // rounds never reinitialize or erase outstanding drain slots.
            transaction.execute("DELETE FROM worker_slots", [])?;
            transaction.execute(
                "INSERT INTO membership_phase(singleton,draining) VALUES (1,0)
                 ON CONFLICT(singleton) DO UPDATE SET draining=0", [],
            )?;
            let state = membership_from_connection(&transaction)?;
            if resume {
                if let Some(state) = state {
                    transaction.commit()?;
                    return Ok(state);
                }
            }
            transaction.execute(
                "INSERT INTO membership_state(singleton,revision,next_id) VALUES (1,0,?1)
                 ON CONFLICT(singleton) DO UPDATE SET revision=revision+1, next_id=MAX(next_id,excluded.next_id)",
                [initial_count as u64 + 1],
            )?;
            transaction.execute("UPDATE members SET active=0", [])?;
            for number in 1..=initial_count {
                transaction.execute(
                    "INSERT INTO members(agent_id,ordinal,active) VALUES (?1,?2,1)
                     ON CONFLICT(agent_id) DO UPDATE SET active=1",
                    params![format!("agent-{number:03}"), number as u64],
                )?;
            }
            if !resume {
                transaction.execute("DELETE FROM votes", [])?;
            }
            let membership = membership_from_connection(&transaction)?.context("membership missing")?;
            transaction.commit()?;
            Ok(membership)
        }).await
    }

    pub async fn membership(&self) -> Result<Membership> {
        self.call(|connection| {
            // The revision and rows must share one WAL read snapshot even
            // when another process commits a mutation between the two queries.
            let transaction = connection.transaction()?;
            let membership = membership_from_connection(&transaction)?
                .context("membership has not been initialized")?;
            transaction.commit()?;
            Ok(membership)
        })
        .await
    }

    /// Reserve the worker before spawning it. A concurrent withdrawal can win
    /// the transaction, in which case there is no task to start or drain.
    pub async fn mark_worker_started(&self, agent_id: &str) -> Result<bool> {
        let agent_id = agent_id.to_owned();
        self.call(move |connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let active: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM members WHERE agent_id=?1 AND active=1)
                 AND NOT COALESCE((SELECT draining FROM membership_phase WHERE singleton=1),0)",
                [&agent_id],
                |row| row.get(0),
            )?;
            if !active {
                return Ok(false);
            }
            transaction.execute(
                "INSERT OR IGNORE INTO worker_slots(agent_id) VALUES (?1)",
                [&agent_id],
            )?;
            transaction.commit()?;
            Ok(true)
        })
        .await
    }

    /// Release only after the worker actually exited, including a panic.
    /// Withdrawn IDs reserve capacity until their admitted operations drain.
    pub async fn mark_worker_finished(&self, agent_id: &str) -> Result<()> {
        let agent_id = agent_id.to_owned();
        self.call(move |connection| {
            connection.execute("DELETE FROM worker_slots WHERE agent_id=?1", [agent_id])?;
            Ok(())
        })
        .await
    }

    /// Reopen persistent-session controls after every admitted worker has
    /// joined. This phase is durable, so an independent handle cannot bypass
    /// a committed drain with its own untriggered process-local watch.
    pub async fn finish_round(&self) -> Result<()> {
        self.set_membership_phase(false).await
    }

    /// Final idle detach closes a previously reopened persistent session.
    /// A subsequent intentional Harness initialization can reopen it.
    pub async fn close_session(&self) -> Result<()> {
        self.set_membership_phase(true).await
    }

    /// Commit an operator drain before publishing the shutdown signal. The
    /// durable phase prevents independent handles from admitting more workers.
    pub async fn drain_round(
        &self,
        body: String,
        publish: impl FnOnce(u64) + Send + 'static,
    ) -> Result<u64> {
        let revision = self.revision.clone();
        self.call(move |connection| {
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute("UPDATE membership_phase SET draining=1 WHERE singleton=1", [])?;
            if body.starts_with("Owner stopped current work;") {
                transaction.execute("UPDATE session_state SET paused=0 WHERE singleton=1", [])?;
            }
            transaction.execute("DELETE FROM votes", [])?;
            transaction.execute(
                "INSERT INTO board(sender,body,owner,created_at_ms) VALUES ('owner-control',?1,1,?2)",
                params![body, timestamp_ms()],
            )?;
            let seq = latest_cursor(&transaction)?;
            transaction.commit()?;
            publish(seq);
            revision.send_replace(seq);
            Ok(seq)
        }).await
    }

    async fn set_membership_phase(&self, draining: bool) -> Result<()> {
        self.call(move |connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let outstanding: usize =
                transaction.query_row("SELECT COUNT(*) FROM worker_slots", [], |row| row.get(0))?;
            ensure!(outstanding == 0, "workers are still draining");
            transaction.execute(
                "UPDATE membership_phase SET draining=?1 WHERE singleton=1",
                [draining],
            )?;
            transaction.commit()?;
            Ok(())
        })
        .await
    }

    /// Join notices, roster changes, vote revocation and publication are one
    /// actor operation serialized with consensus. Cancellation of the caller
    /// after admission cannot discard the durable change or publication.
    pub async fn add_members(
        &self,
        count: usize,
        shutdown: watch::Receiver<bool>,
        publish: impl FnOnce(Membership) + Send + 'static,
    ) -> Result<Membership> {
        ensure!(
            (1..=500).contains(&count),
            "add count must be between 1 and 500"
        );
        self.change_membership(Some(count), Vec::new(), shutdown, publish)
            .await
    }

    /// Withdrawal prevents new quorum participation immediately. The runtime
    /// drains the removed worker's already admitted request/tool operations.
    pub async fn remove_members(
        &self,
        agent_ids: &[String],
        shutdown: watch::Receiver<bool>,
        publish: impl FnOnce(Membership) + Send + 'static,
    ) -> Result<Membership> {
        ensure!(!agent_ids.is_empty(), "select at least one agent to remove");
        let unique: std::collections::HashSet<_> = agent_ids.iter().collect();
        ensure!(
            unique.len() == agent_ids.len(),
            "duplicate removal identity"
        );
        self.change_membership(None, agent_ids.to_vec(), shutdown, publish)
            .await
    }

    async fn change_membership(
        &self,
        add_count: Option<usize>,
        removed: Vec<String>,
        shutdown: watch::Receiver<bool>,
        publish: impl FnOnce(Membership) + Send + 'static,
    ) -> Result<Membership> {
        let revision = self.revision.clone();
        self.call(move |connection| {
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            ensure!(!*shutdown.borrow(), "the round is draining; retry after it finishes");
            let draining: bool = transaction.query_row(
                "SELECT COALESCE((SELECT draining FROM membership_phase WHERE singleton=1),0)",
                [], |row| row.get(0),
            )?;
            ensure!(!draining, "the round is draining; retry after it finishes");
            let current = membership_from_connection(&transaction)?.context("membership has not been initialized")?;
            let mut notices = Vec::new();
            if let Some(count) = add_count {
                let draining: usize = transaction.query_row(
                    "SELECT COUNT(*) FROM worker_slots JOIN members USING(agent_id) WHERE members.active=0",
                    [], |row| row.get(0),
                )?;
                ensure!(current.agent_ids.len() + draining + count <= 500,
                    "active plus draining agent count cannot exceed 500; retry after removed workers finish");
                let next_id: u64 = transaction.query_row(
                    "SELECT next_id FROM membership_state WHERE singleton=1", [], |row| row.get(0),
                )?;
                let following = next_id.checked_add(count as u64).filter(|next| *next <= i64::MAX as u64)
                    .context("agent identity allocation exhausted")?;
                for number in next_id..following {
                    let id = format!("agent-{number:03}");
                    transaction.execute("INSERT INTO members(agent_id,ordinal,active) VALUES (?1,?2,1)", params![id, number])?;
                    notices.push(format!("Owner added {id}. Join ongoing work, read the full shared global board and collaborate with the current active roster."));
                }
                transaction.execute("UPDATE membership_state SET next_id=?1 WHERE singleton=1", [following])?;
            } else {
                ensure!(removed.len() < current.agent_ids.len(), "at least one active agent must remain");
                for id in &removed {
                    ensure!(current.agent_ids.contains(id), "{id} is not an active agent");
                }
                for id in &removed {
                    transaction.execute("UPDATE members SET active=0 WHERE agent_id=?1", [id])?;
                    notices.push(format!("Owner removed {id}. Stop admitting new work and drain all in-flight requests/tools gracefully; remaining agents continue collaborating through the same global board."));
                }
            }
            transaction.execute("UPDATE membership_state SET revision=revision+1 WHERE singleton=1", [])?;
            transaction.execute("DELETE FROM votes", [])?;
            let started: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM prompts)", [], |row| row.get(0))?;
            for body in notices.into_iter().filter(|_| started) {
                transaction.execute(
                    "INSERT INTO board(sender,body,owner,created_at_ms) VALUES ('owner-control',?1,1,?2)",
                    params![body, timestamp_ms()],
                )?;
            }
            let cursor = latest_cursor(&transaction)?;
            let membership = membership_from_connection(&transaction)?.context("membership missing")?;
            transaction.commit()?;
            publish(membership.clone());
            revision.send_replace(cursor);
            Ok(membership)
        }).await
    }

    pub async fn append(
        &self,
        sender: impl Into<String>,
        body: impl Into<String>,
        owner: bool,
    ) -> Result<BoardMessage> {
        let sender = sender.into();
        let body = body.into();
        ensure!(!sender.trim().is_empty(), "board sender must not be empty");
        let revision = self.revision.clone();
        self.call(move |connection| {
            let created_at_ms = timestamp_ms();
            let transaction = connection.transaction()?;
            transaction.execute(
                "INSERT INTO board(sender, body, owner, created_at_ms) VALUES (?1, ?2, ?3, ?4)",
                params![sender, body, owner, created_at_ms],
            )?;
            let seq = transaction.last_insert_rowid() as u64;
            if owner {
                // New owner instructions and vote revocation are one atomic
                // change, so the supervisor cannot observe stale consensus.
                transaction.execute("DELETE FROM votes", [])?;
                if sender == "owner" {
                    transaction.execute(
                        "INSERT INTO prompts(seq,body,created_at_ms) VALUES (?1,?2,?3)",
                        params![seq, body, created_at_ms],
                    )?;
                }
            }
            transaction.commit()?;
            revision.send_replace(seq);
            Ok(BoardMessage {
                seq,
                sender,
                body,
                owner,
                created_at_ms,
            })
        })
        .await
    }

    /// Read the one global, ordered board. Only cursor and page size selection
    /// Commit an operator change before notifying workers, serialized with the
    /// consensus gate. A finished round cannot accept a late model switch.
    pub async fn owner_action(
        &self,
        sender: &str,
        body: String,
        shutdown: watch::Receiver<bool>,
        publish: impl FnOnce() + Send + 'static,
    ) -> Result<BoardMessage> {
        self.owner_action_with_pause(sender, body, shutdown, None, publish)
            .await
    }

    /// Commit pause state and its audit notice together before publication.
    pub async fn owner_action_with_pause(
        &self,
        sender: &str,
        body: String,
        shutdown: watch::Receiver<bool>,
        paused: Option<bool>,
        publish: impl FnOnce() + Send + 'static,
    ) -> Result<BoardMessage> {
        let sender = sender.to_owned();
        let revision = self.revision.clone();
        self.call(move |connection| {
            ensure!(!*shutdown.borrow(), "the session has finished");
            let created_at_ms = timestamp_ms();
            let transaction = connection.transaction()?;
            if let Some(paused) = paused {
                transaction.execute(
                    "UPDATE session_state SET paused=?1 WHERE singleton=1",
                    [paused],
                )?;
            }
            transaction.execute(
                "INSERT INTO board(sender, body, owner, created_at_ms) VALUES (?1, ?2, 1, ?3)",
                params![sender, body, created_at_ms],
            )?;
            let seq = transaction.last_insert_rowid() as u64;
            transaction.execute("DELETE FROM votes", [])?;
            if sender == "owner" {
                transaction.execute(
                    "INSERT INTO prompts(seq,body,created_at_ms) VALUES (?1,?2,?3)",
                    params![seq, body, created_at_ms],
                )?;
            }
            transaction.commit()?;
            publish();
            revision.send_replace(seq);
            Ok(BoardMessage {
                seq,
                sender,
                body,
                owner: true,
                created_at_ms,
            })
        })
        .await
    }

    pub async fn paused(&self) -> Result<bool> {
        self.call(|connection| {
            Ok(connection.query_row(
                "SELECT paused FROM session_state WHERE singleton=1",
                [],
                |row| row.get(0),
            )?)
        })
        .await
    }

    pub async fn set_paused(&self, paused: bool) -> Result<()> {
        self.call(move |connection| {
            connection.execute(
                "UPDATE session_state SET paused=?1 WHERE singleton=1",
                [paused],
            )?;
            Ok(())
        })
        .await
    }

    /// The latest task remains visible even after completion or an operator stop.
    pub async fn latest_task_prompt(&self) -> Result<Option<BoardMessage>> {
        self.call(|connection| {
            Ok(connection
                .query_row(
                    "SELECT seq,body,created_at_ms FROM prompts ORDER BY seq DESC LIMIT 1",
                    [],
                    |row| {
                        Ok(BoardMessage {
                            seq: row.get(0)?,
                            body: row.get(1)?,
                            created_at_ms: row.get(2)?,
                            sender: "owner".into(),
                            owner: true,
                        })
                    },
                )
                .optional()?)
        })
        .await
    }

    pub async fn prompts(&self) -> Result<Vec<BoardMessage>> {
        self.call(|connection| {
            let mut query = connection
                .prepare("SELECT seq,body,created_at_ms FROM prompts ORDER BY seq DESC")?;
            let prompts = query
                .query_map([], |row| {
                    Ok(BoardMessage {
                        seq: row.get(0)?,
                        body: row.get(1)?,
                        created_at_ms: row.get(2)?,
                        sender: "owner".into(),
                        owner: true,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(prompts)
        })
        .await
    }

    /// Resume tasks outside the last fully drained consensus gate. A prompt
    /// queued after that gate is not covered by its later drain marker. Merely
    /// committing consensus is insufficient: interrupted draining still needs
    /// recovery. Control notices are audit entries, never task prompts.
    pub async fn unfinished_prompt(&self) -> Result<Option<BoardMessage>> {
        self.call(|connection| {
            Ok(connection
                .query_row(
                    "SELECT seq,sender,body,owner,created_at_ms FROM board
                 WHERE owner=1 AND sender='owner' AND seq > MAX(COALESCE((
                     SELECT COALESCE((
                         SELECT MAX(gate.seq) FROM board gate
                         WHERE gate.sender='harness'
                           AND gate.body='75% completion consensus reached; draining workers'
                           AND gate.seq < drained.seq
                           AND gate.seq > COALESCE((
                               SELECT MAX(previous.seq) FROM board previous
                               WHERE previous.sender='harness'
                                 AND previous.body='all workers drained; swarm complete'
                                 AND previous.seq < drained.seq
                           ),0)
                     ), drained.seq) FROM board drained
                     WHERE drained.sender='harness' AND drained.body='all workers drained; swarm complete'
                     ORDER BY drained.seq DESC LIMIT 1
                  ),0), COALESCE((
                      SELECT MAX(stopped.seq) FROM board stopped
                      WHERE stopped.sender='owner-control'
                        AND stopped.body IN ('Owner stopped current work; draining in-flight operations.',
                                             'Owner stopped current work; cancelling in-flight operations immediately.')
                        AND EXISTS(SELECT 1 FROM board drained
                            WHERE drained.sender='harness'
                              AND drained.body='all workers drained; work stopped'
                              AND drained.seq > stopped.seq
                              AND NOT EXISTS(SELECT 1 FROM board later
                                  WHERE later.sender='owner-control'
                                    AND later.body IN ('Owner stopped current work; draining in-flight operations.',
                                                        'Owner stopped current work; cancelling in-flight operations immediately.',
                                                        'Owner closed session; draining in-flight operations.')
                                    AND later.seq > stopped.seq AND later.seq < drained.seq))
                  ),0)) ORDER BY seq DESC LIMIT 1",
                    [],
                    |row| {
                        Ok(BoardMessage {
                            seq: row.get(0)?,
                            sender: row.get(1)?,
                            body: row.get(2)?,
                            owner: row.get(3)?,
                            created_at_ms: row.get(4)?,
                        })
                    },
                )
                .optional()?)
        })
        .await
    }
    pub async fn save_snapshot(&self, seq: u64, tree: String) -> Result<()> {
        self.call(move |connection| {
            connection.execute(
                "UPDATE prompts SET snapshot=?1 WHERE seq=?2",
                params![tree, seq],
            )?;
            Ok(())
        })
        .await
    }
    pub async fn prompt_snapshot(&self, seq: u64) -> Result<Option<String>> {
        self.call(move |connection| {
            Ok(connection
                .query_row("SELECT snapshot FROM prompts WHERE seq=?1", [seq], |row| {
                    row.get::<_, Option<String>>(0)
                })
                .optional()?
                .flatten())
        })
        .await
    }

    /// Read the one global, ordered board. Only cursor and page size selection
    /// are supported; there are intentionally no sender/topic/channel filters.
    pub async fn read_board(&self, after: u64, limit: usize) -> Result<Vec<BoardMessage>> {
        ensure!(
            after <= i64::MAX as u64,
            "board cursor exceeds SQLite integer range"
        );
        ensure!(
            limit > 0 && limit <= MAX_PAGE_SIZE,
            "board page size must be between 1 and {MAX_PAGE_SIZE}"
        );
        self.call(move |connection| {
            let mut statement = connection.prepare_cached(
                "SELECT seq, sender, body, owner, created_at_ms FROM board
                 WHERE seq > ?1 ORDER BY seq ASC LIMIT ?2",
            )?;
            let messages = statement
                .query_map(params![after, limit as u64], |row| {
                    Ok(BoardMessage {
                        seq: row.get(0)?,
                        sender: row.get(1)?,
                        body: row.get(2)?,
                        owner: row.get(3)?,
                        created_at_ms: row.get(4)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(messages)
        })
        .await
    }

    pub async fn latest_seq(&self) -> Result<u64> {
        self.call(|connection| latest_cursor(connection)).await
    }

    /// Owner instructions and controls invalidate votes; peer chatter does not.
    pub async fn latest_owner_seq(&self) -> Result<u64> {
        self.call(|connection| {
            Ok(connection.query_row(
                "SELECT COALESCE(MAX(seq), 0) FROM board WHERE owner = 1",
                [],
                |row| row.get(0),
            )?)
        })
        .await
    }

    /// An atomic, unpaginated snapshot for operator export.
    pub async fn export_board(&self) -> Result<Vec<BoardMessage>> {
        self.call(|connection| {
            let mut query = connection
                .prepare("SELECT seq,sender,body,owner,created_at_ms FROM board ORDER BY seq")?;
            let rows = query
                .query_map([], |row| {
                    Ok(BoardMessage {
                        seq: row.get(0)?,
                        sender: row.get(1)?,
                        body: row.get(2)?,
                        owner: row.get(3)?,
                        created_at_ms: row.get(4)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    /// Only an idle operator control may call this. Retain AUTOINCREMENT state
    /// so existing runtime cursors still see subsequent prompts.
    pub(crate) async fn clear_board(&self) -> Result<()> {
        let revision = self.revision.clone();
        self.call(move |connection| {
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let workers: usize = transaction.query_row("SELECT COUNT(*) FROM worker_slots", [], |row| row.get(0))?;
            ensure!(workers == 0, "wait for all workers to drain before clearing the board");
            transaction.execute_batch("DELETE FROM votes; DELETE FROM checkpoints; DELETE FROM prompts; DELETE FROM board;")?;
            transaction.commit()?;
            revision.send_replace(0);
            Ok(())
        }).await
    }

    pub async fn has_started(&self) -> Result<bool> {
        self.call(|connection| {
            Ok(connection
                .query_row("SELECT EXISTS(SELECT 1 FROM prompts)", [], |row| row.get(0))?)
        })
        .await
    }

    /// One supervisor can poll cross-process changes and wake all local agents.
    /// Unchanged cursors do not notify receivers or create extra worker work.
    pub async fn refresh_board(&self) -> Result<u64> {
        let revision = self.revision.clone();
        self.call(move |connection| {
            let cursor = latest_cursor(connection)?;
            revision.send_if_modified(|known| {
                if *known == cursor {
                    return false;
                }
                *known = cursor;
                true
            });
            Ok(cursor)
        })
        .await
    }

    pub async fn save_checkpoint(&self, agent_id: &str, state: &Value) -> Result<()> {
        self.save_checkpoint_serializable(agent_id, state).await
    }

    /// Serialize a borrowed checkpoint directly, without cloning its message
    /// tree into a temporary JSON Value while waiting for actor admission.
    pub async fn save_checkpoint_serializable<T: Serialize + Sync + ?Sized>(
        &self,
        agent_id: &str,
        state: &T,
    ) -> Result<()> {
        let agent_id = agent_id.to_owned();
        let state_json = serde_json::to_string(state)?;
        self.call(move |connection| {
            connection.execute(
                "INSERT INTO checkpoints(agent_id, state_json, updated_at_ms) VALUES (?1, ?2, ?3)
                 ON CONFLICT(agent_id) DO UPDATE SET state_json=excluded.state_json,
                 updated_at_ms=excluded.updated_at_ms",
                params![agent_id, state_json, timestamp_ms()],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn load_checkpoint(&self, agent_id: &str) -> Result<Option<Value>> {
        let agent_id = agent_id.to_owned();
        self.call(move |connection| {
            let state: Option<String> = connection
                .query_row(
                    "SELECT state_json FROM checkpoints WHERE agent_id = ?1",
                    [agent_id],
                    |row| row.get(0),
                )
                .optional()?;
            state
                .map(|json| serde_json::from_str(&json).context("decode agent checkpoint"))
                .transpose()
        })
        .await
    }

    pub async fn set_vote(&self, agent_id: &str, done: bool, reason: &str) -> Result<Vote> {
        ensure!(
            !agent_id.trim().is_empty(),
            "vote agent id must not be empty"
        );
        let agent_id = agent_id.to_owned();
        let reason = reason.to_owned();
        self.call(move |connection| write_vote(connection, agent_id, done, reason, None))
            .await
    }

    /// Positive votes may require the exact last delivered board cursor. Taking
    /// the write transaction before checking prevents a new owner instruction
    /// from slipping between the freshness check and vote insertion.
    pub async fn set_vote_at(
        &self,
        agent_id: &str,
        done: bool,
        reason: &str,
        expected_seq: u64,
    ) -> Result<Vote> {
        ensure!(
            !agent_id.trim().is_empty(),
            "vote agent id must not be empty"
        );
        let agent_id = agent_id.to_owned();
        let reason = reason.to_owned();
        self.call(move |connection| {
            write_vote(connection, agent_id, done, reason, Some(expected_seq))
        })
        .await
    }

    pub async fn vote(&self, agent_id: &str) -> Result<Option<Vote>> {
        let agent_id = agent_id.to_owned();
        self.call(move |connection| {
            Ok(connection.query_row(
                "SELECT agent_id, done, reason, updated_at_ms, board_seq FROM votes WHERE agent_id = ?1",
                [agent_id], vote_from_row,
            ).optional()?)
        }).await
    }

    /// Atomically gate completion and publish its marker. Only known voters
    /// whose evidence covers the latest owner instruction count. An external owner
    /// append either precedes this transaction and revokes quorum, or follows
    /// the committed gate; it cannot interleave the check and marker insertion.
    pub async fn try_commit_consensus(
        &self,
        agent_ids: &[String],
        expected_owner_seq: u64,
        threshold: usize,
    ) -> Result<Option<usize>> {
        self.commit_consensus(agent_ids, expected_owner_seq, None, threshold, None)
            .await
    }

    /// Bind the atomic gate to the owner revision observed during the grace
    /// period. Peer posts after the observed board cursor do not reset quorum;
    /// a newer owner instruction still rejects the gate atomically.
    pub async fn try_commit_consensus_at(
        &self,
        agent_ids: &[String],
        expected_owner_seq: u64,
        expected_board_seq: u64,
        threshold: usize,
    ) -> Result<Option<usize>> {
        self.commit_consensus(
            agent_ids,
            expected_owner_seq,
            Some(expected_board_seq),
            threshold,
            None,
        )
        .await
    }

    /// Publish the committed control signal before notifying board readers.
    /// Voted workers awakened by the durable marker therefore already observe
    /// shutdown and cannot admit a new request while the supervisor awaits us.
    pub async fn try_commit_consensus_at_with_shutdown(
        &self,
        agent_ids: &[String],
        expected_owner_seq: u64,
        expected_board_seq: u64,
        threshold: usize,
        shutdown: watch::Sender<bool>,
    ) -> Result<Option<usize>> {
        self.commit_consensus(
            agent_ids,
            expected_owner_seq,
            Some(expected_board_seq),
            threshold,
            Some(shutdown),
        )
        .await
    }

    async fn commit_consensus(
        &self,
        agent_ids: &[String],
        expected_owner_seq: u64,
        expected_board_seq: Option<u64>,
        threshold: usize,
        shutdown: Option<watch::Sender<bool>>,
    ) -> Result<Option<usize>> {
        ensure!(
            threshold > 0 && threshold <= agent_ids.len(),
            "invalid consensus threshold"
        );
        let agent_ids: std::collections::HashSet<String> = agent_ids.iter().cloned().collect();
        let revision = self.revision.clone();
        self.call(move |connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if let Some(membership) = membership_from_connection(&transaction)? {
                let draining: bool = transaction.query_row(
                    "SELECT draining FROM membership_phase WHERE singleton=1",
                    [],
                    |row| row.get(0),
                )?;
                if draining {
                    return Ok(None);
                }
                let active: std::collections::HashSet<_> =
                    membership.agent_ids.into_iter().collect();
                if active != agent_ids || threshold != active.len().saturating_mul(3).div_ceil(4) {
                    return Ok(None);
                }
            }
            let owner_seq: u64 = transaction.query_row(
                "SELECT COALESCE(MAX(seq), 0) FROM board WHERE owner = 1",
                [],
                |row| row.get(0),
            )?;
            if owner_seq != expected_owner_seq {
                return Ok(None);
            }
            let board_seq = latest_cursor(&transaction)?;
            if expected_board_seq
                .is_some_and(|expected| expected < owner_seq || expected > board_seq)
            {
                return Ok(None);
            }
            let done = {
                let mut statement = transaction.prepare_cached(
                    "SELECT agent_id FROM votes WHERE done = 1 AND board_seq >= ?1",
                )?;
                let voters = statement.query_map([owner_seq], |row| row.get::<_, String>(0))?;
                let mut done = 0;
                for voter in voters {
                    if agent_ids.contains(&voter?) {
                        done += 1;
                    }
                }
                done
            };
            if done < threshold {
                return Ok(None);
            }
            transaction.execute(
                "INSERT INTO board(sender, body, owner, created_at_ms) VALUES (?1, ?2, 0, ?3)",
                params![
                    "harness",
                    "75% completion consensus reached; draining workers",
                    timestamp_ms()
                ],
            )?;
            let cursor = transaction.last_insert_rowid() as u64;
            transaction.execute(
                "UPDATE membership_phase SET draining=1 WHERE singleton=1",
                [],
            )?;
            transaction.commit()?;
            if let Some(shutdown) = shutdown {
                shutdown.send_replace(true);
            }
            revision.send_replace(cursor);
            Ok(Some(done))
        })
        .await
    }

    pub async fn votes(&self) -> Result<Vec<Vote>> {
        self.call(|connection| {
            let mut statement = connection.prepare_cached(
                "SELECT agent_id, done, reason, updated_at_ms, board_seq FROM votes ORDER BY agent_id",
            )?;
            let votes = statement.query_map([], vote_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(votes)
        }).await
    }

    pub async fn reset_votes(&self) -> Result<()> {
        self.call(|connection| {
            connection.execute("DELETE FROM votes", [])?;
            Ok(())
        })
        .await
    }

    pub async fn stats(&self) -> Result<StoreStats> {
        self.call(|connection| {
            Ok(StoreStats {
                board_messages: connection
                    .query_row("SELECT COUNT(*) FROM board", [], |r| r.get(0))?,
                checkpoints: connection
                    .query_row("SELECT COUNT(*) FROM checkpoints", [], |r| r.get(0))?,
                votes: connection.query_row("SELECT COUNT(*) FROM votes", [], |r| r.get(0))?,
            })
        })
        .await
    }

    /// Durability barrier and passive WAL maintenance, with no deadline.
    pub async fn flush(&self) -> Result<()> {
        self.call(|connection| {
            connection.execute_batch("PRAGMA wal_checkpoint(PASSIVE)")?;
            Ok(())
        })
        .await
    }
}

fn write_vote(
    connection: &mut Connection,
    agent_id: String,
    done: bool,
    reason: String,
    expected_seq: Option<u64>,
) -> Result<Vote> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let membership_initialized: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM membership_state WHERE singleton=1)",
        [],
        |row| row.get(0),
    )?;
    if membership_initialized {
        let active: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM members WHERE agent_id=?1 AND active=1)",
            [&agent_id],
            |row| row.get(0),
        )?;
        ensure!(active, "{agent_id} is not an active quorum member");
    }
    let board_seq = latest_cursor(&transaction)?;
    if let Some(expected) = expected_seq.filter(|_| done) {
        ensure!(board_seq == expected,
            "board changed before vote: delivered cursor {expected}, latest {board_seq}; read new entries first");
    }
    let updated_at_ms = timestamp_ms();
    transaction.execute(
        "INSERT INTO votes(agent_id, done, reason, updated_at_ms, board_seq)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(agent_id) DO UPDATE SET done=excluded.done, reason=excluded.reason,
         updated_at_ms=excluded.updated_at_ms, board_seq=excluded.board_seq",
        params![agent_id, done, reason, updated_at_ms, board_seq],
    )?;
    transaction.commit()?;
    Ok(Vote {
        agent_id,
        done,
        reason,
        updated_at_ms,
        board_seq,
    })
}

fn vote_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Vote> {
    Ok(Vote {
        agent_id: row.get(0)?,
        done: row.get(1)?,
        reason: row.get(2)?,
        updated_at_ms: row.get(3)?,
        board_seq: row.get(4)?,
    })
}

fn membership_from_connection(connection: &Connection) -> Result<Option<Membership>> {
    let revision: Option<u64> = connection
        .query_row(
            "SELECT revision FROM membership_state WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(revision) = revision else {
        return Ok(None);
    };
    let mut statement =
        connection.prepare("SELECT agent_id FROM members WHERE active=1 ORDER BY ordinal")?;
    let agent_ids = statement
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(Some(Membership {
        revision,
        agent_ids,
    }))
}

fn latest_cursor(connection: &Connection) -> Result<u64> {
    Ok(
        connection.query_row("SELECT COALESCE(MAX(seq), 0) FROM board", [], |row| {
            row.get(0)
        })?,
    )
}

fn timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn external_membership_writer_cannot_tear_revision_and_roster_read_snapshot() -> Result<()>
    {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("coherent-roster.sqlite");
        let reader = Store::open(&path).await?;
        reader.initialize_membership(1, false).await?;
        let objective = reader
            .append("owner", "exercise concurrent membership", true)
            .await?;
        let writer = Store::open(&path).await?;
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer_done = done.clone();
        let writes = async move {
            let (shutdown, _) = watch::channel(false);
            for _ in 0..1_000 {
                let joined = writer.add_members(1, shutdown.subscribe(), |_| {}).await?;
                let id = joined
                    .agent_ids
                    .last()
                    .context("new member missing")?
                    .clone();
                writer
                    .remove_members(&[id], shutdown.subscribe(), |_| {})
                    .await?;
            }
            writer_done.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok::<_, anyhow::Error>(())
        };
        let reads = async {
            let mut reads = 0;
            let mut concurrent_reads = 0;
            while !done.load(std::sync::atomic::Ordering::SeqCst) || reads < 2_000 {
                let writing = !done.load(std::sync::atomic::Ordering::SeqCst);
                let roster = reader.membership().await?;
                assert_eq!(
                    roster.agent_ids.len(),
                    1 + (roster.revision % 2) as usize,
                    "each odd revision joins one member; each even revision withdraws it"
                );
                assert_eq!(roster.agent_ids[0], "agent-001");
                concurrent_reads += usize::from(writing);
                reads += 1;
            }
            assert!(
                concurrent_reads > 0,
                "must exercise snapshots while the independent writer is active"
            );
            Ok::<_, anyhow::Error>(())
        };
        tokio::try_join!(writes, reads)?;
        assert_eq!(reader.membership().await?.revision, 2_000);
        assert_eq!(reader.read_board(objective.seq, 10_000).await?.len(), 2_000);
        Ok(())
    }

    #[tokio::test]
    async fn unfinished_prompt_survives_reopen_and_only_full_drain_marks_task_complete(
    ) -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("interrupted.sqlite");
        let store = Store::open(&path).await?;
        assert_eq!(store.unfinished_prompt().await?, None);
        store
            .append("owner", "earlier completed task", true)
            .await?;
        store
            .append("harness", "all workers drained; swarm complete", false)
            .await?;
        store
            .append("owner-control", "Owner selected another model", true)
            .await?;
        assert_eq!(store.unfinished_prompt().await?, None);
        store.append("owner", "interrupted task", true).await?;
        let latest = store
            .append("owner", "newest instruction in that task", true)
            .await?;
        store
            .append(
                "harness",
                "75% completion consensus reached; draining workers",
                false,
            )
            .await?;
        store
            .append("owner-control", "Owner added agent-003", true)
            .await?;
        assert_eq!(store.unfinished_prompt().await?, Some(latest.clone()));
        store.flush().await?;
        let reopened = Store::open(&path).await?;
        assert_eq!(reopened.unfinished_prompt().await?, Some(latest));
        reopened
            .append("harness", "all workers drained; swarm complete", false)
            .await?;
        assert_eq!(
            store.unfinished_prompt().await?,
            None,
            "completed history must stay audit-only on restart"
        );
        store
            .append("owner", "work reaching consensus", true)
            .await?;
        store
            .append(
                "harness",
                "75% completion consensus reached; draining workers",
                false,
            )
            .await?;
        let queued = store
            .append("owner", "new external task queued during drain", true)
            .await?;
        store
            .append("harness", "all workers drained; swarm complete", false)
            .await?;
        assert_eq!(
            reopened.unfinished_prompt().await?,
            Some(queued),
            "a post-consensus prompt is not covered by the old round's later drain marker"
        );
        store
            .append(
                "harness",
                "75% completion consensus reached; draining workers",
                false,
            )
            .await?;
        store
            .append("harness", "all workers drained; swarm complete", false)
            .await?;
        assert_eq!(reopened.unfinished_prompt().await?, None);
        store
            .append("owner", "completed legacy marker-only round", true)
            .await?;
        store
            .append("harness", "all workers drained; swarm complete", false)
            .await?;
        assert_eq!(
            reopened.unfinished_prompt().await?,
            None,
            "a legacy completed round cannot inherit an older round's consensus gate"
        );
        Ok(())
    }

    #[tokio::test]
    async fn operator_stop_requires_drain_and_preserves_later_queued_prompts() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("stopped.sqlite3");
        let store = Store::open(&path).await?;
        store.initialize_membership(1, false).await?;
        let first = store.append("owner", "task stopped by owner", true).await?;
        store
            .drain_round(
                "Owner stopped current work; cancelling in-flight operations immediately.".into(),
                |_| {},
            )
            .await?;
        assert_eq!(
            store.unfinished_prompt().await?,
            Some(first),
            "interrupted draining is still recoverable"
        );
        let external = Store::open(&path).await?;
        assert!(
            !external.mark_worker_started("agent-001").await?,
            "durable stop prevents independent worker admission"
        );
        let later = external
            .append("owner", "new task queued after stop", true)
            .await?;
        store
            .append("harness", "all workers drained; work stopped", false)
            .await?;
        assert_eq!(
            store.unfinished_prompt().await?,
            Some(later.clone()),
            "old drain cannot discard a newly queued task"
        );
        store.finish_round().await?;
        store
            .drain_round(
                "Owner closed session; draining in-flight operations.".into(),
                |_| {},
            )
            .await?;
        store
            .append("harness", "all workers drained; session detached", false)
            .await?;
        assert_eq!(
            external.unfinished_prompt().await?,
            Some(later),
            "detach preserves unfinished work for explicit resume"
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn parallel_membership_changes_are_durable_unique_and_visible_on_global_board(
    ) -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("membership.sqlite");
        let store = Store::open(&path).await?;
        let initial = store.initialize_membership(2, false).await?;
        let objective = store
            .append("owner", "exercise parallel membership", true)
            .await?;
        let external = Store::open(&path).await?;
        let (shutdown, _) = watch::channel(false);
        store
            .set_vote("agent-001", true, "old roster complete")
            .await?;
        let mut joins = tokio::task::JoinSet::new();
        for index in 0..12 {
            let writer = if index % 2 == 0 {
                store.clone()
            } else {
                external.clone()
            };
            let shutdown = shutdown.subscribe();
            joins.spawn(async move { writer.add_members(1, shutdown, |_| {}).await });
        }
        while let Some(join) = joins.join_next().await {
            join??;
        }
        let joined = store.membership().await?;
        assert_eq!(joined.revision, initial.revision + 12);
        assert_eq!(joined.agent_ids.len(), 14);
        assert_eq!(joined.agent_ids.last().unwrap(), "agent-014");
        assert!(store.votes().await?.is_empty());
        let notices = external.read_board(objective.seq, 100).await?;
        assert_eq!(notices.len(), 12);
        assert!(notices
            .iter()
            .all(|notice| notice.owner && notice.sender == "owner-control"));
        assert!(
            store.prompts().await?.len() == 1,
            "membership notices do not create additional user tasks"
        );

        let mut removals = tokio::task::JoinSet::new();
        for id in joined.agent_ids.iter().skip(2).cloned() {
            let writer = external.clone();
            let shutdown = shutdown.subscribe();
            removals.spawn(async move { writer.remove_members(&[id], shutdown, |_| {}).await });
        }
        while let Some(removal) = removals.join_next().await {
            removal??;
        }
        let remaining = store.membership().await?;
        assert_eq!(remaining.agent_ids, initial.agent_ids);
        assert_eq!(remaining.revision, initial.revision + 24);
        assert_eq!(store.read_board(objective.seq, 100).await?.len(), 24);
        assert!(store
            .set_vote("agent-014", true, "retired stale vote")
            .await
            .is_err());
        store.flush().await?;
        let reopened = Store::open(&path).await?;
        assert_eq!(reopened.initialize_membership(99, true).await?, remaining);
        let next = reopened
            .add_members(1, shutdown.subscribe(), |_| {})
            .await?;
        assert_eq!(next.agent_ids.last().unwrap(), "agent-015");
        // Even a fresh session cannot allocate an old dynamically added ID.
        reopened.initialize_membership(1, false).await?;
        let fresh = reopened
            .add_members(1, shutdown.subscribe(), |_| {})
            .await?;
        assert_eq!(fresh.agent_ids, ["agent-001", "agent-016"]);
        Ok(())
    }

    #[tokio::test]
    async fn membership_validation_is_atomic_and_stale_rosters_cannot_commit_consensus(
    ) -> Result<()> {
        let store = Store::open(":memory:").await?;
        let initial = store.initialize_membership(2, false).await?;
        let (shutdown, shutdown_rx) = watch::channel(false);
        let current = store.add_members(2, shutdown.subscribe(), |_| {}).await?;
        let cursor = store.latest_seq().await?;
        assert!(store
            .remove_members(&current.agent_ids, shutdown.subscribe(), |_| {})
            .await
            .is_err());
        assert!(store
            .remove_members(&["missing".into()], shutdown.subscribe(), |_| {})
            .await
            .is_err());
        assert!(store
            .remove_members(
                &["agent-001".into(), "agent-001".into()],
                shutdown.subscribe(),
                |_| {}
            )
            .await
            .is_err());
        assert!(store
            .add_members(497, shutdown.subscribe(), |_| {})
            .await
            .is_err());
        assert_eq!(store.membership().await?, current);
        assert_eq!(store.latest_seq().await?, cursor);
        for id in &initial.agent_ids {
            store
                .set_vote_at(id, true, "fresh board but old roster", cursor)
                .await?;
        }
        assert_eq!(
            store
                .try_commit_consensus_at_with_shutdown(
                    &initial.agent_ids,
                    cursor,
                    cursor,
                    2,
                    shutdown.clone()
                )
                .await?,
            None
        );
        assert_eq!(
            store
                .try_commit_consensus_at_with_shutdown(
                    &current.agent_ids,
                    cursor,
                    cursor,
                    2,
                    shutdown.clone()
                )
                .await?,
            None,
            "lower obsolete threshold cannot close current roster"
        );
        assert!(!*shutdown_rx.borrow());
        store
            .set_vote_at("agent-003", true, "new roster considered", cursor)
            .await?;
        assert_eq!(
            store
                .try_commit_consensus_at_with_shutdown(
                    &current.agent_ids,
                    cursor,
                    cursor,
                    3,
                    shutdown.clone()
                )
                .await?,
            Some(3)
        );
        assert!(*shutdown_rx.borrow());
        let committed = store.latest_seq().await?;
        assert!(store
            .add_members(1, shutdown.subscribe(), |_| {})
            .await
            .is_err());
        assert!(store
            .remove_members(&["agent-004".into()], shutdown.subscribe(), |_| {})
            .await
            .is_err());
        assert_eq!(store.membership().await?, current);
        assert_eq!(store.latest_seq().await?, committed);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn admitted_membership_change_survives_cancelled_caller_and_publishes_after_commit(
    ) -> Result<()> {
        let store = Store::open(":memory:").await?;
        store.initialize_membership(1, false).await?;
        let objective = store
            .append("owner", "exercise cancelled membership caller", true)
            .await?;
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let blocked = store.clone();
        let blocker = tokio::spawn(async move {
            blocked
                .call(move |_| {
                    let _ = started_tx.send(());
                    release_rx.recv()?;
                    Ok(())
                })
                .await
        });
        started_rx.await?;
        let (shutdown, _) = watch::channel(false);
        let (published, roster) = watch::channel(None);
        let changing = store.clone();
        let caller = tokio::spawn(async move {
            changing
                .add_members(1, shutdown.subscribe(), move |membership| {
                    published.send_replace(Some(membership));
                })
                .await
        });
        // Actor is blocked above; occupied queue capacity proves admission,
        // avoiding timing assumptions about when the caller may be cancelled.
        while store.commands.capacity() == QUEUE_CAPACITY {
            tokio::task::yield_now().await;
        }
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        release_tx.send(())?;
        blocker.await??;
        store.flush().await?;
        let membership = store.membership().await?;
        assert_eq!(membership.agent_ids, ["agent-001", "agent-002"]);
        assert_eq!(*roster.borrow(), Some(membership));
        assert!(store.read_board(objective.seq, 10).await?[0]
            .body
            .contains("Owner added agent-002"));
        Ok(())
    }

    #[tokio::test]
    async fn retiring_started_workers_reserve_capacity_until_drained_but_idle_removals_do_not(
    ) -> Result<()> {
        let store = Store::open(":memory:").await?;
        store.initialize_membership(500, false).await?;
        let (shutdown, _) = watch::channel(false);
        assert!(store.mark_worker_started("agent-500").await?);
        store
            .remove_members(&["agent-500".into()], shutdown.subscribe(), |_| {})
            .await?;
        assert!(!store.mark_worker_started("agent-500").await?);
        let withdrawn = store.membership().await?;
        let cursor = store.latest_seq().await?;
        assert!(store
            .add_members(1, shutdown.subscribe(), |_| {})
            .await
            .is_err());
        assert_eq!(store.membership().await?, withdrawn);
        assert_eq!(store.latest_seq().await?, cursor);
        store.mark_worker_finished("agent-500").await?;
        let replacement = store.add_members(1, shutdown.subscribe(), |_| {}).await?;
        assert_eq!(replacement.agent_ids.len(), 500);
        assert_eq!(replacement.agent_ids.last().unwrap(), "agent-501");
        store
            .remove_members(&["agent-501".into()], shutdown.subscribe(), |_| {})
            .await?;
        let idle_replacement = store.add_members(1, shutdown.subscribe(), |_| {}).await?;
        assert_eq!(idle_replacement.agent_ids.last().unwrap(), "agent-502");
        Ok(())
    }

    #[tokio::test]
    async fn external_false_watch_cannot_bypass_durable_consensus_drain_phase() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("drain-phase.sqlite");
        let store = Store::open(&path).await?;
        let roster = store.initialize_membership(2, false).await?;
        let external = Store::open(&path).await?;
        for id in &roster.agent_ids {
            assert!(store.mark_worker_started(id).await?);
            store.set_vote(id, true, "current work verified").await?;
        }
        let (shutdown, local_shutdown) = watch::channel(false);
        let (independent, independent_shutdown) = watch::channel(false);
        assert_eq!(
            store
                .try_commit_consensus_at_with_shutdown(&roster.agent_ids, 0, 0, 2, shutdown)
                .await?,
            Some(2)
        );
        assert!(*local_shutdown.borrow());
        assert!(!*independent_shutdown.borrow());
        assert!(external
            .add_members(1, independent.subscribe(), |_| {})
            .await
            .is_err());
        assert!(external
            .remove_members(&["agent-002".into()], independent.subscribe(), |_| {})
            .await
            .is_err());
        assert!(!external.mark_worker_started("agent-001").await?);
        assert_eq!(external.membership().await?, roster);
        assert_eq!(external.latest_seq().await?, 1);
        assert!(external.finish_round().await.is_err());
        store.mark_worker_finished("agent-001").await?;
        assert!(external.finish_round().await.is_err());
        store.mark_worker_finished("agent-002").await?;
        store.finish_round().await?;
        let next = external
            .add_members(1, independent.subscribe(), |_| {})
            .await?;
        assert_eq!(next.agent_ids.last().unwrap(), "agent-003");
        assert_eq!(
            external.latest_seq().await?,
            1,
            "membership stays silent without an owner objective"
        );
        store.close_session().await?;
        assert!(external
            .add_members(1, independent.subscribe(), |_| {})
            .await
            .is_err());
        assert!(external
            .remove_members(&["agent-003".into()], independent.subscribe(), |_| {})
            .await
            .is_err());
        assert_eq!(external.membership().await?, next);
        assert_eq!(external.latest_seq().await?, 1);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_500_writers_have_gapless_global_pages() -> Result<()> {
        let store = Store::open(":memory:").await?;
        let mut changes = store.subscribe_board();
        let mut writers = tokio::task::JoinSet::new();
        for index in 0..500 {
            let store = store.clone();
            writers.spawn(async move {
                store
                    .append(
                        format!("agent-{index:03}"),
                        format!("update {index}"),
                        false,
                    )
                    .await
            });
        }
        while let Some(result) = writers.join_next().await {
            result??;
        }
        changes.changed().await?;
        assert_eq!(*changes.borrow_and_update(), 500);
        let mut cursor = 0;
        let mut seen = std::collections::HashSet::new();
        loop {
            let page = store.read_board(cursor, 17).await?;
            if page.is_empty() {
                break;
            }
            for entry in page {
                assert_eq!(entry.seq, cursor + 1);
                assert!(seen.insert(entry.sender));
                cursor = entry.seq;
            }
        }
        assert_eq!(cursor, 500);
        assert_eq!(seen.len(), 500);
        Ok(())
    }

    #[tokio::test]
    async fn persistent_board_votes_and_replaced_checkpoint_survive_reopen() -> Result<()> {
        #[derive(Serialize)]
        struct BorrowedCheckpoint<'a> {
            cursor: u64,
            messages: &'a [&'a str],
        }
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("state.sqlite");
        let store = Store::open(&path).await?;
        store.append("owner", "objective", true).await?;
        store
            .save_checkpoint("agent-001", &json!({"cursor": 0}))
            .await?;
        store
            .save_checkpoint_serializable(
                "agent-001",
                &BorrowedCheckpoint {
                    cursor: 1,
                    messages: &["retained"],
                },
            )
            .await?;
        store.set_vote("agent-001", true, "verified").await?;
        store.flush().await?;
        let reopened = Store::open(&path).await?;
        assert_eq!(reopened.read_board(0, 10).await?[0].body, "objective");
        assert_eq!(
            reopened.load_checkpoint("agent-001").await?,
            Some(json!({"cursor": 1, "messages": ["retained"]}))
        );
        assert_eq!(reopened.votes().await?[0].board_seq, 1);
        assert_eq!(*reopened.subscribe_board().borrow(), 1);
        assert_eq!(
            reopened.stats().await?,
            StoreStats {
                board_messages: 1,
                checkpoints: 1,
                votes: 1
            }
        );
        assert!(reopened.load_checkpoint("missing").await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn owner_update_revokes_votes_atomically_and_agent_posts_do_not() -> Result<()> {
        let store = Store::open(":memory:").await?;
        store.set_vote("agent-001", true, "done").await?;
        store.append("agent-002", "reviewing", false).await?;
        assert_eq!(store.votes().await?.len(), 1);
        store.append("owner", "new requirement", true).await?;
        assert!(store.votes().await?.is_empty());
        store.set_vote("agent-001", true, "verified again").await?;
        store.set_vote("agent-001", false, "found issue").await?;
        assert_eq!(store.votes().await?.len(), 1);
        assert!(!store.votes().await?[0].done);
        store.reset_votes().await?;
        assert!(store.votes().await?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn external_owner_refresh_wakes_local_subscribers_and_rejects_stale_votes() -> Result<()>
    {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("external.sqlite");
        let local = Store::open(&path).await?;
        let external = Store::open(&path).await?;
        let mut changes = local.subscribe_board();
        local.set_vote("agent-001", true, "verified").await?;
        external.append("owner", "new instruction", true).await?;
        assert!(!changes.has_changed()?);
        assert_eq!(local.refresh_board().await?, 1);
        assert!(changes.has_changed()?);
        assert_eq!(*changes.borrow_and_update(), 1);
        local.refresh_board().await?;
        assert!(!changes.has_changed()?);
        assert!(local.vote("agent-001").await?.is_none());
        assert!(local
            .set_vote_at("agent-001", true, "stale", 0)
            .await
            .is_err());
        assert!(local.vote("agent-001").await?.is_none());
        local.set_vote_at("agent-001", true, "fresh", 1).await?;
        assert!(local.vote("agent-001").await?.unwrap().done);
        local
            .set_vote_at("agent-001", false, "withdraw despite stale cursor", 0)
            .await?;
        assert!(!local.vote("agent-001").await?.unwrap().done);
        Ok(())
    }

    #[tokio::test]
    async fn consensus_commit_checks_owner_revision_and_known_voters_atomically() -> Result<()> {
        let store = Store::open(":memory:").await?;
        let ids = vec!["agent-001".to_owned(), "agent-002".to_owned()];
        let owner = store.append("owner", "objective", true).await?;
        store.set_vote("agent-001", true, "verified").await?;
        store
            .set_vote("outsider", true, "not a swarm voter")
            .await?;
        assert_eq!(store.try_commit_consensus(&ids, owner.seq, 2).await?, None);
        store.set_vote("agent-002", true, "verified").await?;
        assert_eq!(
            store.try_commit_consensus(&ids, owner.seq + 1, 2).await?,
            None
        );
        let updated_owner = store.append("owner", "new objective", true).await?;
        assert_eq!(store.try_commit_consensus(&ids, owner.seq, 2).await?, None);
        store.set_vote("agent-001", true, "reverified").await?;
        store.set_vote("agent-002", true, "reverified").await?;
        assert_eq!(
            store
                .try_commit_consensus(&ids, updated_owner.seq, 2)
                .await?,
            Some(2)
        );
        let marker = store.read_board(updated_owner.seq, 10).await?;
        assert_eq!(marker.len(), 1);
        assert_eq!(marker[0].sender, "harness");
        assert!(!marker[0].owner);
        Ok(())
    }

    #[tokio::test]
    async fn peer_updates_preserve_quorum_and_owner_revision_grace() -> Result<()> {
        let store = Store::open(":memory:").await?;
        let ids = vec!["agent-001".to_owned(), "agent-002".to_owned()];
        let owner = store.append("owner", "objective", true).await?;
        store
            .set_vote_at("agent-001", true, "verified", owner.seq)
            .await?;
        store
            .set_vote_at("agent-002", true, "verified", owner.seq)
            .await?;
        let peer = store
            .append("agent-002", "review found more work", false)
            .await?;
        assert_eq!(store.latest_owner_seq().await?, owner.seq);
        assert!(store
            .votes()
            .await?
            .iter()
            .all(|vote| vote.done && vote.board_seq == owner.seq));
        // New votes still require full delivery, while existing evidence remains
        // valid despite newer peer chatter during the consensus grace period.
        assert!(store
            .set_vote_at("agent-003", true, "unread peer post", owner.seq)
            .await
            .is_err());
        let (shutdown, shutdown_rx) = watch::channel(false);
        let mut revisions = store.subscribe_board();
        // A cursor older than the owner instruction cannot authorize a gate.
        assert_eq!(
            store
                .try_commit_consensus_at_with_shutdown(
                    &ids,
                    owner.seq,
                    owner.seq.saturating_sub(1),
                    2,
                    shutdown.clone(),
                )
                .await?,
            None
        );
        assert!(!*shutdown_rx.borrow());
        assert!(!revisions.has_changed()?);
        assert_eq!(store.latest_seq().await?, peer.seq);
        assert_eq!(
            store
                .try_commit_consensus_at_with_shutdown(&ids, owner.seq, owner.seq, 2, shutdown)
                .await?,
            Some(2)
        );
        revisions.changed().await?;
        assert!(*shutdown_rx.borrow());
        Ok(())
    }
}
