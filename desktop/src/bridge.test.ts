import { beforeEach, describe, expect, it, vi } from "vitest";
import { mergeBoard, native, type Message, type Snapshot } from "./bridge";

const { invoke, listen } = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen }));

const message = (seq: number, body = "text"): Message => ({ seq, body, sender: "agent-001", owner: false, created_at_ms: 0 });
describe("board history", () => {
  it("deduplicates overlap and preserves earlier pages in global order", () => {
    expect(mergeBoard([message(2), message(1)], [message(2, "updated"), message(3)]))
      .toEqual([message(1), message(2, "updated"), message(3)]);
  });
  it("supports empty snapshots", () => expect(mergeBoard([], [])).toEqual([]));
  it("keeps the last authoritative copy when one snapshot repeats a sequence", () => {
    expect(mergeBoard([message(1, "saved")], [message(1, "earlier"), message(1, "latest")]))
      .toEqual([message(1, "latest")]);
  });
  it("retains older history when the live snapshot window advances", () => {
    const retained = Array.from({ length: 500 }, (_, index) => message(index + 1));
    const pushed = Array.from({ length: 256 }, (_, index) => message(index + 490, "live"));
    const merged = mergeBoard(retained, pushed);
    expect(merged).toHaveLength(745);
    expect(merged[0].seq).toBe(1);
    expect(merged[489].body).toBe("live");
    expect(merged[744].seq).toBe(745);
    expect(retained[489].body).toBe("text");
  });
});

describe("native bridge contract", () => {
  beforeEach(() => { vi.resetAllMocks(); });

  it("hydrates the current session once, with null for a fresh native host", async () => {
    invoke.mockResolvedValueOnce(null).mockResolvedValueOnce({ state: "PAUSED", agents: [] });
    await expect(native.snapshot()).resolves.toBeNull();
    await expect(native.snapshot()).resolves.toEqual({ state: "PAUSED", agents: [] });
    expect(invoke.mock.calls).toEqual([["desktop_snapshot"], ["desktop_snapshot"]]);
  });

  it("opens the workspace in the in-process native runtime", async () => {
    const response = { state: "IDLE" };
    invoke.mockResolvedValue(response);
    await expect(native.open("C:\\my workspace", false)).resolves.toBe(response);
    expect(invoke).toHaveBeenCalledWith("desktop_open", { request: { workspace: "C:\\my workspace", mock: false } });
  });

  it("forwards session controls without rewriting their fields", async () => {
    const request = { action: "model", provider: "openai", model: "example-model", variant: "high" };
    invoke.mockResolvedValue({ state: "IDLE" });
    await native.control(request);
    expect(invoke).toHaveBeenCalledWith("desktop_control", { request });
  });

  it.each([
    { action: "start", text: "Improve this workspace" },
    { action: "pause" },
    { action: "resume" },
    { action: "stop" },
    { action: "new" },
    { action: "session", id: "saved-session" },
    { action: "add", count: 500 },
    { action: "remove", agentId: "agent-007" },
    { action: "restore", sequence: 42 },
    { action: "variant", variant: null },
    { action: "theme", name: "midnight" },
    { action: "mcp", name: "workspace-tools" },
    { action: "board_export", path: "C:\\my workspace\\board.txt" },
  ])("preserves the $action shared control payload", async request => {
    await native.control(request);
    expect(invoke).toHaveBeenCalledExactlyOnceWith("desktop_control", { request });
  });

  it("requests bounded activity tails and board history", async () => {
    await native.activity("agent-007");
    await native.activityTail("agent-007");
    await native.board(512);
    expect(invoke.mock.calls).toEqual([
      ["desktop_activity", { agentId: "agent-007" }],
      ["desktop_activity_tail", { agentId: "agent-007", maxBytes: 16384 }],
      ["desktop_board", { after: 512, limit: 256 }],
    ]);
  });

  it("queries the shared native settings sources", async () => {
    for (const name of ["themes", "sessions", "catalog", "mcp"]) await native.query(name);
    expect(invoke.mock.calls.map(call => call[0])).toEqual([
      "desktop_themes", "desktop_sessions", "desktop_catalog", "desktop_mcp",
    ]);
  });

  it("receives pushed snapshots and returns the native unsubscribe handle", async () => {
    const unsubscribe = vi.fn();
    listen.mockResolvedValue(unsubscribe);
    const callback = vi.fn();
    await expect(native.subscribe(callback)).resolves.toBe(unsubscribe);
    expect(listen).toHaveBeenCalledWith("openraid://snapshot", expect.any(Function));
    const snapshot = { state: "RUNNING", agents: [] } as unknown as Snapshot;
    listen.mock.calls[0][1]({ payload: snapshot });
    expect(callback).toHaveBeenCalledWith(snapshot);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("receives native runtime errors without exposing event metadata", async () => {
    const unsubscribe = vi.fn();
    listen.mockResolvedValue(unsubscribe);
    const callback = vi.fn();
    await expect(native.errors(callback)).resolves.toBe(unsubscribe);
    expect(listen).toHaveBeenCalledWith("openraid://error", expect.any(Function));
    listen.mock.calls[0][1]({ payload: "Provider credentials are missing", id: 42 });
    expect(callback).toHaveBeenCalledWith("Provider credentials are missing");
  });

  it("preserves native failures for user-facing error feedback", async () => {
    invoke.mockRejectedValue(new Error("Workspace unavailable"));
    await expect(native.open("missing", false)).rejects.toThrow("Workspace unavailable");
    listen.mockRejectedValue(new Error("Native connection unavailable"));
    await expect(native.subscribe(vi.fn())).rejects.toThrow("Native connection unavailable");
  });
});
