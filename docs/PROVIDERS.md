# Providers, models, and thinking variants

OpenRaid separates three choices: the **provider** supplies the endpoint and authentication, the **model** supplies capabilities and token limits, and the **thinking variant** supplies model-specific reasoning options. A model ID belongs to its provider; a variant belongs to its model.

## Select interactively

```sh
cargo run --release -- setup
```

The launcher lets you search providers, select a model, and choose that model's thinking variant before starting the swarm. Typing filters the current list; use the arrow keys to select and `Enter` to continue. `Esc` returns to the previous step, `Ctrl+U` clears the current search or input, and `Ctrl+C` cancels setup. API-key entry is masked. In multiline objective input, `Shift+Enter` or `Ctrl+J` inserts a newline.

Standard providers use their catalog or configured endpoints automatically: choose a provider, model, and variant, then connect with your key or supported account. You do not need to type an API URL. Explicit `--base-url` and configuration overrides still apply. Endpoint entry is reserved for `codex-lb` and a custom provider with no configured endpoint.

The launch review shows the connection, model, thinking choice, output reserve, objective, and agent count. You can edit the objective or agent count there; agents start only after you choose **Launch swarm**.

For Codex LB, the launcher asks for the endpoint and API key **before** model selection. It fetches the model list from that server and shows its advertised thinking levels. If discovery fails, you can retry or edit the connection; it does not substitute a bundled OpenAI model list.

An imported provider named `codex-pool` uses its configured endpoint automatically, asks for a key when needed, then discovers models before thinking selection. Select that provider to use its endpoint, credentials, and model overrides from your OpenCode configuration.

`run --select` opens the launcher with the other run arguments as starting values. Interactive setup requires an actual terminal; use explicit flags for scripts and redirected input/output.

Guided launches keep an interactive session open after a batch reaches consensus. The saved launch profile remembers its workspace, database, swarm size, model settings, and explicit configuration path. A later `setup` opens saved history at an idle home screen, including interrupted unfinished tasks. Reopening or switching to a session does not start agents. Use `/start your objective` to begin work or `/new` to create fresh history; `setup --resume` explicitly continues an interrupted task with its durable roster and checkpoints. Completed historical objectives are not replayed. `run --select` starts the selection flow again; noninteractive runs exit after their work drains.

## Live session controls

Press `/` for the searchable command menu, or use these controls while the dashboard is open:

| Command | Shortcut | Behavior |
| --- | --- | --- |
| `/models` | `Ctrl+X`, then `m`; `F2` | Search tool-capable models across connected providers and switch the active selection |
| `/connect` | `Ctrl+X`, then `c`; `F3` | Connect a catalog provider with a masked API-key prompt; Codex LB also prompts for its endpoint |
| `/variant` | `Ctrl+X`, then `t`; `F4` | Choose thinking depth for the current model; `Ctrl+T` cycles choices |
| `/mcp` | Slash menu | Inspect configured MCP servers; enable, disable, or retry a connection |
| `/jump` | `Ctrl+X`, then `j`; `F5` | Search sent prompts and jump to their board position |
| `/agents` | `Ctrl+X`, then `a`; `F6` | Toggle a paged tiled view of the swarm |
| `/members` | `F7` | Manage the current parallel-agent roster |
| `/add [count]` | `Ctrl+X`, then `+`; `F8` | Add a batch of collaborators; omitted count adds one |
| `/remove [IDs]` | `Ctrl+X`, then `-`; `F9` | Gracefully retire a batch; no IDs opens the roster selector |

Direct commands can also be entered in the owner composer, such as `/models openai/gpt-5.4` or `/variant high`. `/variant default` removes the selected thinking override. Normal prompt text goes to the shared board and revokes stale completion votes. Use `o` to compose, `Enter` to send, and `Shift+Enter` for a newline.

Model changes preserve workspace and board history. Workers apply the new selection at safe boundaries after active requests and tool operations finish. Available model and variant choices are refreshed from the live API for connected Codex providers. The selected launch profile is remembered for the next session.

