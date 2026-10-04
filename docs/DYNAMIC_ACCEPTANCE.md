# Dynamic membership acceptance

This record maps the ten adversarial cases in
[`tests/dynamic_membership.rs`](../tests/dynamic_membership.rs) to the required
membership, graceful-removal, liveness, and automatic-continuation contracts.
It supplements the [full verification record](VERIFICATION.md), which owns
unified gates, platform builds, and public-console walkthrough results.

## Recorded command and result

```powershell
cargo test --locked --test dynamic_membership -- --nocapture
```

The latest consolidated run on 2026-10-03 reported **10 passed, 0 failed,
0 ignored**, warning-free, in **0.40 seconds**. All ten cases also passed in the
frozen unified `cargo test --locked --all-targets -- --include-ignored` gate:
**208 total tests** (130 library, 15 CLI-configuration, 63 integration) in the final
direct gate, which adds the imported-custom-endpoint setup regression to the
earlier 207-test frozen gate. The dynamic suite remains ten cases.

The exact rejected-batch test was separately rerun after the requested
existing-vote assertion:

```powershell
cargo test --locked --test dynamic_membership rejected_membership_changes_do_not_publish_notices_or_change_roster -- --exact --nocapture
```

That run reported **1 passed, 0 failed**, warning-free. Owned-target
`cargo clippy --locked --test dynamic_membership -- -D warnings` and formatting
checks also passed. These are recorded results; they do not require repeating
unchanged gates to establish readiness.

## Exact case-to-contract mapping

### 1. Parallel idle mutations and persistent roster

`parallel_idle_mutations_are_durable_and_persistent_rounds_use_current_roster`

- Concurrent additions allocate five distinct identities; concurrent batch
  removals leave the exact expected active roster.
- Every added/removed identity has durable global notices, independently
  verified after reopening SQLite. Membership controls are not user prompts.
- Two completed persistent rounds use the current three-member roster. Removed
  idle members never spawn in those rounds.
- An idle addition followed by detach reports **four current agents** while
  retaining **three prior-round drained workers**. A resumed session restores
  and drains all four members despite an original configured count of two.

### 2. Invalid batches preserve all prior evidence

`rejected_membership_changes_do_not_publish_notices_or_change_roster`

Zero additions, active-capacity overflow, integer overflow, last-member removal,
and mixed valid/unknown removal batches are rejected atomically. The active
roster, global cursor, and **exact previously valid completion vote** remain
unchanged; no partial membership notice is published.

### 3. Held HTTP retirement and full-history late join

`removed_worker_drains_http_and_checkpoint_while_added_worker_reads_full_global_board`

A concurrent live addition/removal occurs while two real localhost HTTP
requests are held. The retired request finishes naturally and its assistant
message is checkpointed. All not-yet-started `board_read`, `apply_patch`, and
`run_command` calls receive explicit skipped results; no new file or process
side effect is launched. The joined worker receives the original objective,
earlier peer history, and retirement notice from the same unfiltered board.
The retired identity quits once without respawning; the final summary has
**two active members and three drained workers**.

### 4. Active-plus-draining admission bound and final closure

`additions_count_removed_workers_until_their_inflight_http_requests_drain`

With one active worker and one retired worker still holding an HTTP request,
adding 499 workers is rejected: the **500-worker bound includes the draining
slot**. Rejection leaves the cursor/roster unchanged. Both admitted workers
eventually drain, retirement signals clear, and an independent caller with a
false local shutdown watch cannot add workers after the headless supervisor
exits. That rejected late addition publishes no notice.

### 5. Many parallel running changes drain exactly once

`parallel_running_additions_and_removals_drain_every_worker_once`

Ten concurrent additions each return only their own newly allocated identity.
All ten dispatch against the ongoing objective before ten parallel removals.
Their held requests finish and each identity posts one quit notice. The two
original workers remain active and subsequently complete: **12 workers drain
for a final two-member roster**, with no retired completion votes remaining.

### 6. An already-running tool finishes; later tools do not start

`removal_drains_an_already_running_command_and_skips_remaining_tool_calls`

A real native command first creates its start marker and waits for an external
release. Removal does not cancel it or announce completion early. After release,
the command finishes normally and its result is checkpointed; the following
`apply_patch` call is skipped and its target file never appears. This distinguishes
draining an admitted operation from admitting a new operation after retirement.

### 7. Unexpected worker interruption automatically continues the same agent

`unexpected_checkpoint_interruption_restarts_same_agent_without_replaying_side_effects`

A SQLite trigger injects a genuine result-checkpoint failure **after a native
command's side effect**. The supervisor automatically restarts the same identity
from its pending checkpoint, retains original history, publishes one durable
restart notice, and repairs missing tool results as unknown outcomes. The
observed command side effect occurs once, and the following mutation is not
replayed. This verifies automatic supervisor continuation without a production
test hook or manual restart command.

### 8. An interrupted persistent task resumes its objective, roster, and history

`interrupted_persistent_session_resumes_unfinished_prompt_roster_and_checkpoint_automatically`

A saved unfinished owner task, three-member durable roster, and pending tool
checkpoint are reopened with an empty objective and configured count of one.
With `resume` enabled, the original three identities dispatch automatically,
receive the unfinished objective/history, and repair pending tool outcomes.
No new user submission, duplicate owner prompt, or unknown-side-effect replay
is required. The console remains open after the recovered round completes.
Automatic selection of recovery by remembered public `setup` is separately
covered by the full verification record's actual-console regression.

### 9. Independent durable roster changes wake workers and gate new effects

`changes_from_an_independent_store_handle_wake_and_reconcile_running_workers`

An independent SQLite handle adds a collaborator, which joins ongoing work and
receives its global notice. It then removes a worker and releases the held HTTP
response **immediately**, without waiting for local supervisor reconciliation.
Safe tool admission observes durable retirement: all returned unstarted tool
calls are checkpointed as skipped, no files/process effects appear, and the
worker quits once. The final roster has **two active members and three drained
workers**. Coherent revision/roster snapshot stress coverage is complementary
storage evidence recorded in the full verification record.

### 10. Owner and membership steering wake voters; peer chatter preserves votes

`waiting_voted_workers_ignore_peers_and_wake_for_owner_and_membership_work_without_waiting_for_other_requests`

Two workers first vote and wait while a third real HTTP request remains held,
preventing the three-member quorum. Owner steering wakes both voters, while an
ordinary peer entry preserves their exact vote rows and starts no provider work.
A membership notice wakes them again with full unfiltered history. A fourth
collaborator joins, and the four-member quorum cannot complete while two requests
remain held. Releasing admitted requests then drains all four workers.

## Interpretation boundaries

- Fixture deadlines bound the regression harness; production provider requests,
  commands, and retirement drains have no duration-based abort policy.
- Checkpoints repair unknown outcomes rather than promising exactly-once
  filesystem or subprocess effects. Case 7 verifies one observed injected-fault
  scenario, not an arbitrary side-effect guarantee.
- Public shortcut/menu responsiveness, pooled transport, native PTY lifecycle,
  atomic storage consensus/phase boundaries, and fresh optimized console runs
  have complementary evidence in [VERIFICATION.md](VERIFICATION.md). This mapping
  names the ten dynamic integration cases without substituting them for those
  other acceptance checks.
