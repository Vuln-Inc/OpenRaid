// @vitest-environment jsdom
import { afterEach, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "@testing-library/jest-dom/vitest";
import { App } from "./App";
import { native, type Snapshot } from "./bridge";

vi.mock("./bridge", async importOriginal => {
  const original = await importOriginal<typeof import("./bridge")>();
  return { ...original, native: { snapshot: vi.fn(), subscribe: vi.fn(), errors: vi.fn().mockResolvedValue(() => {}), query: vi.fn(), control: vi.fn(), opencodeStatus: vi.fn().mockResolvedValue({ available: false, consent: null }) } };
});
const snapshot: Snapshot = {
  state: "IDLE", workspace: "D:\\workspace", database: "D:\\workspace\\session.sqlite3", session_id: "session-test",
  provider: "codex-pool", model: "gpt-6.1-sol", variant: "medium", agents: [], votes: [], board: [], prompts: [], latest_seq: 0, runtime_error: null,
};
let publish: (value: Snapshot) => void;
beforeAll(() => { vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} }); });
beforeEach(() => {
  vi.mocked(native.snapshot).mockResolvedValue(null);
  vi.mocked(native.snapshot).mockClear();
  vi.mocked(native.subscribe).mockImplementation(async callback => { publish = callback; return () => {}; });
  vi.mocked(native.query).mockImplementation(async name => name === "themes" ? { active: "dark", themes: [{ id: "dark", dark: true }] } : []);
  vi.mocked(native.control).mockResolvedValue(snapshot);
  vi.mocked(native.control).mockClear();
});
afterEach(cleanup);

it("reattaches to the running native session after a frontend reload", async () => {
  vi.mocked(native.snapshot).mockResolvedValue({ ...snapshot, state: "PAUSED" });
  const first = render(<App />);
  expect(await screen.findByRole("status", { name: "Session state: PAUSED" })).toHaveTextContent("Paused");
  expect(screen.queryByRole("button", { name: "Open workspace" })).toBeNull();
  first.unmount();
  render(<App />);
  await screen.findByRole("status", { name: "Session state: PAUSED" });
  expect(native.snapshot).toHaveBeenCalledTimes(2);
  expect(native.control).not.toHaveBeenCalled();
});

it("sends with Enter while Shift+Enter keeps a multiline draft", async () => {
  render(<App />);
  await act(async () => { publish(snapshot); });
  const input = screen.getByRole("textbox", { name: "Objective" });
  await userEvent.type(input, "First line");
  await userEvent.keyboard("{Shift>}{Enter}{/Shift}Second line");
  expect(native.control).not.toHaveBeenCalled();
  expect(input).toHaveValue("First line\nSecond line");
  await userEvent.keyboard("{Enter}");
  expect(native.control).toHaveBeenCalledWith({ action: "start", text: "First line\nSecond line" });
  expect(input).toHaveValue("");
});

it("does not overwrite a newer event with a delayed startup snapshot", async () => {
  let resolve!: (value: Snapshot | null) => void;
  vi.mocked(native.snapshot).mockReturnValue(new Promise(done => { resolve = done; }));
  render(<App />);
  await waitFor(() => expect(native.snapshot).toHaveBeenCalled());
  await act(async () => { publish({ ...snapshot, state: "RUNNING", session_id: "newer-session" }); });
  await act(async () => { resolve({ ...snapshot, state: "IDLE", session_id: "stale-session" }); });
  expect(screen.getByRole("status", { name: "Session state: RUNNING" })).toHaveTextContent("Running");
  expect(screen.getByText("newer-session")).toBeInTheDocument();
  expect(screen.queryByText("stale-session")).toBeNull();
});

it("preserves an event delivered while the initial subscription is being established", async () => {
  vi.mocked(native.subscribe).mockImplementation(async callback => {
    callback({ ...snapshot, state: "RUNNING" });
    return () => {};
  });
  render(<App />);
  await waitFor(() => expect(native.snapshot).toHaveBeenCalled());
  expect(screen.getByRole("status", { name: "Session state: RUNNING" })).toHaveTextContent("Running");
  expect(screen.queryByRole("button", { name: "Open workspace" })).toBeNull();
});

it("removes native listeners when unmounted during startup hydration", async () => {
  const unsubscribe = vi.fn();
  const unsubscribeErrors = vi.fn();
  vi.mocked(native.subscribe).mockImplementation(async callback => { publish = callback; return unsubscribe; });
  vi.mocked(native.errors).mockResolvedValueOnce(unsubscribeErrors);
  let resolve!: (value: Snapshot | null) => void;
  vi.mocked(native.snapshot).mockReturnValue(new Promise(done => { resolve = done; }));
  const view = render(<App />);
  await waitFor(() => expect(native.snapshot).toHaveBeenCalled());
  view.unmount();
  await act(async () => { resolve(snapshot); });
  expect(unsubscribe).toHaveBeenCalledTimes(1);
  expect(unsubscribeErrors).toHaveBeenCalledTimes(1);
});

it("keeps a fresh native host on the welcome screen", async () => {
  render(<App />);
  expect(await screen.findByRole("button", { name: "Open workspace" })).toBeEnabled();
});