For example, `/add 5` adds five collaborators and `/remove agent-002 agent-003` retires two. A late joiner receives the current objective and full global board. Membership changes append durable global join/removal notices, revoke stale votes, and recalculate the 75% quorum from the active roster. Removed workers disappear from the live roster on the next redraw while finishing their current request or tool operation and checkpointing before drain; their identities are not reused, and historical usage/activity remains accounted for. The header distinguishes active members from workers still draining. At least one active member must remain; active plus still-draining workers cannot exceed 500. Changes are rejected during a committed round drain and become available again at the persistent idle home.

An active worker that unexpectedly errors, panics, or exits restarts automatically under the same identity and restores its durable context. Recovery repairs incomplete tool groups without replaying side effects whose outcome is unknown. Explicitly removed workers remain retired. `--resume` restores an interrupted durable session when launching explicitly; remembered interactive setup stays idle until the operator starts work.

Click a sent owner prompt to copy its complete text, jump to it, or restore it into the editor. Restore is available when the swarm is idle. In a Git workspace, OpenRaid captures a pre-prompt working-tree snapshot and can restore it along with the prompt; Git HEAD and the operator's index are preserved. If no snapshot is available, only the prompt text is restored. Previous board entries remain as an audit record, and restore does not automatically resend the prompt.

## Select from the command line

Browse providers and narrow a model list:

```sh
cargo run --release -- providers
cargo run --release -- providers anthropic
cargo run --release -- models openai gpt-5
cargo run --release -- models anthropic sonnet
```

Add `--json` to provider or model listing commands for machine-readable metadata, including model variants. To run, supply the provider ID and a model ID from that provider's list:

```sh
cargo run --release -- run 'Implement and verify the objective' --provider openai --model gpt-5.4 --variant high --agents 8
```

Qualified model identifiers are also accepted: `--model openai/gpt-5.4` selects that provider/model pair. Provider-native model IDs that themselves contain `/` remain usable with `--provider`.

For a headless run, append `--no-tui`. Run settings can also come from `OPENRAID_PROVIDER`, `OPENRAID_MODEL`, `OPENRAID_VARIANT`, and `OPENRAID_BASE_URL`; explicit flags override those variables.

Connect a provider with a masked key prompt, inspect connections, or remove an OpenRaid-saved key:

```sh
cargo run --release -- auth connect anthropic
cargo run --release -- auth list
cargo run --release -- auth disconnect anthropic
```

Disconnecting removes the OpenRaid-saved key. Environment credentials and the OpenCode fallback remain usable if present.

### Custom endpoints and request options

`--base-url` overrides the endpoint. `--protocol` chooses `chat`, `responses`, `anthropic`, `gemini`, or `sdk`. `--provider-options` accepts a JSON object using OpenCode SDK option names, and repeatable `--header NAME=VALUE` arguments add endpoint-specific headers.

For a model not present in the catalog, supply an explicit endpoint and protocol. Use provider options for its reasoning settings, since it has no catalog variants:

```sh
cargo run --release -- run 'Implement the objective' --provider my-api --model my-model --base-url https://api.example.com/v1 --protocol responses --provider-options '{"reasoningEffort":"high"}' --no-tui
```

Use `OPENRAID_API_KEY` for an explicit key override, or save a key with `auth connect my-api`. The example URL and model are placeholders to replace with your server's values.

### Custom provider configuration

OpenRaid first imports provider definitions from `opencode/opencode.json` and `opencode/opencode.jsonc` under `XDG_CONFIG_HOME`, or `~/.config` when it is unset (`USERPROFILE` is supported on Windows). It reads the `provider` section and supported MCP settings without modifying those files or copying credentials into the repository.

