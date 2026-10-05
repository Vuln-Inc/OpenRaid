import { describe, expect, it } from "vitest";
import { isSnapshot, type Snapshot } from "./bridge";

describe("native control response discrimination", () => {
  it("keeps restore drafts out of runtime snapshot state", () => {
    expect(isSnapshot({ text: "Restore this objective", restored: true })).toBe(false);
    expect(isSnapshot({ text: "Draft only", restored: false })).toBe(false);
  });

  it("keeps export acknowledgements out of runtime snapshot state", () => {
    expect(isSnapshot({ path: "C:\\workspace\\board.json" })).toBe(false);
  });

  it("recognizes idle and live snapshots without requiring any agents", () => {
    const snapshot: Snapshot = {
      state: "IDLE",
      workspace: "/workspace",
      database: "/workspace/.openraid/openraid.sqlite3",
      session_id: null,
      provider: "mock",
      model: "offline",
      agents: [],
      votes: [],
      board: [],
      prompts: [],
      latest_seq: 0,
      runtime_error: null,
    };
    expect(isSnapshot(snapshot)).toBe(true);
    for (const state of ["RUNNING", "PAUSED", "STOPPING"] as const) {
      expect(isSnapshot({ ...snapshot, state })).toBe(true);
    }
  });
});
