# Verification record

## v1.1.1 — Session reopening, storage, roster, and inspection

This patch release combines the issue #15 fix and the #16–#18 fixes documented
below. The combined Windows acceptance passed **316 Rust tests**: 187 library,
19 CLI-configuration, and 110 integration tests, with no failures or ignored
tests. Release preparation reran the full locked all-target suite with version
**1.1.1** and passed the same 316 tests. Warnings-denied all-target Clippy,
formatting, and whitespace checks passed. The Node SDK/provider/transport suite
was also rerun and passed **25 tests**. README links/headings, SVG validity, and
the publication-source scan passed.

Release automation verifies and builds Linux x86-64, Windows x86-64, macOS Intel,
and macOS Apple Silicon. Publication and downloadable archives depend on those
native CI jobs succeeding; the local acceptance results do not assert CI success.

## Remembered databases, live roster, and agent inspection — issues #16–#18

- **#16:** Implicit workspace reopening corrects the old root
  `openraid.sqlite3` path from both remembered launch profiles and the latest
  session catalog entry to `.openraid/openraid.sqlite3`. The corrected launch
  profile is saved on startup. Existing root database/WAL/SHM files remain
  untouched; explicit `--database`/`--session` and custom paths retain their
  behavior. `tests/workspace_database_migration.rs` exercises actual native
  terminal launches, including a second reopening and explicit old-session access.
- **#17:** The dashboard renders the committed active roster independently of
  historical telemetry and retiring workers. Removed agents disappear on the
  next redraw while admitted work drains safely. Selection follows agent identity
  across earlier-row removal; retiring the inspected agent resets its history
  cursor. Rendered-sidebar tests and a held-HTTP membership regression verify
  immediate visibility, retained usage/activity, and completed retirement.