`run` and `setup` accept `--config path/to/providers.jsonc`. Without an explicit file, OpenRaid checks the selected workspace for `openraid.json`, `openraid.jsonc`, `opencode.json`, then `opencode.jsonc`, using the first file present. Workspace or explicit provider settings override global settings. The explicit file remains available to live model menus and remembered launches. Provider definitions use OpenCode's `provider` shape; importing them does not import its plugins or unrelated application settings.

For example, save this as `openraid.json` and replace the endpoint and wire model ID with your server's values:

```json
{
  "provider": {
    "my-api": {
      "name": "My inference server",
      "npm": "@ai-sdk/openai",
      "options": { "baseURL": "https://api.example.com/v1" },
      "models": {
        "my-model": {
          "id": "server-model-id",
          "name": "My coding model",
          "reasoning": true,
          "tool_call": true,
          "limit": { "context": 128000, "output": 16384 },
          "variants": {
            "focused": { "reasoningEffort": "high" }
          }
        }
      }
    }
  }
}
```

Then connect and launch:

```sh
cargo run --release -- auth connect my-api
cargo run --release -- run 'Implement the objective' --provider my-api --model my-model --variant focused --protocol responses
```

The displayed model key is `my-model`; the wire request uses `server-model-id`. `@ai-sdk/openai` supports Responses, while `@ai-sdk/openai-compatible` denotes Chat Completions. An explicit `--protocol` selects the desired protocol. Model variant overrides merge with derived variants; `"disabled": true` removes a variant from selection.

Provider `options` supply factory settings, including `baseURL` and an optional `apiKey`; model `options` supply generation defaults. Model defaults merge with the selected variant, then explicit `--provider-options` overrides the result. Explicit `--header` values override configured headers and generated protocol defaults; an active OAuth session's refreshed authorization remains authoritative.

Configuration string values can use `{env:VARIABLE_NAME}` or `{file:path/to/value.txt}` references. File references are trimmed text values; relative paths resolve from the configuration file's parent directory, and `~/` is supported. Endpoint templates such as cloud account/project/region placeholders are resolved from SDK settings or the corresponding provider environment variables.

## Native transports and the SDK bridge

Standard Chat Completions, Responses, Anthropic Messages, and Gemini requests run through the shared Rust HTTP client. Providers with specialized protocols or cloud credential chains use an optional AI SDK bridge. The catalog chooses the adapter per model, including model-level provider overrides.

For SDK-backed providers, install **Node.js 22.12 or newer** and the bridge dependencies from the repository root:

```sh
npm ci --prefix scripts
```

The bridge uses the pinned versions in `scripts/package-lock.json`. Native transports do not need Node.js. Specialized providers still require their own connection settings, such as the appropriate cloud region, resource, deployment, or credential chain.

Pass SDK factory settings in the `sdkSettings` object within provider options. For example, append this argument to a Bedrock run to select its region:

```sh
--provider-options '{"sdkSettings":{"region":"us-west-2"}}'
```

Other model options remain alongside `sdkSettings` and are passed to generation. The bridge removes `sdkSettings` from generation options before constructing the provider.

The bridge only generates model responses and tool calls. OpenRaid continues to execute workspace tools, coordinate agents, and persist checkpoints in Rust. SDK results are returned after generation finishes rather than streamed token by token. SDK requests share one lazily started Node.js sidecar with request-ID multiplexing and the provider concurrency limit; the process and imported adapters are reused across agents. The bridge restarts after a sidecar exit.

GitLab workflow discovery uses the selected workspace's GitLab project to augment its static model list. `models gitlab` and connected live menus attempt discovery; `models gitlab --refresh` surfaces discovery errors. Configured and static choices remain available if optional discovery fails. Discovered choices retain workflow references, context/output limits, and tool capabilities. This discovery uses the Node.js sidecar and supports API-token and OAuth authentication.

SDK fetches use a shared pooled transport with header, body, and connection deadlines disabled. The same policy applies to discovery fetches and overrides built-in per-dispatch timeout defaults; explicit lifecycle cancellation remains separate from elapsed time.

