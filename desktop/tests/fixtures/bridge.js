// Browser-only fixture. Route /src/bridge.ts to this module in a test context;
// it never connects to Tauri, reads credentials, or writes session databases.
const snapshot = {
  state: "IDLE", workspace: "D:\\projects\\openraid", database: "D:\\projects\\openraid\\.openraid\\session.sqlite3",
  session_id: "session-ui-test", provider: "openai", model: "gpt-test", variant: null,
  agents: Array.from({ length: 500 }, (_, i) => ({
    id: `agent-${String(i + 1).padStart(3, "0")}`, status: "idle", input_tokens: 0, output_tokens: 240,
    cached_tokens: 0, tools: 3, retries: 0, detail: "Waiting for your objective", stream: "",
  })),
  votes: [], board: [{ seq: 1, sender: "agent-001", body: "Shared board preview", owner: false, created_at_ms: 1 }],
  prompts: [], latest_seq: 1, runtime_error: null,
};
const catalog = [
  { id: "openai", name: "OpenAI", configured: true, authentication: "connected", models: [
    { id: "gpt-test", name: "GPT Test", variants: ["low", "high"] },
    { id: "gpt-fast", name: "GPT Fast", variants: [] },
    { id: "gpt-blocked", name: "GPT Unavailable", available: false, variants: [] },
  ] },
  { id: "anthropic", name: "Anthropic", configured: true, authentication: "connected", models: [
    { id: "claude-test", name: "Claude Test", variants: ["balanced", "extended"] },
  ] },
  { id: "local", name: "Offline", configured: true, authentication: "not_required", models: [] },
];
let listener = () => {};
let active = "dark";
const themes = [
  { id: "dark", name: "Dark", dark: true, palette: { background: "#101116", surface: "#181a21", text: "#e9ebf0", muted: "#a0a5b5", accent: "#a6b5ff", border: "#30333e" } },
  { id: "light", name: "Light", dark: false, palette: { background: "#f1f3f8", surface: "#ffffff", text: "#222638", muted: "#616779", accent: "#4358b6", border: "#d9dce6" } },
];
export function isSnapshot(value) { return "state" in value && "agents" in value; }
export function mergeBoard(history, current) { return [...new Map([...history, ...current].map(item => [item.seq, item])).values()].sort((a, b) => a.seq - b.seq); }
export const native = {
  snapshot: async () => null,
  subscribe: async callback => { listener = callback; return () => { listener = () => {}; }; },
  errors: async () => () => {},
  open: async () => ({ ...snapshot }),
  query: async name => name === "catalog" ? catalog : name === "themes" ? { active, themes } : name === "sessions" ? [
    { id: snapshot.session_id, title: "UI test session", workspace: snapshot.workspace, database: snapshot.database, updated_ms: 1 },
    { id: "session-saved", title: "Saved session", workspace: snapshot.workspace, database: "saved.sqlite3", updated_ms: 2 },
  ] : [],
  control: async request => {
    window.__lastControl = request;
    if (request.action === "theme") active = request.name;
    if (request.action === "model") Object.assign(snapshot, { provider: request.provider, model: request.model, variant: request.variant });
    if (request.action === "new") Object.assign(snapshot, { session_id: "session-new", agents: [], board: [] });
    if (request.action === "session") snapshot.session_id = request.id;
    listener({ ...snapshot }); return { ...snapshot };
  },
  board: async () => snapshot.board,
  activity: async () => "Generation and tools preview",
  activityTail: async () => "Live generation preview",
};
