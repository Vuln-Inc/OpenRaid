import { useEffect, useMemo, useState } from "react";
import { native, type Snapshot } from "./bridge";
import { displayPath } from "./display-path";
import { VirtualList } from "./VirtualList";
import "./session-navigation.css";
import { Button } from "./ui";
import { Input } from "./components/base/input/input";

export interface SavedSession {
  id: string;
  title: string;
  workspace: string;
  database: string;
  updated_ms: number;
}

export function filterSessions(sessions: SavedSession[], search: string): SavedSession[] {
  const query = search.trim().toLocaleLowerCase();
  return sessions.filter(session => !query || [session.title, session.id, displayPath(session.workspace), displayPath(session.database)]
    .some(value => value.toLocaleLowerCase().includes(query)))
    .sort((a, b) => b.updated_ms - a.updated_ms || a.id.localeCompare(b.id));
}

interface Props {
  snapshot: Snapshot;
  busy: boolean;
  onNew: () => void | Promise<void>;
  onOpen: (id: string) => void | Promise<void>;
}

export function SessionNavigator({ snapshot, busy, onNew, onOpen }: Props) {
  const [sessions, setSessions] = useState<SavedSession[]>([]);
  const [search, setSearch] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [refresh, setRefresh] = useState(0);
  const [opening, setOpening] = useState(false);
  useEffect(() => {
    let disposed = false;
    setLoading(true);
    setError("");
    native.query("sessions").then(value => {
      if (!Array.isArray(value) || !value.every(item => item && typeof item.id === "string" && typeof item.title === "string" && typeof item.workspace === "string" && typeof item.database === "string" && typeof item.updated_ms === "number")) {
        throw new Error("Saved session information is unavailable. Try refreshing the list.");
      }
      if (!disposed) setSessions(value as SavedSession[]);
    }).catch(reason => { if (!disposed) setError(String(reason)); })
      .finally(() => { if (!disposed) setLoading(false); });
    return () => { disposed = true; };
  }, [snapshot.database, snapshot.state, refresh]);
  const filtered = useMemo(() => filterSessions(sessions, search), [sessions, search]);
  const idle = snapshot.state === "IDLE";
  const disabled = busy || opening || !idle;
  const unavailable = !idle ? "Stop the current run before creating or opening another session. Pausing keeps this session active." : "";
  async function perform(action: () => void | Promise<void>) {
    setOpening(true); setError("");
    try { await action(); } catch (reason) { setError(String(reason)); }
    finally { setOpening(false); }
  }
  return <section className="session-navigator" aria-label="Saved sessions">
    <div className="session-navigation-heading"><div><h2>Sessions</h2><p>Pick up where you left off, or create a fresh session without starting agents.</p></div>
      <Button className="primary" disabled={disabled} title={unavailable || "Your current session remains saved"} onClick={() => void perform(onNew)}>New session</Button>
    </div>
    {unavailable && <p className="session-navigation-hint" role="status">{unavailable}</p>}
    <div className="session-search-row"><Input className="flex-1" label="Find a saved session" type="search" placeholder="Search title, workspace or session ID" value={search} onChange={setSearch} />
      <Button disabled={loading || opening} onClick={() => setRefresh(value => value + 1)}>Refresh sessions</Button></div>
    {error && <div className="session-navigation-error" role="alert"><p>{error}</p><Button disabled={loading} onClick={() => setRefresh(value => value + 1)}>Retry</Button></div>}
    <p className="session-navigation-status" role="status">{loading ? "Loading saved sessions…" : `${filtered.length} ${filtered.length === 1 ? "session" : "sessions"}${search.trim() ? " matching your search" : " · most recent first"}`}</p>
    <div className="saved-session-list" aria-busy={loading}>
      <VirtualList items={filtered} rowHeight={116} label="Saved sessions" emptyState={loading || error ? <span /> : <div className="empty-state"><h2>{search.trim() ? "No matching sessions" : "No saved sessions yet"}</h2><p>{search.trim() ? "Try another title or workspace path." : "Your sessions are saved automatically and shared with the terminal app."}</p>{search.trim() && <Button onClick={() => setSearch("")}>Clear search</Button>}</div>} getKey={item => item.id} render={item => {
        const current = item.id === snapshot.session_id || item.database === snapshot.database;
        const updated = new Date(item.updated_ms);
        return <Button className={`saved-session-card ${current ? "current" : ""}`} disabled={disabled || current} aria-current={current ? "page" : undefined} title={current ? "This session is already open" : unavailable || `Open ${item.title || item.id}`} onClick={() => void perform(() => onOpen(item.id))}>
          <span className="saved-session-title"><strong>{item.title || "Untitled session"}</strong><span>{current ? "Current session" : "Open session"}</span></span>
          <span className="saved-session-workspace" title={displayPath(item.workspace)}>{displayPath(item.workspace)}</span>
          <span className="saved-session-footer"><small title={item.id}>{item.id}</small><time dateTime={Number.isFinite(updated.getTime()) ? updated.toISOString() : undefined}>{Number.isFinite(updated.getTime()) ? updated.toLocaleString() : "Unknown update time"}</time></span>
        </Button>;
      }} />
    </div>
  </section>;
}