When deploying the executable away from the repository, keep `sdk-bridge.mjs`, `http-transport.mjs`, `package.json`, `package-lock.json`, and installed `node_modules` together in a scripts directory. Set `OPENRAID_SDK_BRIDGE_DIR` to that directory. `OPENRAID_NODE` can override the Node.js executable. Script discovery also checks `scripts` in the current directory, a `scripts` directory beside the executable, and the source checkout used to build it.

## Credentials and remembered selections

API-key resolution uses this order:

1. An explicit `--api-key` value or `OPENRAID_API_KEY` override.
2. A configured provider `options.apiKey` value.
3. The selected provider's authentication environment variables.
4. An API key saved in OpenRaid.
5. An API-key entry in OpenCode's `auth.json`.

Cloud project IDs, regions, credential-file paths, and AWS signing-chain variables configure their SDK credential chains; they are not treated as bearer API keys.

OpenRaid stores credentials, connected endpoint overrides, the last provider/model/variant selection, and launch preferences in `openraid/auth.json` under `XDG_DATA_HOME`, or under `~/.local/share` when `XDG_DATA_HOME` is unset. The previous objective and session-only API-key field are not saved in the launch profile. On Windows, the home fallback uses `USERPROFILE` if `HOME` is unavailable. Set `OPENRAID_AUTH_FILE` to use a different OpenRaid credentials file.

The OpenCode fallback is read from `opencode/auth.json` under the same data directory. Imported OAuth entries are distinguished from API keys. An environment or OpenRaid API key for the same provider takes precedence over that fallback.

Saving a selection remembers its identifiers; it does not establish access to a model. Model access remains controlled by the provider or proxy.

### ChatGPT and GitHub Copilot account login

OpenRaid supports device-code account login for **OpenAI/ChatGPT Codex**, **GitHub Copilot**, and **GitHub Copilot Enterprise**:

```sh
cargo run --release -- auth login openai
cargo run --release -- auth login github-copilot
cargo run --release -- auth login github-copilot-enterprise --enterprise-url company.ghe.com
```

The command displays a verification URL and code. Open that URL in your browser, approve the account connection, and leave OpenRaid running until login completes. `Ctrl+C` cancels the wait. The guided launcher offers the same account-sign-in choice during connection setup.

Existing OpenCode OAuth accounts can also be reused through the auth-file fallback without another login. For Codex account authentication, OpenRaid uses the Codex Responses endpoint and account-routing metadata. Copilot selects its protocol by model family, including native Anthropic Messages for Claude models.

Codex access tokens are refreshed before provider requests when needed. Agents share the refresh lock, and refreshed credentials are saved in the OpenRaid credentials file without rewriting OpenCode's original file. Run `auth login openai` again if the account is revoked or its refresh token is no longer valid; Copilot accounts can similarly reconnect with their `auth login` command.

### Imported Azure, GitLab, and Snowflake accounts

OpenCode account entries can also supply these provider-specific connection paths:

| Provider | Account requirements and behavior |
| --- | --- |
| Azure | An imported resource/account marker plus an authenticated Azure CLI session. OpenRaid obtains a fresh bearer token from `az account get-access-token` and caches it with expiry-aware refresh; imported placeholder tokens are not sent as API keys. `OPENRAID_AZ` can select the Azure CLI executable. SDK-backed requests require the bridge dependencies. |
| GitLab | An imported GitLab OAuth account with its instance URL. Access-token refresh uses the instance's OAuth endpoint; the SDK receives OAuth authentication settings. `GITLAB_OAUTH_CLIENT_ID` can select a custom OAuth client. |
| Snowflake Cortex | An imported Snowflake OAuth account with account ID and refresh credentials. OpenRaid refreshes the access token when needed and uses bearer authentication at the account's Cortex endpoint. |

Other OAuth credential types need their own provider-specific integration and produce an actionable error. API-key and SDK cloud-credential paths remain available for the corresponding providers.

## MCP servers

