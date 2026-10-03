# OpenCode source-provider behavioral audit

Reviewed source: [`packages/opencode/src/provider/provider.ts`](https://github.com/anomalyco/opencode/blob/35fc7a776cddb72d334ee60590c3cce41154be05/packages/opencode/src/provider/provider.ts), commit **`35fc7a776cddb72d334ee60590c3cce41154be05`** (last changed September 28, 2026). Audit performed October 3, 2026.

The reviewed source has **23 custom loaders and 24 bundled adapter entries**. The models.dev snapshot additionally uses **29 installed catalog SDK packages**, excluding the native Copilot adapter. Importing those packages alone does not establish loader parity.

## Executable evidence

```sh
npm test --prefix scripts
node scripts/check-catalog-adapters.mjs
node scripts/check-opencode-providers.mjs
cargo test --locked --all-targets -- --include-ignored
```

The source check fetches both the current upstream file and the pinned review revision, rejects unreviewed differences, and executes the actual upstream custom-loader functions with isolated authentication/environment/configuration fixtures. It checks all 23 loader contracts, compares **56 Bedrock region/model combinations** with OpenRaid, checks six response/chat selectors, and constructs real generation-capable models for every nonnative bundled adapter. Source evaluation uses Node's TypeScript stripping API and requires **Node.js 22.13+**; the runtime bridge itself requires 22.12+.

`scripts/provider-loaders.test.mjs` verifies endpoint/credential precedence, adapter dispatch, Vertex regional defaults, GitLab workflow/discovery metadata, Cloudflare gateway routes/cache/authentication, and construction through the actual resolver for all 29 catalog SDKs. It includes a real local HTTP generation proving Salad reasoning, tool calls, and usage work with the pinned AI SDK. `scripts/sdk-bridge.test.mjs` supplies actual local HTTP Azure/GitLab account requests, native SDK history/tool/usage conversion, safe errors, and multiplexed sidecar requests. Rust tests exercise native transport/configuration and account behavior rather than requiring live cloud credentials.

`scripts/http-transport.test.mjs` verifies the shared pool's **zero header/body/connect deadlines**, including effective dispatch options when an older built-in fetch supplies positive timeout defaults. Two actual localhost fetches with streamed Unicode cover generation/discovery transport, preserved explicit cancellation signals, and singleton pooling. The bridge installs this policy before importing or constructing providers. The complete Node gate currently contains **24 passing tests**, including real SAP factory namespace and configured compatible-name preservation.

### Frozen provider acceptance

The final October 3 gate passed **208/208 Rust tests** (130 library, 15 CLI configuration, 63 integration), with all optional Node-backed fixtures included and none ignored. It extends the coordinated 207-test gate with a regression for imported native providers missing their endpoint. Formatting, locked all-target warning-free Clippy, and the locked optimized Windows release build passed. The provider subset passed **24/24 Node tests**, the **23-loader / 56-mapping / six-selector** pinned source contracts, actual construction of the **23 nonnative upstream adapters**, and all **29 catalog SDK packages** across **226 providers / 8,385 raw models**. Native Copilot is the 24th upstream adapter and has separate Rust endpoint/account regressions. The catalog's 29 SDK package entries do not include its special adapter name.

GitLab acceptance includes actual SDK project REST and workflow GraphQL discovery with API-key versus OAuth authentication, account-scoped caching, workspace-derived project selection, and discovered workflow references/token limits. `tests/gitlab_discovery.rs` exercises the public `models gitlab --refresh --json` executable and live menu through the shared sidecar. Its resolver deliberately returns a saved URL to prove that an active explicit session URL survives selection; a later same-provider `/connect` replaces both URL and key for discovery and selection. The real CLI resolver regression also verifies that ordinary models with inherited per-model endpoints can still change to their own defaults.

These are provider acceptance results; final cross-platform launcher and persistent-console evidence is recorded in [VERIFICATION.md](VERIFICATION.md).

### Runtime and deployment requirements

- Native Chat, Responses, Messages and Gemini transports need no Node installation. SDK-backed providers and GitLab workflow discovery use the optional shared Node sidecar.
- The runtime sidecar requires **Node.js 22.12+** and the locked dependencies from `npm ci --prefix scripts`. The source-only audit additionally needs **22.13+** for TypeScript stripping; it reports Node's experimental-feature notice.
- An external deployment must keep **`sdk-bridge.mjs`, `http-transport.mjs`, `package.json`, `package-lock.json`, and installed `node_modules` together**. Point `OPENRAID_SDK_BRIDGE_DIR` to that scripts directory; `OPENRAID_NODE` can select the executable. Source-contract/test files are only needed when running audit/tests, not for sidecar generation or discovery.
- Cloud credentials, resource/account identifiers and model permissions remain provider-specific. See [PROVIDERS.md](PROVIDERS.md) for configuration, authentication, endpoint precedence and shared-sidecar behavior.

## All custom loaders

| Source loader | Implemented behavior and audit evidence |
| --- | --- |
| `anthropic` | Native Messages; interleaved-thinking and fine-grained-tool-streaming beta defaults, explicit header override, signed reasoning/tool history. Catalog/main source-loader regressions and `anthropic_stream_preserves_signed_thinking_and_native_tool_history`. |
| `opencode` | Uncredentialed public access is restricted to zero-input-cost models and uses the `public` key; authenticated access retains paid models. `source_loader_public_access_and_headers_preserve_explicit_overrides` and main configuration regression. |
| `openai` | Responses routing; native reasoning/encrypted-response history and tool calls; account-specific Codex routing stays separate. Source response selector, catalog protocol, `codex_lb_responses_stream_preserves_tools_reasoning_and_custom_headers`, and SDK actual-construction coverage. |
| `meta` | Responses rather than generic OpenAI-compatible Chat; source selector and catalog routing. |
| `xai` | Responses routing; source selector and real XAI model construction. |
| `github-copilot` | Explicit model `api.endpoint` takes precedence over GPT-family routing; GPT-5+ except GPT-5-mini uses Responses, other GPT models use Chat; Claude uses native Messages with bearer account/API authentication. `copilot_loader_routes_family_and_explicit_endpoint_before_adapter_defaults`, OAuth routing regression, and actual native bearer request tests. |
| `azure` | Resource metadata/environment or explicit endpoint, refreshed CLI OAuth bearer without stale API-key headers, `useCompletionUrls` chat override, fallback order Responses → Messages → Chat → languageModel. Node selector and actual Azure OAuth HTTP tests; Rust imported Azure refresh tests. |
| `azure-cognitive-services` | Cognitive resource endpoint `https://RESOURCE.cognitiveservices.azure.com/openai/v1`; deployment-based mode omits `/v1`; Azure selectors/authentication apply. Node endpoint/deployment tests and source fixture. |
| `amazon-bedrock` | Config/env/default region; profile/credential chain; bearer bypasses signing chain; `endpoint` beats `baseURL`; explicit ARN/cross-region prefixes preserved; US/EU/APAC/Tokyo/Australia mapping; Mantle exact safeguard models use Chat and other Mantle models use Responses. 56 source comparisons plus actual Mantle model and precedence tests. |
| `llmgateway` | OpenRaid referer/title/source defaults with explicit override; native compatible routing. Catalog/header source regression. |
| `openrouter` | OpenRaid referer/title defaults; real SDK construction and native compatible generation/options. Catalog/header regression. |
| `nvidia` | OpenRaid referer/title/billing-origin defaults; explicitly configured providers are usable; explicit header overrides retained. Catalog/header regression. |
| `vercel` | Referer/title defaults and real Vercel SDK model construction. Catalog/header regression and all-adapter construction audit. |
| `google-vertex` | Explicit project/location before environment fallbacks, ADC/signing-chain handling, `us-central1` default, global/continental/regional domains, trimmed wire ID. Node defaults/ID test and actual Vertex SDK construction; Rust cloud-template tests. |
| `google-vertex-anthropic` | `global` default, explicit/environment project/location, EU/US continental endpoint handling, explicit proxy override, trimmed wire ID. Node regional/default/precedence regressions and source fixture. |
| `sap-ai-core` | SAP service-key authentication, deployment/resource-group settings with configured/env values, callable generation provider; real SAP model construction and default/custom generation namespace preservation. Source loader fixture and all-catalog adapter test. |
| `zenmux` | OpenRaid referer/title defaults and compatible endpoint; catalog/header source regression. |
| `gitlab` | OAuth bearer/API credentials, instance URL, agentic feature flags/gateway beta headers; static workflow models and dynamic generic workflows with selected `workflowRef`/definition; discovery uses workspace plus account-scoped cache, returns capability/limit/workflow metadata, and retains existing catalog/configured models. Node routing/discovery tests and real OAuth agentic HTTP test; Rust catalog/public/live discovery regressions. |
| `cloudflare-workers-ai` | Account/template resolution, API key/alias lookup, explicit proxy bypass and OpenRaid user agent. Dollar-template and credential-alias Rust regressions; real compatible SDK construction. |
| `cloudflare-ai-gateway` | Account/gateway/token validation; explicit proxy bypass; OpenAI native Responses, Anthropic native Messages with dashed slugs, Workers unified route with upstream CF token only for Workers; other providers use catalog REST with gateway-ID binding. Cache TTL/key/skip/log/metadata and custom headers/fetch reach actual gateway requests. Node actual gateway generation and routing tests plus Rust template/options tests. |
| `cerebras` | OpenRaid integration header, explicit overrides, native compatible requests, real Cerebras model construction. Catalog/header regression. |
| `kilo` | OpenRaid referer/title defaults and compatible endpoint; catalog/header source regression. |
| `snowflake-cortex` | Account/config/environment/API/OAuth token resolution and endpoint expansion; native bearer; `max_tokens` → `max_completion_tokens`; malformed empty-role chunks normalized; conversation-complete errors accept nested/top-level/string source shapes; OAuth refresh/replay. `snowflake_source_error_shapes_complete_without_retrying_work`, imported account/refresh/native-output tests, and main/auth/template regressions. |

## All upstream bundled adapters

| Adapter entry | OpenRaid route and verification |
| --- | --- |
| `@ai-sdk/amazon-bedrock` | Specialized SDK; credential/region source comparisons and real model construction. |
| `@ai-sdk/amazon-bedrock/mantle` | Specialized SDK; real Chat/Responses safeguard routing. |
| `@ai-sdk/anthropic` | Native Messages by default; real SDK construction also verified. |
| `@ai-sdk/azure` | Specialized SDK; real OAuth HTTP and selector/fallback tests. |
| `@ai-sdk/google` | Native Gemini; real SDK construction and native thought/tool-signature regression. |
| `@ai-sdk/google-vertex` | Specialized SDK; project/location/default/ID tests and real model construction. |
| `@ai-sdk/google-vertex/anthropic` | Specialized SDK; regional endpoint/ID tests and real model construction. |
| `@ai-sdk/openai` | Native Responses by default; SDK actual model construction and native Responses regressions. |
| `@ai-sdk/openai-compatible` | Native Chat by default; actual SDK tool-call/usage/history HTTP regression. |
| `@openrouter/ai-sdk-provider` | Native Chat by default; actual SDK construction. |
| `@ai-sdk/xai` | Native Responses by default; actual SDK construction. |
| `@ai-sdk/mistral` | Native Chat by default; actual SDK construction. |
| `@ai-sdk/groq` | Native Chat by default; actual SDK construction. |
| `@ai-sdk/deepinfra` | Native Chat by default; actual SDK construction. |
| `@ai-sdk/cerebras` | Native Chat by default; actual SDK construction. |
| `@ai-sdk/cohere` | Specialized SDK; actual model construction. |
| `@ai-sdk/gateway` | Specialized SDK; actual model construction. |
| `@ai-sdk/togetherai` | Native Chat by default; actual SDK construction. |
| `@ai-sdk/perplexity` | Native Chat by default; actual SDK construction. |
| `@ai-sdk/vercel` | Specialized SDK; actual model construction. |
| `@ai-sdk/alibaba` | Native Chat by default; actual SDK construction. |
| `gitlab-ai-provider` | Specialized SDK; actual agentic HTTP, workflow routing and discovery tests. |
| `@ai-sdk/github-copilot` | Native Copilot implementation; explicit endpoint/family/bearer/account regressions. |
| `venice-ai-sdk-provider` | Native Chat by default; actual SDK construction. |

The catalog additionally contains Aihubmix, SAP, Qvac, Salad, Merge Gateway, Watsonx, and Cloudflare gateway SDKs, all included in actual bridge-construction coverage. Salad's pinned package advertises the newer V4 protocol while the shared AI SDK is V6/V3; a narrowly scoped compatibility wrapper retains its compatible generation result shape. Its actual HTTP reasoning/tool-call/usage regression is required evidence, rather than a factory-import pass.

## Deliberate OpenRaid-specific policies

- The searchable provider catalog remains complete; OpenCode's loader `autoload` flag does not hide provider choices. Credential/access checks, cloud defaults and public/free-model gating determine usable models and connections.
- Product attribution headers identify **OpenRaid**. Explicit configured/model/session headers remain authoritative, except refreshed OAuth authorization.
- Requests have **no duration-based aborts**, including upstream OpenCode's default header/chunk/request deadlines. Operator cancellation, worker draining and explicit native-PTY termination govern lifecycle.
- SDK/global fetch uses a shared Undici dispatcher with header/body/connect deadlines disabled, including per-dispatch header/body override protection. Idle pool keepalive is separate from an in-progress request.
- Native supported protocols use one Rust HTTP pool; specialized protocols use one shared lazily started Node sidecar. The bridge generates only: native tools, global board, checkpoints and consensus remain Rust-owned.
- Optional remote discovery cannot prove a live account's permissions offline. Tests verify routing, metadata, authentication and deterministic local HTTP behavior without consuming live model workload.