- **#18:** Individual inspection uses OpenCode's
  [session view](https://github.com/anomalyco/opencode/blob/dev/packages/tui/src/routes/session/index.tsx)
  and [sidebar](https://github.com/anomalyco/opencode/blob/dev/packages/tui/src/routes/session/sidebar.tsx)
  as references: an accented transcript, distinct response/tool/result headings,
  themed payload surfaces, and a responsive session/usage/activity sidebar.
  Compact terminals retain the transcript and follow/history controls. Rendering
  regressions cover visual hierarchy, literal fenced payloads, resizing, Unicode,
  complete history beyond 65,535 rows, and existing inspection navigation.

Final Windows verification passed on **2026-10-04**:

| Check | Result |
| --- | --- |
| `cargo test --locked --all-targets -- --include-ignored` | **316 passed, 0 failed, 0 ignored**: 187 library, 19 CLI-configuration, 110 integration, including native terminal and optional Node/MCP/SDK fixtures |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| `cargo fmt --all -- --check` | Passed |
| `git diff --check` | Passed |

## Idle session reopening — issue #15

Interactive `setup` and session switching now open saved sessions idle, including
unfinished tasks. Work begins when the operator submits an objective, or when
the launch explicitly requests `--resume`. Idle reopening preserves the durable
roster, votes, checkpoints, and unfinished prompt.

The native terminal regression in `tests/remembered_recovery.rs` verifies zero
provider requests and zero new board rows while reopening and exiting unfinished
history, then verifies explicit `setup --resume` restores the original task,
roster, and checkpoint without duplicating the prompt. Focused startup and runtime
unit tests cover the recovery opt-in and durable-state preservation.

Windows verification passed on **2026-10-04**:

| Check | Result |
| --- | --- |
| `cargo test --locked --all-targets -- --include-ignored` | **300 passed, 0 failed, 0 ignored**, including native terminal and optional Node/MCP/SDK fixtures |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| `cargo fmt --all -- --check` | Passed |
| `git diff --check` | Passed |

Older automatic remembered-session recovery results below describe historical
behavior superseded by this issue fix for interactive launches.

## v1.1.0 — Themes and issues #11–#14

The combined implementation's Windows acceptance passed **298 Rust tests**
(`cargo test --locked --all-targets -- --include-ignored`): 174 library,
18 binary/CLI-configuration, and 106 integration tests, with zero failures or
ignored tests. The Node SDK/provider/HTTP suite passed **25/25**. Repository-wide
formatting and whitespace checks also passed. Direct release preparation reran
the complete Windows gate with version 1.1.0 and passed the same 298 tests and
25 Node tests. Warnings-denied all-target Clippy passed after grouping the PTY
helper's execution metadata and removing an unnecessary owned test comparison.

This gate covers:

- Ten semantic theme palettes, at least 4.5:1 text/status/selection contrast,
  Unicode aliases, saved preferences and error cleanup, local previews,
  dark/light filtering, compact layouts, and theme routing during remote work.
- Full-screen individual-agent inspection, preserved dashboard return, all 11
  navigation/render regressions, histories beyond 65,535 rows, tool details,
  authenticated live output, split Unicode, per-agent isolation, and spool cleanup.
- Immediate operator stop during requests, commands, PTYs, consensus drain,
  snapshot preparation, and SQLite/control-lock contention. Queued mutations and
  running blocking reads observe generation and caller-drop cancellation.
- SDK caller-drop cancellation, actual HTTP abort with an already-active sibling,
  exact-ID MCP cancellation and reusable shared connections, plus real stdio cleanup.
- Unconditional nested database defaults, root database/WAL/SHM preservation,
  explicit paths, and the concurrency-prompt clarification.

Activity transcripts use temporary per-agent spools for the current runtime,
separate from durable SQLite board/checkpoint state and process output logs.
Stop does not roll back previously completed filesystem side effects; already-issued
OS operations can finish before the next cooperative cancellation boundary.

Four-platform release CI gates publication on native tests, formatting, all-target
warnings-denied Clippy, Node tests, optimized builds, and 500-worker offline smoke
checks on Windows, Linux, macOS Intel, and macOS Apple Silicon. Historical records
below describe their corresponding versions rather than the current stop semantics.

## Session controls and directory isolation — issue #4

The final v1.0.1 session-control update was verified on **2026-10-03** using native
Windows and Linux under WSL, Rust/Cargo 1.99.0 and Node.js 22.14.0 for optional
Linux Rust fixtures. The separate Node provider/transport suite ran with the host
Node installation.

| Check | Result |
| --- | --- |
| Windows `cargo test --locked --all-targets -- --include-ignored` | **223 passed, 0 failed, 0 ignored**: 136 library, 18 CLI-configuration, 69 integration |
| Linux `cargo test --locked --all-targets -- --include-ignored` | **224 passed, 0 failed, 0 ignored**: 136 library, 18 CLI-configuration, 70 integration |
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed, warning-free |
| Node SDK/provider-loader/HTTP-transport suite | **24 passed** |
| `git diff --check` and `bash -n run.sh` | Passed |

Coverage includes invocation-directory scoping, fresh databases, local/all-workspace
session listing, explicit cross-workspace IDs, conflicting flags, canonical
database aliases, and separate saved model settings for older sessions. Real
provider fixtures verify pause/resume history preservation, draining an admitted
request, skipping unstarted side effects on stop, reusable idle consoles, and
unfinished-task recovery after closing. Startup/queued-prompt and snapshot/control
races have dedicated regressions. Terminal tests cover commands, editing shortcuts,
clickable controls, compact layouts, visible cwd/session/status, and lifecycle
actions while an unrelated mutation is pending.

A direct walkthrough against the final binary used a real Unix terminal and
held local HTTP provider requests. It verified visible workspace/session/buttons,
`/start`, pause during an admitted request, resume with preserved tool history,
STOPPING while the next request was held, clean drain with its unstarted file
mutation skipped, `/new`, the workspace session chooser, switching back without
replaying the explicitly stopped task, and clean console exit. Fixture processes,
HTTP sockets, and terminal handles were closed afterward.

Final review also serialized queued-round admission with operator stop, preventing
startup from clearing an already-acknowledged stop between selecting a prompt and
admitting its workers. The snapshot/control regression verifies that late admission
preserves the stop signal.

The native release gate additionally exposed a close/admission race on Linux:
workers now observe console closure directly at dispatch boundaries, and the
supervisor does not restart a worker finishing during closure. The held-request
detach regression explicitly rejects any subsequent provider turn.
The corrected ten-test session-control suite and warnings-denied all-target Clippy
passed again on Windows and Linux; the Linux held-request detach regression then
passed **80 consecutive runs**.

The held-MCP initialization regression now waits for its explicit request-start
barrier before allowing a fast offline round to reach consensus; it retains the
worker-collaboration and final transport-shutdown assertions.

The earlier release/build/launcher and provider-source audits below are historical
platform evidence; the final issue-fix Rust gates above ran on both Windows and Linux.

## Previous native release acceptance gate

The current checklist includes the provider-loader behavioral audit, domain-folder
reorganization, MCP integration, branding/default endpoints, persistent-console
walkthroughs, native PTY support, and parallel worker add/remove controls with
durable global notices, graceful draining, and dynamic consensus. It also covers
worker liveness after new work/model changes and automatic same-agent checkpoint
continuation after unexpected exits or interrupted remembered sessions. Earlier gates
below are historical evidence, not acceptance of those new requirements.

After the coordinated implementation, final acceptance was completed directly,
including a regression fix for imported native providers with a missing URL.
The final source was verified on Windows on **2026-10-03**, using rustc
1.96.0, Cargo 1.96.0, Node.js 24.21.0, and npm 11.19.0:

| Check | Result |
| --- | --- |
| `cargo test --locked --all-targets -- --include-ignored` | **208 passed, 0 failed, 0 ignored**: 130 library, 15 CLI-configuration, 63 integration |
| `cargo fmt --all -- --check` | Passed after coordinated formatting |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed, warning-free |
| `git diff --check` | Passed |
| `npm test --prefix scripts` | **24 passed**, including actual SDK/discovery HTTP and effective no-deadline transport |
| `node scripts/check-catalog-adapters.mjs` | **29 catalog adapters** available, 226 source providers / 8,385 raw models |
| `node scripts/check-opencode-providers.mjs` | **23 actual upstream loader fixtures**, 56 Bedrock mappings, six selectors, all 24 upstream adapter entries accounted for |
| `cargo build --locked --release` | Passed on final source in **1 min 40 s**; `target/release/openraid.exe` |
| Ubuntu `cargo test --locked --test native_pty` | **9 passed**, warning-free, real Unix backend on the frozen source |
| Outside-checkout `build_linux.sh` and `run.sh` | Optimized build, `--help`, and 500-agent demo passed with isolated native toolchain/target |

The source audit matched the current upstream file to reviewed commit
`35fc7a776cddb72d334ee60590c3cce41154be05`. It constructs all 23 nonnative
upstream SDK models; native Copilot routing/account behavior is covered in Rust.
The [source-provider audit](SOURCE_PROVIDER_AUDIT.md) records every loader and
adapter's behavior and executable evidence. Factory availability is a separate
check, not a claim of loader parity.

The final Rust gate includes all optional Node fixtures, **ten dynamic-membership
cases**, **eight real Windows PTY cases**, **six MCP backend cases**, **six runtime
unit cases**, and **14 terminal UI cases**. The held remote-MCP initialization
test verifies that workers still collaborate and reach consensus, final shutdown
returns, and the server socket observes EOF. This closes the upstream bootstrap
lifecycle leak without introducing duration-based cancellation. Open MCP menus
follow real connecting/ready/failed/disabled transitions without extra requests.

Membership notices, roster mutation, vote invalidation, and consensus checks are
serialized in SQLite. Coherent external snapshots, active-plus-draining capacity,
safe tool admission, paused-peer wakeups, current-roster grace/quorum, and durable
closed-drain phases are covered. Already-running requests/tools finish; unstarted
tool calls after retirement receive checkpointed skipped results. The public
module names and catalog/Responses include paths still compile after grouping
Rust implementations into domain folders.

All **13 storage cases** passed in the frozen gate: seven membership/snapshot/
capacity/cancellation/recovery-phase cases and six original global-board/vote/
checkpoint/fresh-consensus cases. Membership phase reopens only for an attached,
continuing persistent session after every worker joins; headless/final exits stay
closed until an intentional new session initialization. Independent read-only
checks of the optimized 500-agent and native-PTY databases confirmed the expected
full-drain markers, closed phase, **zero worker slots**, and no unfinished task.
The final release dependency manifest includes the new domain paths, bundled
`data/models.json`, adjacent `providers/responses.rs`, and `terminal/pty.rs`.

Fresh Windows launchers, optimized multi-round controls, native PTY, live MCP
status, exact Unicode input, and completed remembered-home reopening all passed.
The [dynamic acceptance record](DYNAMIC_ACCEPTANCE.md) maps the ten adversarial
membership/recovery cases to their contracts. Model quality, paid-cloud access, and live
500-agent provider throughput are not established by these local fixtures.

### Full checklist coverage

| Requirement | Evidence on final source |
| --- | --- |
| Branding and native endpoint defaults | CLI/configuration/native-protocol regressions; optimized native-console walkthroughs; missing custom URL regression fails before the fix and passes afterward |
| Persistent interactive console | Multi-round session tests, actual remembered-home ConPTY test, independent parallel roster action lane; optimized two-round controls and completed-home no-replay pass |
| MCP stdio and remote tools/resources/prompts | Six backend regressions with real Node/HTTP fixtures, pending-child reaping, inherited overlays, collision isolation, explicit remote bootstrap/SSE cleanup |
| Every current OpenCode loader/adapter | Pinned 23-loader/24-entry behavioral audit, all 29 catalog SDK constructions, actual GitLab/Azure/Cloudflare/Salad HTTP tests, native Rust protocol/account regressions |
| Clear Rust domains with preserved interfaces/includes | All original public module imports compile; bundled catalog and sibling Responses includes resolve; full 208-test gate and release build pass |
| Native PTY | Eight Windows cases pass in final gate; nine real Unix cases pass on frozen source |
| Easy parallel add/remove | Slash commands, F7/F8/F9, leader shortcuts, independent UI action lane; ten adversarial end-to-end cases |
| Durable unfiltered global notices and correct dynamic consensus | Transactional notices/roster/votes, fresh active-only quorum and grace, no last-member removal, monotonic IDs, coherent independent-handle snapshots |
| Capacity and graceful draining | Active-plus-started-draining limit, held HTTP/running-command completion, skipped unstarted side effects, all-join cleanup, final closed membership phase |
| Sleeping/liveness fixes | Owner/peer/membership wakeups, model-change retry/blocked-context wakeups, nonblocking MCP startup, live status and idle/retired telemetry |
| Automatic session continuation | Same-ID unexpected-error/panic recovery, repaired pending-tool history without observed command replay, saved unfinished task/roster recovery, completed-history no-replay |
| Final checks, release, scripts, and accurate docs | Final Rust/Node/fmt/Clippy/source checks, Windows/Linux releases and actual platform build/run pass; optimized multi-round/status acceptance complete below |

### Final Windows release and launcher verification

The frozen optimized artifact was copied into an isolated temporary fixture
location for public terminal walkthroughs, leaving the checkout release binary
unlocked:

- `build_win.bat` from outside the checkout with `CARGO_TARGET_DIR`: **passed**;
  status output identified the actual custom-target artifact.
- `run.bat --help` from outside the checkout: **passed**.
- `run.bat` forwarded the final 500-agent zero-grace offline demo: **500 active,
  500 drained, 500 votes, 1,003 durable board messages, 934 ms**. The preceding
  custom-target script check also passed (942 ms for its separate fixture).
- The final optimized public native-PTY tool chain passed: spawn, resize,
  exact Unicode input/echo, read/list/cleanup and normal vote/drain. Summary:
  **one active / one drained / one vote / five messages / 106 ms**.
- Actual default `setup`, without `--resume`, recovered an unfinished task on
  the **same three durable IDs despite a one-agent saved launch**, without
  duplicating the owner prompt, and returned idle after drain. The real ConPTY
  screen showed branding and native Responses selection; leader add/remove
  shortcuts preserved the Unicode draft and updated the four-agent quorum to
  three.
- Batch add/remove controls were followed by **two new completed prompt rounds**
  in the optimized persistent console. The final active IDs were `agent-003`,
  `agent-005`, and `agent-008`. Clean idle exit reported **3 active / 3 drained /
  3 votes / 37 durable messages / 114 ms** for the last round. Read-only SQLite
  inspection confirmed distinct prompt/gate/drain groups, a closed final phase,
  and **zero worker slots**. Membership controls were separate from task prompts.
- A direct native-ConPTY driver exercised the actual checkout release against
  isolated Node MCP and HTTP fixtures. Its open filtered MCP menu followed
  **connecting/failed → ready → disabled**; the provider request continued
  independently. The owner prompt retained exact `iii İ` text. The request voted
  and drained normally (**one request, four board messages**), and both consoles
  exited cleanly. Reopening completed remembered home dispatched **zero new
  requests and zero new board rows**.
- Imported native providers with no configured URL now receive endpoint entry in
  setup and `/connect`; existing URLs are reused, and SDK-owned defaults do not
  trigger unnecessary URL prompts. The setup regression failed before the fix;
  the connected-model regression verifies endpoint reuse afterward. The final
  rebuilt native-console walkthrough also connected an imported custom provider
  through URL/key entry, reopened its connection directly at key entry with the
  saved URL reused, and completed the MCP/Unicode/no-replay checks above.

### Final Linux release and launcher verification

See [the reproducible Linux acceptance record](LINUX_ACCEPTANCE.md) for the
complete isolated environment, exact commands, artifact hash, and native suite.

Absolute script paths were invoked from the approved temporary directory outside
the checkout, using an isolated native Linux Rust/Cargo toolchain and
`CARGO_TARGET_DIR`, with the source and lockfile frozen. Platform:
Ubuntu WSL2, Linux 6.18.33.2, rustc/Cargo 1.99.0. Exact script invocation:

```sh
bash /mnt/c/Users/Arda/Desktop/projeler/openraid/build_linux.sh
bash /mnt/c/Users/Arda/Desktop/projeler/openraid/run.sh --help
bash /mnt/c/Users/Arda/Desktop/projeler/openraid/run.sh demo \
  --agents 500 --no-tui --grace-secs 0 \
  --workspace /mnt/c/Users/Arda/AppData/Local/Temp/opencode \
  --database /mnt/c/Users/Arda/AppData/Local/Temp/opencode/direct-final-linux-500.sqlite3 \
  'Verify directly completed Linux global board and graceful drain'
```

- `build_linux.sh`: **exit 0**, final optimized rebuild **1 min 14 s**. Its status output
  correctly identified the custom-target artifact at
  `/mnt/c/Users/Arda/AppData/Local/Temp/opencode/openraid-linux-agent06/release/openraid`.
  Artifact SHA-256:
  `ef38ab2153d97ad30978d86f7f67edf59f5e79608e2affdee3701fa023907644`.
- `run.sh --help`: **exit 0**, expected public commands present.
- `run.sh demo … --agents 500 --no-tui
  --grace-secs 0` with an isolated workspace/database: **exit 0**, **500 agents,
  500 drained, 500 votes, 1,003 durable board messages, 40,186 ms harness time**.
  The database was on mounted NTFS temporary storage, so this is platform/launcher
  correctness evidence rather than a comparable native-filesystem benchmark.
- The final Unix PTY suite passed **9/9** in **0.42 s**, after a 26.86-second
  rebuild. It includes the Unix-only exited-leader/background-descendant cleanup
  case as well as native handles, resize, immediate input, exact Unicode pages,
  full disk logs, shared capacity, ordinary command queues and blocked-input kill.

All application and PTY-test children exited and were reaped; the verification
did not change source files, global toolchain configuration, or Windows artifacts.

### v1.0.0 native-release portability follow-up

The first four-platform release gate identified a membership-admission race and
platform differences that local Windows/Linux acceptance did not expose:

- A membership change could revoke a waiting worker's vote after its board read.
  Provider admission now checks the durable board cursor again, so it catches up
  with the new global notice before dispatching its next request.
- The held MCP stdio fixture now waits for a complete initialization-log record;
  file creation alone did not prove the child's write had finished on macOS.
- Unix PTY cleanup handles Darwin's early terminal EOF after the leader exits,
  terminating any surviving process-group descendants before releasing capacity.
  The regression checks the actual background PID, rather than assuming Linux's
  output-slave lifetime on every Unix platform.
- The large-input Unix fixture explicitly selects noncanonical input and waits
  for that terminal mode. Windows still writes immediately, preserving coverage
  of the ConPTY startup handshake without relying on Unix canonical-line limits.

After these changes, the complete local Windows gate again passed **208 tests**
with none failed or ignored, and warnings-denied all-target Clippy passed. The
actual Linux PTY gate again passed **9/9**. Release publication additionally
requires native tests, Clippy, Node tests, an optimized build, and a 500-worker
smoke on Linux, Windows, macOS Intel, and macOS Apple Silicon.

The earlier frozen gate passed 207 tests and the first Linux build took 2 min 34 s.
The final direct gate adds one meaningful custom-endpoint regression, bringing
the total to 208. Both platform artifacts were rebuilt after that routing fix.

### Earlier interim gates

The 2026-10-03 interim gate, before native PTY and dynamic membership changes,
passed on Windows:

- `cargo test --locked --all-targets -- --include-ignored`: **160 passed,
  0 failed, 0 ignored** — 109 library, 11 CLI-configuration, and 40 integration
  tests. This included real Node-backed SDK and MCP stdio fixtures.
- `npm test --prefix scripts`: **8 passed**.
- `node scripts/check-catalog-adapters.mjs`: **29 adapter imports passed**,
  covering 226 catalog providers and 8,385 raw models. This checks package
  availability; it does not establish OpenCode custom-loader behavioral parity.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy --locked --all-targets -- -D warnings`: passed without warnings.

Regression coverage at that gate includes persistent sessions accepting multiple
rounds without replaying historical prompts; prompts queued before startup;
model changes across compaction; clearing unverified checkpoint reasoning
metadata; retained custom configuration and active credentials; selectable
ad-hoc models; cancellable read-only menu discovery; MCP HTTP authentication,
stdio process sharing/reaping, inherited overlays and queued-disable races; and
snapshot restoration with nested/relative/canonical Windows workspace paths
while preserving user staging and database sidecars.

These interim totals are superseded by the frozen gate above. Live provider
throughput and quality are not implied by local protocol fixtures or offline
swarm measurements.

### Expanded integration snapshot

A later, still pre-freeze snapshot passed **193 Rust tests** (123 library,
12 CLI-configuration, 58 integration; zero failed or ignored), including ten
dynamic-membership and six Windows native-PTY tests. Warning-free locked Clippy
also passed. Later accepted edits added remembered recovery, the independent
roster UI action lane, GitLab discovery integration, and the Unix PTY lifecycle
check; these snapshot totals are not final acceptance totals.

The independent Node gate passed **20 tests**. The strengthened
`node scripts/check-opencode-providers.mjs` matched current upstream source to
reviewed commit `35fc7a776cddb72d334ee60590c3cce41154be05`, executed all **23
custom-loader fixtures**, compared **56 Bedrock region/model cases** and six
routing selectors, and constructed all 23 nonnative upstream adapter entries.
The 24th upstream entry is native Copilot, covered by Rust routing/account tests.
See [the source-provider audit](SOURCE_PROVIDER_AUDIT.md) for every loader and
adapter's implementation and evidence. The source audit uses Node's experimental
TypeScript-stripping API and reports its standard experimental-feature notice.

Dynamic regressions exercise parallel idle and active roster changes, durable
global notices, independent-store reconciliation, current-roster persistent
rounds, full-history late joins, active-plus-draining capacity, owner/peer wakeups,
held HTTP and already-running native-command drain, skipped unstarted side
effects, and unfinished same-task/roster/checkpoint recovery. A real SQLite
checkpoint-trigger fault after a native command proves same-ID automatic
supervisor restart with repaired pending-tool history and no replay of that
command's observed side effect. This is crash-recovery evidence, not an
exactly-once guarantee for arbitrary filesystem or subprocess effects.

### Platform and operator evidence received before final freeze

- Native PTY tests passed **7/7 on Windows ConPTY** and **8/8 on Ubuntu Unix
  PTYs**, followed by a passing targeted blocked-input/explicit-kill case on each
  platform (**eight Windows and nine Unix cases** in the final suite). Tests use
  the real OS backends. Coverage includes terminal handles,
  immediate large input without startup deadlock, interaction/resize, complete
  disk logs with bounded pages, reversible split-Unicode bytes, shared registry
  and board freshness, process-capacity feedback, ordinary-command queueing,
  explicit cleanup, and last-handle child reaping. The Unix-only case verifies
  cleanup of a background descendant retaining the slave after its shell leader
  exits. Explicit termination also releases a maximum-size input write to a child
  that never reads, without leaving its nested process running. Linux verification
  used an isolated Rust toolchain and target directory.
- The public `setup` ConPTY regression passed: remembered unfinished work resumed
  automatically without `--resume` or a new prompt, retained its **two durable
  members despite a saved launch count of one**, restored original task/history,
  and drained. Reopening the completed remembered home produced no new task or
  board rows. Both console children were reaped.
- An interim optimized authenticated-localhost console exercise completed two
  prompt rounds while staying open, retained the exact Unicode prompt, exercised
  model-menu cancellation and a thinking-default change as a control notice,
  then restored terminal state on idle exit. A separate actual-binary PTY tool
  chain spawned a child, resized to 36×100, wrote 15 exact Unicode input bytes,
  read to natural exit, listed and cleaned up the session, then voted normally.
  Its independent disk log contained `NATIVE_PTY_READY` and
  `NATIVE_PTY_ECHO:native-PTY-İ`.
- An interim optimized 500-agent zero-grace offline demo, launched from outside
  the checkout, reported **500 drained / 500 votes / 1,003 messages / 389 ms**.
  This interim result is superseded by the final optimized scripts and
  expanded-roster console evidence above.

## Codex LB live model discovery

The connection-first Codex LB flow was verified on Windows on 2026-10-03:

- `cargo test --all-targets -- --include-ignored`: **135 passed, 0 failed, 0 ignored** — 101 library tests, 7 CLI-configuration unit tests, and 27 integration tests.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy --all-targets -- -D warnings`: passed without warnings.
- `cargo build --release`: passed in **32.03 seconds**; `target/release/openraid.exe` includes live discovery and OpenCode-configured limit overrides.
- New regression checks cover authenticated `GET /v1/models` without a `--refresh` flag, arbitrary server-only model IDs, server-declared thinking levels and limits, missing metadata remaining unknown, stale capabilities not surviving a fresh response, unauthorized discovery without credential leakage or a guessed model list, and rejection of an unadvertised model before database creation.
- A real ConPTY walkthrough against an authenticated localhost fixture confirmed endpoint/key entry before model selection, all eight fixture-supplied model IDs (including `gpt-6.1-sol`, `gpt-6-astra`, and `gpt-reserve`), API-advertised `low`/`high` thinking choices, and back navigation. No model workload was launched. This exercises the live API path with a fixture, not a paid Codex LB account.
- Additional regressions verify global OpenCode provider import, project-over-global precedence, configured context/output limits surviving discovery, configured but unserved models staying out of the live list, and Codex context defaults respecting explicit run budgets.
- The final release command `openraid models codex-pool` successfully fetched the actual configured server's model API using existing OpenCode provider settings. It returned ten models, including all eight IDs supplied by the user plus `gpt-5.5` and `codex-auto-review`. Configured models retained their 372,000-token context and 65,536-token output limits; the other models advertised 272,000-token context. Thinking levels were taken from the response, including model-specific `max`/`ultra` choices. No completion request was made and no credentials were printed or copied into the repository.

Codex LB now has no bundled OpenAI fallback model list. Discovery errors offer retry/connection editing rather than displaying guessed models. The earlier fallback-based walkthroughs below describe the previous implementation and are superseded by this flow.

## Provider and launcher expansion: initial automated test gate

The expanded implementation was tested on Windows on 2026-10-03:

- `cargo test --all-targets -- --include-ignored`: **128 passed, 0 failed, 0 ignored** — 99 library tests, 6 CLI-configuration unit tests, and 23 integration tests.
- Both optional Node-backed checks were included with Node.js and SDK bridge dependencies installed: real-adapter Rust integration and sidecar crash/restart/frame bounds.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy --all-targets -- -D warnings`: passed after the final OAuth lint correction.
- `npm test --prefix scripts`: **8 passed**, including real SDK requests, OAuth bearer routing, generation/header options, and sidecar request correlation.
- `node scripts/check-catalog-adapters.mjs`: **29 of 29 adapter imports passed** against the complete bundled catalog.
- Final same-scope acceptance checks passed after the unified run: **13 provider-protocol tests** covering native/SDK OAuth header precedence and Snowflake PAT/OAuth/generic controls, plus **5 thinking-variant tests** confirming native requests omit factory-only SDK settings.
- `cargo build --release`: passed on that native-settings snapshot (**32.29 seconds**). Its optimized artifact was `target/release/openraid.exe`; formatting and all-target warning-free Clippy checks also passed on that historical snapshot.

At that earlier snapshot, the catalog retained all **226 source providers and 8,385 raw models**. OpenCode-style experimental modes produced **8,481 normalized source model choices** after alias deduplication. The subsequently removed Codex fallback described by that initial gate is superseded by connection-first live discovery; the custom codex-lb entry remains.

The new coverage includes Responses encrypted reasoning replay and tool results; full codex-lb base-path routing; signed Anthropic thinking and Gemini thought signatures; trailing usage events; OpenRouter reasoning details; chat-compatible reasoning replay; model display/wire aliases; custom headers and config overrides; device-code login; shared OAuth refresh/persistence; Azure CLI token acquisition through an isolated command shim; GitLab and Snowflake refresh; account-scoped Snowflake HTTP normalization; and real SDK request multiplexing with out-of-order replies.

## Core-harness baseline checks

The core-harness baseline was verified on Windows on 2026-10-03 with rustc 1.96.0, before the provider-catalog and launcher expansion. Its release executable includes in-run checkpoint recovery, restored board cursors, consensus-drain ordering, and Turkish AltGr owner-input fixes.

- `cargo test --all-targets`: **49 passed, 0 failed** — 39 library tests and 10 integration tests.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy --all-targets -- -D warnings`: passed without warnings.
- `cargo build --release`: passed; optimized artifact is `target/release/openraid.exe`.

The suite covers 500 concurrent durable board writers; exact global cursor pagination; atomic owner/vote invalidation and fresh-quorum grace; shutdown publication before completion-marker notification; 50/100/500-agent harness runs followed by a smaller run against the same database; real localhost provider/tool/file/command integration; 429/5xx/interrupted-stream retries; bounded provider admission; context-overflow summary recovery; external peer/owner reconsideration; native command output spooling; Unicode/AltGr console input; and actual fresh-versus-recovered worker checkpoint behavior. Interrupted commands are represented as unknown effects for inspection, rather than automatically replayed.

## Baseline optimized offline measurements

Command:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/measure-scale.ps1 `
  -DatabaseDirectory "C:\Users\Arda\AppData\Local\Temp\opencode"
```

Each run used one release process, zero consensus grace, and a separate retained SQLite database. Every agent read the global handshake evidence, voted, and drained normally.

| Agents | Drained / votes | Harness elapsed | Whole process elapsed | Peak working set | Sampled CPU | Peak observed threads | Durable board rows |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 50 | 50 / 50 | 159 ms | 319.65 ms | 9.81 MiB | 31.25 ms | 14 | 103 |
| 100 | 100 / 100 | 112 ms | 278.88 ms | 11.05 MiB | 62.50 ms | 14 | 203 |
| 500 | 500 / 500 | 501 ms | 611.20 ms | 36.16 MiB | 968.75 ms | 14 | 1003 |

These are individual local observations, not averaged benchmarks or live-model throughput claims. CPU and thread values are sampled; CPU totals may exceed elapsed time because multiple runtime threads execute concurrently. Short-run startup/scheduling variation explains why the 100-agent run completed sooner than the 50-agent run. The script uses observation cadence without operation deadlines and retains unique database paths in its JSON output.

## Real operator-console exercise

A corrected interactive console run persisted the exact authenticated owner message:

```text
owner smoke 07: verify iii and İ before finishing coordinated native work
```

The durable record had sender `owner`, `owner=true`, and sequence 10. Help, focus/follow navigation, agent selection and detail drilldown were exercised. `q` restored the alternate screen and bracketed-paste terminal mode while the harness remained running, demonstrating display-only detachment. Native consensus then completed normally: 8 agents, 8 drained, 8 votes and 20 durable messages. The run used a 60-second consensus stability grace; that grace is not an operation timeout.

## Provider and launcher expansion: command/console smoke

This historical debug smoke predates connection-first Codex discovery. Its
bundled-fallback observations are superseded by the live discovery section above.
The expanded debug executable was exercised on Windows on 2026-10-03:

- Root help exposes `setup`, `providers`, `models`, and `auth`; `run --help` exposes provider/model/variant/protocol, custom JSON/JSONC config, provider options, headers, and guided-selection flags.
- `providers codex` finds the custom `codex-lb` entry. `models openai gpt-5.4` reports model limits and ordered effort variants; `models codex-lb gpt-5.6-sol` finds the release guide's example model in the fallback catalog.
- An unavailable thinking variant is rejected before launching agents.
- The guide's custom-provider JSONC example, including a display/wire model alias and custom thinking variant, parsed and configured successfully through the public `--config` command. A deliberately invalid agent count then stopped the run before any database or request was launched; the temporary fixture was removed.
- An actual ConPTY setup session filtered 228 launcher choices to the single `codex-lb` choice. `Enter` opened its model list with a clear bundled-model fallback when the local server was unavailable. Forward navigation and `Esc` back navigation worked. `Ctrl+C` cancelled with the expected exit code 1, and terminal restoration completed.
- The setup smoke used isolated temporary auth/data paths, saved no connection, and launched no model workload. Its PTY session was removed after exit.
- `node --check scripts/sdk-bridge.mjs` passed.

### Redesigned dashboard walkthrough

A fresh debug build ran an eight-agent offline demo in a real ConPTY console with isolated temporary database/auth paths. The walkthrough sent the command-palette shortcut, palette/help navigation, `Esc`, direct `1`/`2`/`3` focus keys, owner-composer open/cancel, and `q` detachment. The alternate-screen display cleared while the process continued headless. After the configured 180-second consensus grace, it exited successfully with:

```json
{
  "agents": 8,
  "finished_agents": 8,
  "votes": 8,
  "board_messages": 19,
  "elapsed_ms": 180118
}
```

The cancelled composer posted no owner message. A preceding eight-agent console run also completed successfully with 8 finished agents, 8 votes, and 19 board messages after its 60-second grace. Both PTY sessions were removed after exit; no browser or live-model request was used.

## Genuine limits

- The measured scale runs are offline orchestration exercises. Paid-provider quality, rate limits, cost and throughput were not benchmarked at 500 live agents.
- Context budgeting uses conservative heuristics. Configure the context budget for the actual endpoint window. An indivisible oversized history can visibly park with its checkpoint and full global board preserved; resume with suitable capacity. A summary request may also exceed an incorrectly configured endpoint window.
- Checkpoints do not make filesystem or subprocess side effects exactly-once. A crash before a durable result leaves an unknown outcome that must be inspected before retrying.
- Commands use the operator's OS privileges and are not sandboxed. The harness implements core tool/session/compaction mechanics and shared MCP tools/resources/prompts, not the complete OpenCode plugin/LSP/multimodal ecosystem.
- Requests and commands have no duration deadlines, so an active operation can delay final drain indefinitely.