Configure servers in the same global, workspace, or explicit JSON/JSONC file used for providers. OpenRaid accepts an OpenCode-shaped `mcp` object or the `mcpServers` alias. For example:

```json
{
  "mcp": {
    "local-tools": {
      "type": "local",
      "command": ["node", "path/to/server.mjs"],
      "environment": { "SERVICE_TOKEN": "{env:SERVICE_TOKEN}" },
      "enabled": true
    },
    "remote-tools": {
      "type": "remote",
      "url": "https://mcp.example.com/mcp",
      "headers": { "Authorization": "Bearer {env:SERVICE_TOKEN}" },
      "enabled": false
    }
  }
}
```

Replace the command and URL with your server's actual values. Local servers communicate over stdio and start in the selected workspace; remote servers use Streamable HTTP. `environment` (or `env`) configures local subprocess variables; `headers` configures remote request headers. String values support the same `{env:...}` and `{file:...}` references as provider configuration.

Workspace definitions merge with inherited servers. For example, `"mcp": { "local-tools": { "enabled": false } }` disables that inherited server without repeating its command or credentials. An entry set to `false` removes the inherited server entirely. Failed or closed connections can be retried through `/mcp`; reconnecting replaces the old connection and refreshes its available tools.

One connection and local subprocess are shared across the swarm for each server. Enabled servers initialize in the background so a slow server does not block provider workers from starting; ready tools, resources, and prompts become native agent-callable definitions. `/mcp` displays connection status and lets you disable a preparing or connected server, enable a disabled server, or retry a failed connection. Explicit disable and final session shutdown cancel pending initialization and clean up its transport or child process. Dashboard toggles apply to the current session; edit the configuration for a persistent default.

## Thinking variants

Variants are derived per provider/model pair using OpenCode-style option names. They are not a universal list of effort levels:

- OpenAI-family models may expose effort choices such as `low`, `medium`, `high`, and `xhigh`, depending on their model family and release.
- Anthropic models use either a thinking-token budget or adaptive thinking with an effort setting, depending on the model.
- Gemini models use model-specific thinking budgets or thinking levels.
- A model with no advertised variants uses its provider defaults.

**Default** means that OpenRaid does not add a selected variant override. It does not mean that reasoning is disabled. A model that supports an explicit `none` variant can expose that separately.

Thinking consumes part of the model's output allowance. Built-in Anthropic budget variants are derived against the effective run output reserve. Explicit custom thinking budgets may require a larger output allowance and context budget; the launcher validates the resulting reservation before starting. Model token limits shown in the catalog describe the model's capacity. OpenRaid caps effective run context/output reservations to advertised model limits and keeps output below the effective context budget; smaller explicit run budgets still apply.

## Catalog updates

