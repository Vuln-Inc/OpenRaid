import { useEffect, useState } from "react";
import { native } from "./bridge";
import { displayPath } from "./display-path";
import { Button } from "./ui";
import { Input } from "./components/base/input/input";

interface Server { name: string; status: unknown }
export function SettingsPanel({ busy, onControl }: { busy: boolean; onControl: (action: string, fields: Record<string, unknown>) => Promise<void> }) {
  const [servers, setServers] = useState<Server[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [path, setPath] = useState("board-export.json");
  async function refresh() {
    setLoading(true); setError("");
    try { setServers(await native.query("mcp") as Server[]); }
    catch (reason) { setError(String(reason)); }
    finally { setLoading(false); }
  }
  useEffect(() => { void refresh(); }, []);
  const statusLabel = (status: unknown): string => typeof status === "string" ? status : status && typeof status === "object" ? Object.keys(status)[0] ?? "Unknown" : "Unknown";
  return <>
    <section className="settings-section"><div className="section-heading"><div><h2>Board export</h2><p>Save the complete shared board as a JSON file.</p></div></div>
      <form className="export-form" onSubmit={event => { event.preventDefault(); void onControl("board_export", { path }); }}><Input className="flex-1" label="Export location" value={path} onChange={setPath} placeholder="board-export.json" /><Button type="submit" disabled={busy || !path.trim()}>Export board</Button></form><small>Relative paths are saved in your workspace. Existing files are not overwritten. {displayPath(path)}</small>
    </section>
    <section className="settings-section"><div className="section-heading"><div><h2>MCP connections</h2><p>Manage the tool servers configured for this workspace.</p></div><Button disabled={loading || busy} onClick={() => void refresh()}>Refresh</Button></div>
      {loading ? <p role="status">Loading connections…</p> : error ? <p role="alert" className="inline-error">{error}</p> : servers.length === 0 ? <p className="empty">No MCP servers configured. Add servers to your shared OpenRaid configuration to connect external tools.</p> : <div className="server-list">{servers.map(server => <div className="server-row" key={server.name}><div><strong>{server.name}</strong><small>{statusLabel(server.status)}</small></div><Button disabled={busy} onClick={async () => { await onControl("mcp", { name: server.name }); await refresh(); }}>Toggle / retry</Button></div>)}</div>}
    </section>
  </>;
}
