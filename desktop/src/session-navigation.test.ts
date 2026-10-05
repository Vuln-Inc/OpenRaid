import { describe, expect, it } from "vitest";
import { filterSessions, type SavedSession } from "./SessionNavigator";

const sessions: SavedSession[] = [
  { id: "session-old", title: "Fix authentication", workspace: "C:\\work\\api", database: "C:\\work\\api\\old.db", updated_ms: 1 },
  { id: "session-new", title: "Design review", workspace: "C:\\work\\web", database: "C:\\work\\web\\new.db", updated_ms: 2 },
];
describe("saved session discovery", () => {
  it("sorts most recently updated first without changing source order", () => {
    expect(filterSessions(sessions, "").map(item => item.id)).toEqual(["session-new", "session-old"]);
    expect(sessions[0].id).toBe("session-old");
  });
  it("searches titles, readable workspace paths and stable ids case-insensitively", () => {
    expect(filterSessions(sessions, " AUTH ")[0].id).toBe("session-old");
    expect(filterSessions(sessions, "work\\web")[0].id).toBe("session-new");
    expect(filterSessions(sessions, "SESSION-NEW")[0].id).toBe("session-new");
    expect(filterSessions(sessions, "missing")).toEqual([]);
  });
});