The bundled offline catalog is derived from the same [models.dev](https://models.dev/) metadata source used by OpenCode. It retains provider identifiers, model identifiers, authentication environment-variable names, adapter names, reasoning/tool capabilities, token limits, and cost metadata.

Refresh the bundled snapshot from the repository root:

```sh
python scripts/update-catalog.py
cargo build --release
```

The snapshot is embedded in the executable, so rebuild after updating it. Refreshing metadata does not connect accounts or change server-side model access. A provider's catalog entry identifies its adapter requirements; selecting an entry also requires an available transport and suitable credentials.

See [the source provider audit](SOURCE_PROVIDER_AUDIT.md) for the pinned OpenCode loader inventory, behavioral checks, adapter-construction coverage, and provider-specific routing/settings evidence.

Models that advertise experimental modes can appear as additional mode-suffixed selections. These keep the original upstream model ID while applying the mode's generation options, headers, and cost metadata. Selectable counts can therefore exceed the source snapshot's model count.

## codex-lb v1.24.0

[codex-lb](https://github.com/Soju06/codex-lb/releases/tag/v1.24.0) pools ChatGPT accounts behind a local or remote API. OpenRaid connects to that API; account login and account-pool management happen in the codex-lb dashboard.

### Start the server

To run the requested version with `uvx`:

```sh
uvx --from codex-lb==1.24.0 codex-lb
```

Or use the versioned Docker image:

```sh
docker volume create codex-lb-data
docker run -d --name codex-lb -p 2455:2455 -p 1455:1455 -v codex-lb-data:/var/lib/codex-lb ghcr.io/soju06/codex-lb:1.24.0
```

Open `http://127.0.0.1:2455`, add an account, and create an API key if your deployment enables API-key authentication. Remote clients require a dashboard-issued API key.

### Endpoint and reasoning

The OpenAI-compatible endpoint is **`http://127.0.0.1:2455/v1`**. The Codex CLI endpoint is **`http://127.0.0.1:2455/backend-api/codex`**. Supply the base URL, without appending `/responses` yourself.

Use the **Responses** protocol for Codex models. codex-lb's tagged client guide recommends Responses because it preserves multi-turn reasoning state, including encrypted reasoning items. Chat Completions is a separate compatibility interface.

Model availability depends on the account pool and upstream rollout. OpenRaid obtains Codex LB models from the configured server's live `/models` API rather than a hardcoded model list. Model IDs, advertised thinking levels, and token limits come from that response; unavailable metadata is not inferred from unrelated OpenAI models.

Explicit per-model configuration, such as `provider.codex-pool.models.MODEL.limit.context` and `.limit.output`, overrides discovered limits. These settings survive a refresh, but models absent from the API response are not added to the picker. For `codex-lb` and `codex-pool`, the effective model context limit becomes the default run context budget. An explicit `--context-budget` keeps a smaller budget; output reservation remains controlled by `--max-output-tokens` and capped at the model's output limit.

To inspect the models currently served by your deployment:

```sh
cargo run --release -- models codex-lb --base-url http://127.0.0.1:2455/v1
```

If authentication is enabled, set `CODEX_LB_API_KEY` first:

```sh
export CODEX_LB_API_KEY='your-dashboard-key'
```

```powershell
$env:CODEX_LB_API_KEY = 'your-dashboard-key'
```

Choose **Codex LB (custom Codex API)** in `setup`. For an explicit Responses run with a model returned by your server, use:

```sh
cargo run --release -- run 'Implement and verify the objective' --provider codex-lb --model MODEL_FROM_SERVER --base-url http://127.0.0.1:2455/v1 --protocol responses --agents 8
```

Replace `MODEL_FROM_SERVER` with an exact model ID returned by your server. Codex LB listing commands fetch live models automatically, so `--refresh` is optional. Headless runs also discover models before launch and reject IDs absent from the server's response. Use `--variant` to select an advertised thinking level; when no levels are advertised, the picker offers the provider default.

Reasoning effort also depends on the model. Select a variant advertised for that model rather than assuming every model accepts `xhigh`. codex-lb can further constrain reasoning effort with per-key policies.

### Troubleshooting

| Symptom | Check |
| --- | --- |
| Connection refused | codex-lb is running and port `2455` is reachable from the machine running OpenRaid |
| HTTP 401 / 403 | The API key was created in the codex-lb dashboard and matches the server's authentication settings |
| Model not found | The model is available in the server's live `/v1/models` response and allowed by the selected API key |
| Reasoning option rejected | The selected model and key policy support that effort |
| HTTP 404 | The configured URL is the API base, normally `/v1`, rather than the dashboard root or a duplicated `/responses` path |

## Reference sources

- [OpenCode providers](https://opencode.ai/docs/providers/) — provider connections and custom endpoint configuration.
- [OpenCode models](https://opencode.ai/docs/models/) — provider/model identifiers and model-specific variants.
- [models.dev](https://models.dev/) — provider and model capability metadata used by OpenCode.
- [codex-lb v1.24.0 client setup](https://github.com/Soju06/codex-lb/blob/v1.24.0/docs/client-setup.md) — endpoint, model-discovery, authentication, and Responses recommendations for the requested release.