it("shows a quiet, accessible session status that follows runtime events", async () => {
  render(<App />);
  expect(screen.getByRole("status", { name: "Session state: not connected" })).toHaveTextContent("Connecting to desktop");
  for (const [state, label] of [["IDLE", "Idle"], ["RUNNING", "Running"], ["PAUSED", "Paused"], ["STOPPING", "Stopping"]] as const) {
    await act(async () => { publish({ ...snapshot, state }); });
    const status = screen.getByRole("status", { name: `Session state: ${state}` });
    expect(status).toHaveTextContent(label);
    expect(status).toHaveAttribute("data-state", state);
    expect(status.querySelector("svg")).toHaveAttribute("aria-hidden", "true");
    expect(status).not.toHaveClass("rounded-full");
  }
});

it("resumes an idle reopened unfinished task without sending another prompt", async () => {
  vi.mocked(native.snapshot).mockResolvedValue(snapshot);
  render(<App />);
  await screen.findByRole("status", { name: "Session state: IDLE" });
  expect(screen.getByRole("button", { name: "Resume" })).toBeDisabled();
  await act(async () => { publish({ ...snapshot, resumable: true }); });
  const resume = screen.getByRole("button", { name: "Resume saved task" });
  expect(resume).toBeEnabled();
  await userEvent.click(resume);
  expect(native.control).toHaveBeenCalledExactlyOnceWith({ action: "resume" });
  expect(screen.getByRole("textbox", { name: "Objective" })).toHaveValue("");
  await act(async () => { publish({ ...snapshot, state: "RUNNING", resumable: true }); });
  expect(screen.getByRole("button", { name: "Resume" })).toBeDisabled();
});

it("shows the active reasoning variant next to the model and updates from pushed snapshots", async () => {
  render(<App />);
  await act(async () => { publish(snapshot); });
  expect(await screen.findByLabelText("Reasoning variant: medium")).toHaveTextContent("medium");
  const summary = document.querySelector(".model-label")!;
  expect(summary).toHaveTextContent("codex-pool / gpt-6.1-sol");
  expect(summary).toHaveAttribute("title", "codex-pool / gpt-6.1-sol · Reasoning: medium");
  await act(async () => { publish({ ...snapshot, variant: "high" }); });
  expect(screen.getByLabelText("Reasoning variant: high")).toHaveTextContent("high");
  await act(async () => { publish({ ...snapshot, variant: null }); });
  expect(screen.getByLabelText("Reasoning variant: Default")).toHaveTextContent("Default");
});

it("keeps controls, views and composer together beside the independent roster", async () => {
  render(<App />);
  await act(async () => { publish(snapshot); });
  const workspace = document.querySelector(".workspace")!;
  const roster = screen.getByRole("complementary", { name: "Agents" });
  const panel = screen.getByRole("region", { name: "Session workspace" });
  expect(roster.parentElement).toBe(workspace);
  expect(panel.parentElement).toBe(workspace);
  expect(panel).toContainElement(screen.getByLabelText("Session controls"));
  expect(roster).toContainElement(screen.getByRole("tablist", { name: "Workspace views" }));
  expect(panel).toContainElement(screen.getByRole("textbox", { name: "Objective" }));
  expect(panel).toContainElement(screen.getByRole("button", { name: "Start swarm" }));
  expect(roster.querySelector(".roster-header")).toContainElement(screen.getByRole("heading", { name: "Agents 0" }));
  expect(roster.querySelector(".roster-header")).toContainElement(screen.getByRole("button", { name: "Add agents" }));
  expect(roster.querySelector(".roster-body")).toContainElement(screen.getByRole("region", { name: "Agent roster" }));
  expect(roster).not.toContainElement(screen.getByRole("textbox", { name: "Objective" }));
  await userEvent.type(screen.getByRole("textbox", { name: "Objective" }), "Review the workspace");
  await userEvent.click(screen.getByRole("button", { name: "Start swarm" }));
  expect(native.control).toHaveBeenCalledWith({ action: "start", text: "Review the workspace" });
});

it("uses vertical sidebar navigation and keeps the draft when opening the composer model menu", async () => {
  render(<App />);
  await act(async () => { publish(snapshot); });
  expect(screen.getByRole("tablist", { name: "Workspace views" })).toHaveAttribute("aria-orientation", "vertical");
  await userEvent.type(screen.getByRole("textbox", { name: "Objective" }), "Keep this draft");
  await userEvent.click(screen.getByRole("button", { name: "Model settings" }));
  expect(screen.getByRole("dialog", { name: "Model settings" })).toBeInTheDocument();
  await userEvent.click(screen.getByRole("button", { name: "Done" }));
  expect(screen.getByRole("textbox", { name: "Objective" })).toHaveValue("Keep this draft");
  await userEvent.click(screen.getByRole("tab", { name: "Settings" }));
  expect(screen.queryByRole("textbox", { name: "Objective" })).toBeNull();
});

it("collapses sidebar and project roster without touching the active native session", async () => {
  render(<App />);
  await act(async () => { publish(snapshot); });
  const toggle = screen.getByRole("button", { name: "Toggle agents for workspace" });
  await userEvent.click(toggle);
  expect(toggle).toHaveAttribute("aria-expanded", "false");
  expect(screen.queryByRole("region", { name: "Agent roster" })).toBeNull();
  await userEvent.click(toggle);
  expect(screen.getByRole("region", { name: "Agent roster" })).toBeInTheDocument();
  await userEvent.click(screen.getAllByRole("button", { name: "Collapse sidebar" })[0]);
  expect(document.querySelector(".app")).toHaveAttribute("data-sidebar", "closed");
  await userEvent.click(screen.getByRole("button", { name: "Expand sidebar" }));
  expect(document.querySelector(".app")).toHaveAttribute("data-sidebar", "open");
  expect(native.control).not.toHaveBeenCalled();
});
