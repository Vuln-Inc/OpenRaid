import { useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import { native, mergeBoard, isSnapshot, type Message, type Snapshot, type ThemeInfo } from "./bridge";
import { VirtualList } from "./VirtualList";
import { displayPath } from "./display-path";
import { SettingsPanel } from "./SettingsPanel";
import { ConfirmDialog } from "./ConfirmDialog";
import { ThemePicker } from "./ThemePicker";
import { ModelPicker } from "./ModelPicker";
import { SessionNavigator } from "./SessionNavigator";
import { MessageDialog } from "./MessageDialog";
import { Button } from "./ui";
import { Input } from "./components/base/input/input";
import { InputNumber } from "./components/base/input/input-number";
import { Checkbox } from "./components/base/checkbox/checkbox";
import { TextArea } from "./components/base/textarea/textarea";
import { Dot } from "./components/foundations/dot-icon";
import { Tabs, TabList, TabPanel } from "./components/application/tabs/tabs";
import { Notifications, notify, notifyError } from "./notifications";
import { appearanceStyle, desktopPalette } from "./appearance";
import { ShellIcon, type ShellIconName } from "./ShellIcon";
import { ModelMenu } from "./ModelMenu";

const appIcon = new URL("../app-icon.svg", import.meta.url).href;

type Tab = "Board" | "Activity" | "Prompts" | "Sessions" | "Settings";
const viewIcons: Record<Tab, ShellIconName> = { Board: "board", Activity: "activity", Prompts: "prompts", Sessions: "sessions", Settings: "settings" };
export function App() {
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [workspace, setWorkspace] = useState(".");
  const [mock, setMock] = useState(false);
  const [busy, setBusy] = useState(false);
  const [connected, setConnected] = useState(false);
  const [tab, setTab] = useState<Tab>("Board");
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [rosterOpen, setRosterOpen] = useState(true);
  const sidebarToggle = useRef<HTMLButtonElement>(null);
  useEffect(() => { if (!sidebarOpen) sidebarToggle.current?.focus(); }, [sidebarOpen]);
  const [draft, setDraft] = useState("");
  const objectiveInput = useRef<HTMLTextAreaElement>(null);
  const [selected, setSelected] = useState("");
  const [transcript, setTranscript] = useState("");
  const [tail, setTail] = useState("");
  const [confirmation, setConfirmation] = useState<{ title: string; description: string; label: string; action: () => void } | null>(null);
  const [importPrompt, setImportPrompt] = useState<{ workspace: string; startup: boolean; openMock?: boolean } | null>(null);
  const [importRevision, setImportRevision] = useState(0);
  const importCheck = useRef(0);
  const [palette, setPalette] = useState("dark");
  const [themeStyle, setThemeStyle] = useState<CSSProperties>({});
  useEffect(() => {
    const root = document.documentElement;
    root.classList.toggle("dark-mode", palette === "dark");
    root.style.colorScheme = palette;
    const defaults = palette === "light"
      ? appearanceStyle(desktopPalette({ id: "light", dark: false }))
      : appearanceStyle(desktopPalette({ id: "dark", dark: true }));
    for (const [key, value] of Object.entries({ ...defaults, ...themeStyle })) root.style.setProperty(key, String(value));
  }, [palette, themeStyle]);
  async function loadTheme() {
    const result = await native.query("themes") as ThemeInfo;
    const current = result.themes.find(item => item.id === result.active);
    setPalette(current?.dark === false ? "light" : "dark");
    setThemeStyle(appearanceStyle(desktopPalette(current ?? { id: "dark", dark: true })));
  }
  const [expanded, setExpanded] = useState<Message | null>(null);
  const [history, setHistory] = useState<Message[]>([]);
  const [historyCursor, setHistoryCursor] = useState<number | null>(null);
  const [count, setCount] = useState(1);
  const revision = useRef(0);
  const activityFlight = useRef(false);
  const latestActivity = useRef<{ id: string; database: string } | null>(null);
  const snapshotRef = useRef(snapshot);
  snapshotRef.current = snapshot;
  async function fetchTail() {
    const key = latestActivity.current;
    if (!key || activityFlight.current) return;
    activityFlight.current = true;
    try {
      const value = await native.activityTail(key.id);
      if (latestActivity.current?.id === key.id && snapshotRef.current?.database === key.database) setTail(value);
    } catch (reason) {
      if (latestActivity.current?.id === key.id && snapshotRef.current?.database === key.database) notifyError(String(reason));
    } finally {
      activityFlight.current = false;
      const latest = latestActivity.current;
      if (latest && (latest.id !== key.id || latest.database !== key.database)) void fetchTail();
    }
  }
  const database = useRef<string | null>(null);
  useEffect(() => {
    if (!snapshot) return;
    if (database.current !== snapshot.database) {
      database.current = snapshot.database;
      setHistory([]); setHistoryCursor(null); setSelected(""); setTranscript(""); setTail(""); setExpanded(null);
    }
  }, [snapshot?.database]);
  useEffect(() => { if (connected) void loadTheme().catch(reason => notifyError(String(reason))); }, [connected]);
  useEffect(() => {
    if (!connected) return;
    let disposed = false;
    const check = ++importCheck.current;
    const currentWorkspace = snapshot?.workspace ?? workspace;
    native.opencodeStatus(currentWorkspace).then(status => {
      if (!disposed && check === importCheck.current && status.available && status.consent === null) setImportPrompt({ workspace: currentWorkspace, startup: true });
    }).catch(reason => { if (!disposed && check === importCheck.current) notifyError(String(reason)); });
    return () => { disposed = true; };
  }, [connected, snapshot?.workspace]);
  useEffect(() => {
    latestActivity.current = selected && snapshot ? { id: selected, database: snapshot.database } : null;
    setTail("");
    void fetchTail();
    return () => { latestActivity.current = null; };
  }, [selected, snapshot?.database]);
  useEffect(() => { void fetchTail(); }, [snapshot]);
  useEffect(() => {
    let disposed = false;
    let cleanup: (() => void) | undefined;
    let cleanupErrors: (() => void) | undefined;
    const before = revision.current;
    native.errors(value => { if (!disposed) notifyError(value); }).then(unlisten => { if (disposed) unlisten(); else cleanupErrors = unlisten; }).catch(() => {});
    native.subscribe(value => { if (!disposed) { revision.current++; setSnapshot(value); } })
      .then(async unlisten => {
        if (disposed) { unlisten(); return; }
        cleanup = unlisten;
        // Subscribe first, then hydrate once. A newer pushed snapshot wins over
        // an older IPC response, including session navigation during startup.
        const current = await native.snapshot();
        if (disposed) return;
        if (before === revision.current) setSnapshot(current);
        setConnected(true);
      })
      .catch(reason => { if (!disposed) notifyError(`Native desktop connection unavailable: ${String(reason)}`); });
    return () => { disposed = true; cleanup?.(); cleanupErrors?.(); };
  }, []);
  async function task(work: () => Promise<void>) {
    setBusy(true); notifyError("");
    try { await work(); } catch (reason) { notifyError(String(reason)); }
    finally { setBusy(false); }
  }
  async function openWorkspace(path: string, demo: boolean) {
    const before = revision.current;
    const result = await native.open(path, demo);
    if (before === revision.current) setSnapshot(result);
  }
  async function requestOpen() {
    ++importCheck.current;
    await task(async () => {
      const status = await native.opencodeStatus(workspace);
      if (status.available && status.consent === null) {
        setImportPrompt({ workspace, startup: true, openMock: mock });
        return;
      }
      await openWorkspace(workspace, mock);
    });
  }
  async function decideImport(enabled: boolean) {
    const prompt = importPrompt;
    if (!prompt) return;
    await task(async () => {
      await native.importOpencode(prompt.workspace, enabled);
      setImportRevision(value => value + 1);
      if (enabled) notify("OpenCode credentials and provider settings are now available.");
      if (prompt.openMock !== undefined) await openWorkspace(prompt.workspace, prompt.openMock);
    });
  }
  function requestImport(path: string) {
    ++importCheck.current;
    setImportPrompt({ workspace: path, startup: false });
  }
  async function control(action: string, fields: Record<string, unknown> = {}) {
    const work = async () => {
      const before = revision.current;
      const result = await native.control({ action, ...fields });
      if (isSnapshot(result) && before === revision.current) setSnapshot(result);
      else if ("text" in result) { setDraft(result.text); notify(result.restored ? "Prompt and workspace snapshot restored." : "Prompt restored. No workspace snapshot was available."); }
      else if ("path" in result) { notify(`Board exported to ${displayPath(result.path)}`); }
      if (action === "theme") {
        await loadTheme();
      }
      if (action === "new") { setTab("Board"); setDraft(""); notify("New session ready. Add an objective to start agents."); }
      if (action === "session") { setTab("Board"); setDraft(""); notify("Saved session opened."); }
    };
    if (["stop", "pause", "resume"].includes(action)) {
      try { await work(); } catch (reason) { notifyError(String(reason)); }
    } else await task(work);
  }
  const idle = snapshot?.state === "IDLE";
  const stopping = snapshot?.state === "STOPPING";
  const agent = snapshot?.agents.find(item => item.id === selected);
  const board = useMemo(() => mergeBoard(history, snapshot?.board ?? []), [history, snapshot?.board]);
  const transcriptLines = useMemo(() => transcript.split("\n"), [transcript]);
  const votes = useMemo(() => new Map(snapshot?.votes.map(vote => [vote.agent_id, vote]) ?? []), [snapshot?.votes]);
  const done = snapshot?.votes.filter(vote => vote.done).length ?? 0;
  const confirmNew = () => setConfirmation({ title: "Create a new session?", description: "Your current session stays saved. The new session starts with a clean board and no running agents.", label: "Create session", action: () => { void control("new"); } });
  useEffect(() => {
    const shortcut = (event: KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "n" && snapshot && idle && !busy) {
        event.preventDefault();
        confirmNew();
      }
    };
    window.addEventListener("keydown", shortcut);
    return () => window.removeEventListener("keydown", shortcut);
  }, [snapshot?.database, idle, busy]);
  const projectName = snapshot ? displayPath(snapshot.workspace).split(/[\\/]/).filter(Boolean).pop() ?? "Workspace" : "OpenRaid";
  return <div className={`app palette-${palette}`} style={themeStyle} data-sidebar={sidebarOpen ? "open" : "closed"}>
    <Notifications theme={palette === "light" ? "light" : "dark"} />
    {snapshot?.runtime_error && <div className="error" role="alert"><span>{snapshot.runtime_error}</span></div>}
    {!snapshot ? <main className="welcome"><div className="welcome-brand"><img src={appIcon} alt="" /><span>OpenRaid</span></div><div className="welcome-card"><ShellIcon name="folder" className="welcome-icon" /><h2>Let’s build something</h2><p>Open a project to start working with your agents.</p>
      <span className="connection-status" role="status" aria-label="Session state: not connected">{connected ? "Ready to open a workspace" : "Connecting to desktop…"}</span>
      <form onSubmit={event => { event.preventDefault(); void requestOpen(); }}>
        <Input label="Workspace folder" isRequired value={workspace} onChange={setWorkspace} placeholder="Choose a project folder path" />
        <Checkbox label="Use offline demo provider" isSelected={mock} onChange={setMock} hint="Demo mode runs without credentials. Your real provider preferences stay unchanged." />
        <Button type="submit" className="primary" disabled={busy || !connected || !!importPrompt}>{busy ? "Opening workspace…" : connected ? "Open workspace" : "Connecting to desktop…"}</Button>
      </form><p className="welcome-note">Shared with the terminal: saved sessions, preferences and credentials. Opening a workspace does not start agents.</p></div></main> : <>
      <main className="workspace-shell"><Tabs className="workspace grid" orientation="vertical" selectedKey={tab} onSelectionChange={key => setTab(String(key) as Tab)}>
        <aside className="roster" aria-label="Agents" id="workspace-sidebar">
          <div className="sidebar-brand"><img src={appIcon} alt="" /><span>OpenRaid</span><Button className="icon-button ghost" aria-label="Collapse sidebar" onClick={() => setSidebarOpen(false)}><ShellIcon name="panel" /></Button></div>
          <Button className="new-session ghost" aria-label="New session" disabled={busy || !idle} title={idle ? "Create a separate saved session" : "Stop the current run before creating a session"} onClick={confirmNew}><ShellIcon name="compose" /><span>New session</span><kbd>Ctrl N</kbd></Button>
          <TabList type="button-gray" orientation="vertical" className="sidebar-nav" aria-label="Workspace views" items={(["Board", "Activity", "Prompts", "Sessions", "Settings"] as Tab[]).map(name => ({ id: name, label: <><ShellIcon name={viewIcons[name]} /><span>{name}</span></> }))} />
          <Button className="sidebar-project ghost" aria-label={`Toggle agents for ${projectName}`} aria-expanded={rosterOpen} onClick={() => setRosterOpen(value => !value)} title={displayPath(snapshot.workspace)}><ShellIcon name="folder" /><strong>{projectName}</strong><ShellIcon name={rosterOpen ? "down" : "chevron"} /></Button>
          <div className="roster-header" hidden={!rosterOpen}><div className="panel-heading"><h2>Agents <small>{snapshot.agents.length}</small></h2></div>
          <div className="membership"><InputNumber aria-label="Number of agents to add" minValue={1} maxValue={Math.max(1, 500 - snapshot.agents.length)} value={count} onChange={setCount} size="sm" className="w-full shrink-0 md:w-20" wrapperClassName="h-9" inputClassName="h-full min-w-0" /><Button className="h-9" title={`${Math.max(0, 500 - snapshot.agents.length)} slots available (500-agent limit)`} disabled={busy || stopping || !Number.isInteger(count) || count < 1 || count > 500 - snapshot.agents.length} onClick={() => void control("add", { count })}>Add agents</Button></div>
          </div>
          <div className="roster-body" hidden={!rosterOpen}>
          <VirtualList items={snapshot.agents} rowHeight={68} label="Agent roster" emptyState={<p className="empty">No agents yet. Add an agent or start an objective.</p>} getKey={item => item.id} render={item => <Button aria-pressed={selected === item.id} className={`agent-row ${selected === item.id ? "selected" : ""}`} onClick={() => { setSelected(item.id); setTranscript(""); setTab("Activity"); }}>
            <span className="agent-title"><span className={`agent-status-dot status-${item.status}`} /><strong>{item.id}</strong><small>{votes.get(item.id)?.done ? <ShellIcon name="check" /> : item.status}</small></span>
            <small className="preview">{item.detail || item.stream || "Waiting for activity"}</small><small className="agent-metrics">{item.tools} tools · {item.output_tokens.toLocaleString()} tokens</small>
          </Button>} />
          </div>
        </aside>
        <section className="session-panel" aria-label="Session workspace">
          <header className="workspace-header">
            <Button ref={sidebarToggle} className="icon-button ghost" aria-label={sidebarOpen ? "Collapse sidebar" : "Expand sidebar"} aria-expanded={sidebarOpen} aria-controls="workspace-sidebar" onClick={() => setSidebarOpen(value => !value)}><ShellIcon name="panel" /></Button>
            <div className="workspace-title"><strong>{tab === "Board" ? "Session" : tab}</strong><span className="header-project"><ShellIcon name="folder" />{projectName}</span></div>
            <span className="session-status" data-state={snapshot.state} role="status" aria-label={`Session state: ${snapshot.state}`}><Dot size="sm" className="session-status-dot" aria-hidden="true" />{{ IDLE: "Idle", RUNNING: "Running", PAUSED: "Paused", STOPPING: "Stopping" }[snapshot.state]}</span>
          <div className="toolbar" aria-label="Session controls">
            <div className="run-actions">
              <Button className="icon-button ghost" aria-label="Pause" disabled={snapshot.state !== "RUNNING"} title="Pause running agents; resume them later" onClick={() => void control("pause")}><ShellIcon name="pause" /></Button>
            <Button className="icon-button ghost" aria-label={idle && snapshot.resumable ? "Resume saved task" : "Resume"} disabled={snapshot.state !== "PAUSED" && !(idle && snapshot.resumable)} title={idle && snapshot.resumable ? "Resume saved task" : "Continue paused agents"} onClick={() => void control("resume")}><ShellIcon name="play" /></Button>
              <Button className="icon-button ghost" aria-label="Stop run" disabled={idle || stopping} title="Cancel current requests and tools; saved history remains" onClick={() => setConfirmation({ title: "Stop this run?", description: "Cancel current requests and tool operations. Saved history remains; completed file changes are not undone.", label: "Stop run", action: () => { void control("stop"); } })}><ShellIcon name="stop" /></Button>
            </div>
          </div>
          </header>
          <div className="session-context"><span className="session-meta" title={displayPath(snapshot.database)}>{snapshot.session_id ?? snapshot.database.split(/[\\/]/).pop()}</span><span className="vote-summary">{done} / {snapshot.agents.length} completion votes</span></div>
          <TabPanel id={tab} className="content flex min-h-0 flex-1 flex-col overflow-hidden">
          {tab === "Board" && <><div className="subtoolbar"><span>Global board · {board.length} retained messages</span><Button disabled={busy} onClick={() => void task(async () => { const database = snapshot.database; const page = await native.board(historyCursor ?? 0); if (snapshotRef.current?.database !== database) return; setHistory(old => mergeBoard(old, page)); if (page.length) setHistoryCursor(page[page.length - 1].seq); })}>{historyCursor === null ? "Load from beginning" : "Load next history page"}</Button></div>
              <VirtualList items={board} rowHeight={124} label="Global messageboard" emptyState={<div className="empty-state board-empty"><ShellIcon name="terminal" /><h2>What would you like to build?</h2><p>Give your agents an objective. Work happens here, together.</p><span className="empty-project"><ShellIcon name="folder" />{projectName}</span></div>} getKey={item => item.seq} render={item => <Button className={`message ${item.owner ? "owner-message" : ""}`} onClick={() => setExpanded(item)}><span><strong>{item.owner ? "You" : item.sender}</strong><small>#{item.seq} · {new Date(item.created_at_ms).toLocaleTimeString()}</small></span><p>{item.body}</p></Button>} /></>}
          {tab === "Activity" && <div className="activity"><div className="subtoolbar"><h2>{selected || "Select an agent"}</h2>{agent && <><Button disabled={busy} onClick={() => void task(async () => { const id = selected; const database = snapshot.database; const text = await native.activity(id); if (latestActivity.current?.id === id && snapshotRef.current?.database === database) setTranscript(text); })}>Load full transcript</Button><Button disabled={busy || stopping} onClick={() => setConfirmation({ title: `Retire ${selected}?`, description: "This agent leaves the active roster after its in-flight work drains. Shared board history is preserved.", label: "Retire agent", action: () => { void control("remove", { agentId: selected }); } })}>Retire agent</Button></>}</div>
            {!agent && <div className="empty-state"><h2>Follow an agent’s work</h2><p>Select an agent in the roster to see generation, tool calls, results and completion votes.</p></div>}
            {agent && <><p className="agent-detail">{agent.status} · {agent.detail}</p>{votes.get(selected) && <p className="vote-detail">Vote: {votes.get(selected)?.done ? "complete" : "not complete"} — {votes.get(selected)?.reason}</p>}<h3>Live generation / tool activity</h3><pre className="live-output">{tail || agent.stream || agent.detail || "No generation yet."}</pre><h3>Retained transcript · generation, tool calls/results, command & PTY output</h3><div className="transcript-window">{transcript ? <VirtualList items={transcriptLines} rowHeight={22} label="Full activity transcript" getKey={(_, index) => index} render={(line, index) => <pre className="transcript-line" title={line}>{index + 1} {line}</pre>} /> : <p className="empty">Load the full transcript on demand. Live previews above update through native events.</p>}</div></>}
          </div>}
           {tab === "Prompts" && <VirtualList items={snapshot.prompts} rowHeight={108} label="Prompt history" emptyState={<div className="empty-state"><h2>Your instructions, saved</h2><p>Objectives and follow-up prompts appear here. Reuse any prompt as a draft, or restore its saved workspace snapshot while idle.</p></div>} getKey={item => item.seq} render={item => <div className="prompt-row"><Button className="message" onClick={() => setExpanded(item)}><strong>Prompt #{item.seq}</strong><p>{item.body}</p></Button><Button onClick={() => { setDraft(item.body); notify("Prompt copied into your draft. Review it before sending."); objectiveInput.current?.focus(); }}>Use draft</Button><Button disabled={busy || !idle} title={idle ? "Restore this prompt and its saved workspace snapshot" : "Stop the run before restoring a workspace snapshot"} onClick={() => setConfirmation({ title: "Restore this prompt?", description: "This restores the prompt and its available Git workspace snapshot. Workspace files may change. Review or save your current work first.", label: "Restore snapshot", action: () => { void control("restore", { sequence: item.seq }); } })}>Restore</Button></div>} />}
          {tab === "Sessions" && <SessionNavigator snapshot={snapshot} busy={busy} onNew={confirmNew} onOpen={id => control("session", { id })} />}
          {tab === "Settings" && <div className="settings"><section className="settings-section"><div className="section-heading"><div><h2>OpenCode import</h2><p>Use existing OpenCode credentials and provider settings after confirmation.</p></div><Button disabled={busy} onClick={() => requestImport(snapshot.workspace)}>Import from OpenCode</Button></div></section><ModelPicker key={importRevision} provider={snapshot.provider} model={snapshot.model} variant={snapshot.variant ?? null} busy={busy} onSelect={(provider, model, variant) => control("model", { provider, model, variant })} /><ThemePicker busy={busy} onSelect={name => control("theme", { name })} /><SettingsPanel key={importRevision} busy={busy} onControl={control} /></div>}
          </TabPanel>
        <div className="composer-dock" hidden={tab === "Settings" || tab === "Sessions"}>
      <form className="composer" onSubmit={event => { event.preventDefault(); if (draft.trim()) void task(async () => { const text = draft; const before = revision.current; const result = await native.control({ action: "start", text }); if (isSnapshot(result) && before === revision.current) setSnapshot(result); setDraft(""); }); }}>
        <TextArea aria-label={idle ? "Objective" : "Follow-up instruction"} textAreaRef={objectiveInput} placeholder={idle ? "Ask your agents to do anything…" : "Send a follow-up instruction…"} value={draft} onChange={setDraft} isDisabled={stopping || busy} size="sm" rows={2} textAreaClassName="composer-input" onKeyDown={event => { if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) { event.preventDefault(); if (!busy && !stopping && draft.trim()) objectiveInput.current?.form?.requestSubmit(); } }} />
        <div className="composer-toolbar">
          <ModelMenu key={importRevision} snapshot={snapshot} busy={busy} onSelect={(provider, model, variant) => control("model", { provider, model, variant })} />
          <Button type="submit" className="primary send-button" aria-label={idle ? "Start swarm" : "Send prompt"} title={idle ? "Start swarm" : "Send prompt"} disabled={busy || stopping || !draft.trim()}><ShellIcon name="arrow" /></Button>
        </div>
      </form>
          <div className="composer-footer"><span><ShellIcon name="folder" />{projectName}</span><span className="lifecycle-hint">{idle ? "Ready to start" : stopping ? "Stopping agents…" : snapshot.state === "PAUSED" ? "Paused · resume to continue" : "Live updates"}</span></div>
        </div>
        </section>
      </Tabs></main>
    </>}
    {confirmation && <ConfirmDialog title={confirmation.title} description={confirmation.description} confirmLabel={confirmation.label} onConfirm={confirmation.action} onClose={() => setConfirmation(null)} />}
    {importPrompt && <ConfirmDialog title="Use existing OpenCode settings?" description="OpenRaid can use the credentials, provider keys, and configurations found in OpenCode. Choose Import from OpenCode to enable access, or keep your OpenRaid setup separate. You can import later from Settings." confirmLabel="Import from OpenCode" cancelLabel={importPrompt.startup ? "Keep separate" : "Cancel"} confirmClassName="primary" onConfirm={() => { void decideImport(true); }} onCancel={importPrompt.startup ? () => { void decideImport(false); } : undefined} onClose={() => setImportPrompt(null)} />}
    {expanded && <MessageDialog message={expanded} onClose={() => setExpanded(null)} />}
  </div>;
}
