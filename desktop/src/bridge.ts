import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export interface Message { seq: number; sender: string; body: string; owner: boolean; created_at_ms: number }
export interface Agent { id: string; status: string; input_tokens: number; output_tokens: number; cached_tokens: number; tools: number; retries: number; detail: string; stream: string }
export interface Vote { agent_id: string; done: boolean; reason: string; board_seq: number }
export interface Snapshot {
  state: "IDLE" | "RUNNING" | "PAUSED" | "STOPPING";
  resumable?: boolean;
  workspace: string; database: string; session_id: string | null; model: string; provider: string; variant?: string | null;
  agents: Agent[]; votes: Vote[]; board: Message[]; prompts: Message[]; latest_seq: number; runtime_error: string | null;
}
export type ControlResult = Snapshot | { text: string; restored: boolean } | { path: string };
export interface ThemeInfo { active: string; themes: { id: string; dark: boolean; palette?: Record<string, string> }[] }
export interface OpencodeStatus { available: boolean; consent: boolean | null }
export function isSnapshot(value: ControlResult): value is Snapshot { return "state" in value && "agents" in value; }
export const native = {
  snapshot: () => invoke<Snapshot | null>("desktop_snapshot"),
  open: (workspace: string, mock: boolean) => invoke<Snapshot>("desktop_open", { request: { workspace, mock } }),
  opencodeStatus: (workspace: string) => invoke<OpencodeStatus>("desktop_opencode_status", { workspace }),
  importOpencode: (workspace: string, enabled: boolean) => invoke<OpencodeStatus>("desktop_import_opencode", { workspace, enabled }),
  control: (request: Record<string, unknown>) => invoke<ControlResult>("desktop_control", { request }),
  activity: (agentId: string) => invoke<string>("desktop_activity", { agentId }),
  activityTail: (agentId: string) => invoke<string>("desktop_activity_tail", { agentId, maxBytes: 16384 }),
  board: (after: number) => invoke<Message[]>("desktop_board", { after, limit: 256 }),
  query: (name: string) => invoke<unknown>(`desktop_${name}`),
  subscribe: (callback: (snapshot: Snapshot) => void) => listen<Snapshot>("openraid://snapshot", event => callback(event.payload)),
  errors: (callback: (error: string) => void) => listen<string>("openraid://error", event => callback(event.payload)),
};

export function mergeBoard(history: Message[], current: Message[]): Message[] {
  const messages = new Map(history.map(message => [message.seq, message]));
  current.forEach(message => messages.set(message.seq, message));
  return [...messages.values()].sort((a, b) => a.seq - b.seq);
}
