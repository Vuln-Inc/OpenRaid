# Optional desktop app

OpenRaid's desktop interface is a separate Tauri application with a React UI.
The terminal executable remains the default: building or running `openraid`
does not require Tauri, a WebView, or frontend Node dependencies.

The desktop host embeds OpenRaid's Rust/Tokio runtime. It does not launch the
CLI as a subprocess or implement a second swarm engine. Agent tools, model
transports, completion rules, native PTYs, and session controls remain in Rust.

## Requirements

Desktop source builds need the normal [Rust requirements](../README.md#requirements),
Rust **1.90 or newer** for the locked desktop dependency set, Node.js 22.12+
with npm, and Tauri 2's native platform prerequisites. This desktop toolchain
requirement does not change the terminal package's dependencies.

| Platform | Additional desktop requirements |
| --- | --- |
| Windows | MSVC C++ build tools and Microsoft Edge WebView2 Runtime |
| Linux | WebKitGTK 4.1, GTK 3, OpenSSL development packages, and desktop bundling libraries |
| macOS | Xcode Command Line Tools; the operating system provides WKWebView |

See the [Tauri prerequisite guide](https://v2.tauri.app/start/prerequisites/)
for current package names for your distribution. For example, Ubuntu/Debian
desktop builders commonly need `libwebkit2gtk-4.1-dev`, `build-essential`,
`curl`, `wget`, `file`, `libxdo-dev`, `libssl-dev`, `librsvg2-dev`, and
`libayatana-appindicator3-dev`.

These dependencies are **desktop-only**. Continue using `cargo build --release
--locked` at the checkout root to build the terminal app without them. Node
may still be needed by an optional SDK-backed provider or a configured MCP
server; that is independent of the desktop frontend.

## Build and launch

From the checkout root, install the frontend's locked dependencies and build
the separate desktop executable:

```sh
npm ci --prefix desktop
npm run tauri --prefix desktop -- build --no-bundle -- --locked
```

The opt-in helpers `build_desktop.bat` (Windows) and
`bash build_desktop.sh` (Linux/macOS) perform those steps and check the required
tools. They do not change the terminal build/run scripts.

| Platform | Default desktop executable |
| --- | --- |
| Windows | `desktop\src-tauri\target\release\openraid-desktop.exe` |
| Linux / macOS | `desktop/src-tauri/target/release/openraid-desktop` |

`CARGO_TARGET_DIR` overrides the target directory. The desktop has its own
Cargo manifest and lockfile; it is not a member of the terminal package's
workspace. The terminal executable remains `target/release/openraid` (or
`openraid.exe` on Windows).

To build platform installers/app bundles, omit `--no-bundle`:

```sh
npm run tauri --prefix desktop -- build -- --locked
```

Bundle outputs are under the desktop target's `release/bundle/` directory.
Build for each platform on that platform with the required native toolchain;
signing/notarization and distribution prerequisites are additional steps.
The release workflow packages the TUI and desktop executables together in each
platform archive. Windows uses `.zip`; Linux and macOS use `.tar.gz`. The desktop
frontend is embedded in its executable, so frontend sources and Node dependencies
are not shipped. Archives include desktop license notices and shared SDK runtime
companions. Installers/app bundles are not produced by the release workflow;
build those separately with the bundling command above. Existing published
archives are unchanged until a release-tag workflow run uploads new archives.

For local development:

```sh
npm run tauri --prefix desktop -- dev
```

Tauri starts Vite on port 1420 and opens the native application. Close the
application and stop the development process when finished. `npm run dev
--prefix desktop` by itself serves only the frontend; a normal browser has no
native Tauri command bridge and cannot start a swarm.

Frontend reloads reattach to the native host's active session without starting
another runtime. Rust source changes restart the native host in development;
after those rebuilds, reopen the saved session from the welcome screen.

## Shared sessions and credentials

Both interfaces use the same workspace configuration, credential store, and
SQLite session format:

- The workspace's first database is `.openraid/openraid.sqlite3`.
- Separate sessions have databases under `.openraid/sessions/`.
- Credentials/preferences use `openraid/auth.json` under `XDG_DATA_HOME`,
  or `~/.local/share` when unset. `OPENRAID_AUTH_FILE` overrides this location.
- Session catalog metadata lives beside that auth file in `sessions/`.
- Workspace configuration follows the normal `openraid.json`,
  `openraid.jsonc`, `opencode.json`, and `opencode.jsonc` lookup order.

See [configuration](../README.md#configuration-reference) for global overlays,
environment variables, provider setup, and custom database paths.

To hand a session between interfaces, stop or close the active interface first,
then open the same workspace and saved session in the other. Opening saved
history is not permission to replay unfinished commands automatically. Avoid
running two independent swarm runtimes against the same session at once:
SQLite sharing is not a cross-process worker-ownership lock.

## Find your way around

The desktop uses a Codex/ZCode-style neutral workspace shell: navigation and
agents live in the collapsible left sidebar, session controls in the quiet
top bar, and the model/reasoning shortcut in the rounded prompt composer.
Settings and saved sessions take the full content area. The built-in OpenRaid,
Dark and Light themes use neutral desktop colors; other runtime palettes stay
available. This does not change terminal theme colors.

On the welcome screen, enter the **Workspace folder** path and choose **Open workspace**.
Select **Use offline demo provider** for a credential-free demo. The session opens
without starting agents; the header shows its workspace, identity, and state.
Demo mode does not verify live credentials or paid-model execution. Do not
use provider/model changes to turn a demo into a real run. To use a real
provider, close and reopen the desktop, then open the workspace without the
demo option.

Opening a real workspace resolves its current provider configuration first.
If missing credentials prevent it from opening, connect that provider through
the [shared authentication flow](PROVIDERS.md#credentials-and-remembered-selections)
or its supported environment variables, then retry. Desktop provider guidance
does not replace every provider's first-run account setup.

- The **agent roster** shows worker status, usage, and completion votes. Select
  an agent to inspect it. Focus a list row and use arrow keys, Home/End, or
  PageUp/PageDown to reach entries beyond the visible virtualized window.
- **Board** shows the global discussion. Click a message to read its full text;
  **Load from beginning** and subsequent history pages retrieve earlier durable
  messages beyond the live preview.
- **Activity** shows the selected agent's live preview. **Load full transcript**
  explicitly retrieves retained generation, tool calls/results, and command or
  PTY output. A bounded retained-output tail updates for the selected agent
  when native events arrive; it is not fetched by a polling timer.
- **Prompts** shows owner instructions. **Use draft** copies a prompt into the
  composer without changing workspace files; **Restore** is a separate action
  that may restore a Git snapshot.
- Use the session, model, and appearance controls to browse available choices
  instead of entering slash commands, identifiers, or JSON. Search helps narrow
  large provider/model catalogs and saved-session lists. Review the workspace
  and session identity before opening history or starting work.
- **Sessions** lists saved history with titles, workspace paths, recency, and
  a current-session marker. Search by title, path, or identity, then open a
  saved session while idle. **New session** creates separate saved history;
  neither opening nor creating a session starts agents.
- **Settings** contains provider/model/variant selection, appearance previews,
  MCP connection controls, and board export. Retrying
  a failed MCP connection uses the same server control as enabling it; session
  toggles do not rewrite your configuration.
- The bottom composer starts an objective while idle and sends follow-up
  steering while work is active. Enter sends; Shift+Enter inserts a newline.
  Its model chip opens a searchable provider/model/reasoning popover without
  leaving the conversation. Ctrl/Cmd+N opens the new-session confirmation.

Provider connection status describes the credentials available to the native
runtime, not a successful live request. Missing-credential guidance names the
supported environment variables or shared authentication flow. Connect keys,
account/OAuth credentials, and specialized cloud configuration through the
existing shared flow, then refresh the catalog. Saved keys are never displayed
in the desktop. Credentials are configured through the shared authentication
flow; the desktop does not expose an API-key entry form. See
[provider authentication](PROVIDERS.md#credentials-and-remembered-selections).

The appearance picker shows named light/dark themes with palette previews and
marks the current selection. Search by name or by `light`/`dark`, then choose a
preview to apply it. The choice uses the same saved preference as the terminal;
it does not create a second desktop-only theme configuration.
The **Dark** and **Light** presets provide neutral charcoal/white and
white/black desktop appearances. Both belong to the shared catalog, so their
saved selection also works in the terminal, with its original terminal palette.

In **Provider & model**, type directly into the **Provider** or **Model**
selector to search its dropdown, or use the arrow to open the full list.
There are no separate search fields. Choose the model's **Reasoning variant**
the same way, then **Apply model selection**. The current applied selection stays
visible while you browse. **Default** removes the thinking override; it does
not disable reasoning. Models and variants are not interchangeable across
providers, and live changes take effect at safe worker boundaries.

Desktop controls use the MIT-licensed **Untitled UI React** components:
buttons, inputs, searchable selectors, number fields, checkboxes, textareas,
badges, tabs and modal dialogs. Dropdowns support arrow keys, Enter and Escape;
modal dialogs trap focus and return it to the trigger when dismissed. Shared
theme colors also apply to dropdowns and dialogs rendered outside the app root.
Session-open, export and prompt-restore confirmations use dismissible Sonner
toasts without shifting the workspace layout. Repeated transient errors are
coalesced; persistent runtime errors remain visible in the workspace.
Vendored component sources, attribution and local adaptations are documented
in [`desktop/src/components/README.md`](../desktop/src/components/README.md).

## Operating safely

The session state is **IDLE**, **RUNNING**, **PAUSED**, or **STOPPING**.

- **Start swarm** submits an objective to the current session.
- **Pause** lets admitted operations finish and parks workers at safe boundaries;
  **Resume** continues the same work.
- **Stop run** cancels current requests and tool operations. It does not undo files
  already changed or mark the objective successfully completed.
- **New session** creates fresh history. Switch or create sessions only when idle and
  fully drained.
- **Add/remove** changes the shared roster. Active plus draining workers cannot
  exceed 500; at least one active member must remain.
- Model and thinking-variant changes take effect at safe worker boundaries.
- Prompt restoration is an explicit idle-only action. A Git-backed snapshot can
  restore workspace files; inspect the chosen prompt carefully before restoring.
  Without a snapshot, only the prompt draft is restored. Restoration does not
  resend the prompt or delete its audit history.
- Board exports preserve history without overwriting an existing destination.
  The output is formatted JSON; relative export paths resolve inside the
  current workspace.

Closing the native desktop window stops active work, then drains the runtime
and flushes the database. This is different from the terminal guided console's
graceful detach behavior: explicitly canceled work is not automatically resumed
on reopening. For a deliberate immediate stop, use **Stop** before closing.

The swarm has the same OS privileges in either UI. Opening a workspace is not
a sandbox: agents can run native commands, and configured MCP servers may
expose additional tools. Only use workspaces and credentials you intend the
swarm to access.

## Live data and scale

The native host pushes live state through Tauri events. React does not poll
the runtime for updates. On opening the UI, an initial snapshot establishes
the session state; later events update the board, roster, votes, and activity.
Agent selection requests retained generation, tool calls/results, and command
or PTY output for that identity.

Board, prompt, and agent lists use virtualized rendering so a 500-agent
roster does not create hundreds of full transcripts in the DOM. Selected-agent
retained transcripts are loaded explicitly rather than repeatedly fetched on
every token event, and their displayed lines are virtualized. The live selected
tail is bounded, but explicitly requesting a full transcript still transfers
the retained text. Virtualization is a display optimization, not a separate
board or an agent ownership system; it does not bound provider or IPC work.
Paid-provider throughput still depends on provider quotas, model latency,
configured request limits, tool capacity, and the machine's resources.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| Windows window fails to open | Install/update the WebView2 Runtime and check MSVC build prerequisites |
| Linux build cannot find WebKitGTK | Install WebKitGTK **4.1** development packages and the distribution's Tauri prerequisites |
| macOS compilation cannot find developer tools | Run `xcode-select --install` and select a valid developer tools installation |
| Desktop has no provider credential | Check that its environment/home directory resolves the same auth file as the terminal app |
| Wrong session or workspace | Verify the visible workspace and session ID before starting work |
| SDK-backed provider fails | Install the optional SDK dependencies separately; see [the provider guide](PROVIDERS.md#native-transports-and-the-sdk-bridge) |
| Stopping does not restore files | Stop cancels ongoing work; it is not a filesystem rollback |

Desktop packaging/signing, provider credentials, and actual native platform
acceptance are separate checks. A frontend production build alone does not
demonstrate that a Tauri binary starts successfully on another operating system.

## Verification and source layout

Keep the terminal checks independent:

```sh
cargo test --locked --all-targets -- --include-ignored
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
```

On a machine with the desktop prerequisites installed, also run:

```sh
npm ci --prefix desktop
npm test --prefix desktop
npm run build --prefix desktop
python scripts/test-package-release.py
cargo test --locked --manifest-path desktop/src-tauri/Cargo.toml
cargo fmt --manifest-path desktop/src-tauri/Cargo.toml -- --check
cargo clippy --locked --manifest-path desktop/src-tauri/Cargo.toml --all-targets -- -D warnings
npm run tauri --prefix desktop -- build --no-bundle -- --locked
```

The single `release.yml` workflow verifies the terminal dependency graph before
installing desktop/WebView prerequisites, then checks and builds both executables
on each native platform. Packaging tests verify combined archives and checksums;
matching TUI/desktop versions are required. Publication waits for all platform
jobs. Consult actual workflow results before claiming platform acceptance.

Before distributing a desktop build, perform a native walkthrough as well:

1. Open a temporary workspace with an offline/demo configuration. Confirm it
   opens idle and does not replay a saved objective.
2. Start a small swarm. Inspect board ordering, completion votes, generation,
   tool arguments/results, and command/PTY output. Browse older retained
   content rather than checking only the latest preview.
3. Pause/resume, send follow-up steering, change a model/variant, add agents,
   remove an agent, and stop. Confirm control failures are visible and the UI
   remains usable during a slow provider/MCP operation.
4. Create and switch sessions while idle. Open a terminal-created session in
   the desktop and then a desktop-created session in the terminal. Check
   workspace, session identity, roster, prompt history, and database paths.
5. Export the board. Verify that exporting to an existing file is rejected.
   In a disposable Git workspace, inspect and explicitly confirm a prompt
   restore; verify it does not automatically resend the objective.
6. Exercise a 500-agent offline roster and inspect an agent while scrolling
   the board. Check DOM/rendering bounds and memory as well as subjective
   responsiveness; this does not establish paid-provider throughput.
7. Close the native window during active work. Check runtime/child-process
   cleanup, then reopen and inspect durable history before restarting.
8. Try both light and dark themes, resize the window, and navigate controls
   using the keyboard. Check visible focus, readable labels, long paths/names,
   current selections, and empty or failed search results. Confirm that model
   choices follow the provider and variant choices follow the selected model.
9. Inspect a provider without credentials. Its status and connection guidance
   should be understandable without exposing a saved key. A configured provider
   is not proof that the account can access every model in its catalog.

Use disposable workspaces for destructive/restore checks. Credentials and
provider account access need separate live checks; an offline pass cannot
establish them.

Record the environment and evidence for each kind of verification separately:

| Verification | What it establishes | What it does not establish |
| --- | --- | --- |
| Frontend tests and production build | Tested UI/data behavior and TypeScript/bundle correctness | Native window startup, IPC, or runtime execution |
| Browser walkthrough with a mocked bridge | Rendered layouts, selectors, feedback, keyboard interaction, and synthetic 500-agent rendering | Real Tauri events, credentials, SQLite interoperability, or native tool cancellation |
| Native desktop walkthrough | Actual WebView/IPC integration and the runtime flows exercised on that OS | Acceptance on other operating systems or untested live providers |

A normal Vite browser tab cannot run native session controls. If a test supplies
a mocked bridge, label its results as mocked frontend evidence, not a native
integration pass. Linux and macOS checks require their own native environments;
a Windows build or screenshots alone do not verify them.

| Path | Responsibility |
| --- | --- |
| `src/desktop.rs` | Shared Rust desktop facade over the existing runtime and session controls |
| `desktop/src/` | React views, state/event bridge, and virtualized lists |
| `desktop/src-tauri/` | Optional Tauri host, native command boundary, events, capabilities, and packaging |
| `build_desktop.sh`, `build_desktop.bat` | Opt-in desktop build helpers |
| `.github/workflows/release.yml` | Shared TUI/desktop verification, platform archives, and release publication |
| `scripts/package-release.py`, `scripts/test-package-release.py` | Combined executable packaging and archive/version regression tests |

Credentials and provider secrets must remain in native code, not in snapshots
sent to React. The desktop WebView has no general-purpose shell or filesystem
plugin permission. Native commands are exposed through the host's typed
application-specific command boundary. This limits the UI bridge; it does not
reduce the OS privileges of agent tools invoked by the Rust runtime.
