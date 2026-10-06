// @vitest-environment jsdom
import { afterEach, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "@testing-library/jest-dom/vitest";
import { App } from "./App";
import { native, type Snapshot } from "./bridge";

vi.mock("./bridge", async importOriginal => {
  const original = await importOriginal<typeof import("./bridge")>();
  return { ...original, native: { snapshot: vi.fn(), subscribe: vi.fn(), errors: vi.fn(), query: vi.fn(), control: vi.fn(), open: vi.fn(), opencodeStatus: vi.fn(), importOpencode: vi.fn() } };
});
const snapshot: Snapshot = {
  state: "IDLE", workspace: "C:\\project", database: "C:\\project\\session.sqlite3", session_id: "saved-session",
  provider: "openai", model: "example-model", agents: [], votes: [], board: [], prompts: [], latest_seq: 0, runtime_error: null,
};
beforeAll(() => { vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} }); });
beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(native.snapshot).mockResolvedValue(null);
  vi.mocked(native.subscribe).mockResolvedValue(() => {});
  vi.mocked(native.errors).mockResolvedValue(() => {});
  vi.mocked(native.query).mockImplementation(async name => name === "themes" ? { active: "dark", themes: [{ id: "dark", dark: true }] } : []);
  vi.mocked(native.opencodeStatus).mockResolvedValue({ available: false, consent: null });
  vi.mocked(native.importOpencode).mockResolvedValue({ available: true, consent: true });
  vi.mocked(native.open).mockResolvedValue(snapshot);
});
afterEach(cleanup);

it("asks on first launch and focuses Keep separate without importing anything", async () => {
  vi.mocked(native.opencodeStatus).mockResolvedValue({ available: true, consent: null });
  render(<App />);
  const dialog = await screen.findByRole("dialog", { name: "Use existing OpenCode settings?" });
  await waitFor(() => expect(within(dialog).getByRole("button", { name: "Keep separate" })).toHaveFocus());
  expect(native.importOpencode).not.toHaveBeenCalled();
  expect(native.open).not.toHaveBeenCalled();
  await userEvent.click(within(dialog).getByRole("button", { name: "Keep separate" }));
  await waitFor(() => expect(native.importOpencode).toHaveBeenCalledExactlyOnceWith(".", false));
  expect(native.open).not.toHaveBeenCalled();
});

it("persists a decline before opening a workspace whose OpenCode configuration was newly found", async () => {
  vi.mocked(native.opencodeStatus).mockResolvedValueOnce({ available: false, consent: null }).mockResolvedValue({ available: true, consent: null });
  render(<App />);
  await userEvent.click(await screen.findByRole("button", { name: "Open workspace" }));
  const dialog = await screen.findByRole("dialog", { name: "Use existing OpenCode settings?" });
  expect(native.open).not.toHaveBeenCalled();
  await userEvent.click(within(dialog).getByRole("button", { name: "Keep separate" }));
  await waitFor(() => expect(native.open).toHaveBeenCalledWith(".", false));
  expect(native.importOpencode).toHaveBeenCalledWith(".", false);
  expect(vi.mocked(native.importOpencode).mock.invocationCallOrder[0]).toBeLessThan(vi.mocked(native.open).mock.invocationCallOrder[0]);
});

it("enables import only after the confirmation and before opening the workspace", async () => {
  vi.mocked(native.opencodeStatus).mockResolvedValueOnce({ available: false, consent: null }).mockResolvedValue({ available: true, consent: null });
  render(<App />);
  await userEvent.click(await screen.findByRole("button", { name: "Open workspace" }));
  const dialog = await screen.findByRole("dialog", { name: "Use existing OpenCode settings?" });
  expect(native.importOpencode).not.toHaveBeenCalled();
  await userEvent.click(within(dialog).getByRole("button", { name: "Import from OpenCode" }));
  await waitFor(() => expect(native.open).toHaveBeenCalledWith(".", false));
  expect(native.importOpencode).toHaveBeenCalledWith(".", true);
  expect(vi.mocked(native.importOpencode).mock.invocationCallOrder[0]).toBeLessThan(vi.mocked(native.open).mock.invocationCallOrder[0]);
});

it("treats Escape at first launch as keeping the setups separate", async () => {
  vi.mocked(native.opencodeStatus).mockResolvedValue({ available: true, consent: null });
  render(<App />);
  await screen.findByRole("dialog", { name: "Use existing OpenCode settings?" });
  await userEvent.keyboard("{Escape}");
  await waitFor(() => expect(native.importOpencode).toHaveBeenCalledExactlyOnceWith(".", false));
});

it("does not let a delayed startup check replace a pending workspace-open confirmation", async () => {
  let resolve!: (status: { available: boolean; consent: null }) => void;
  vi.mocked(native.opencodeStatus).mockReturnValueOnce(new Promise(done => { resolve = done; })).mockResolvedValue({ available: true, consent: null });
  render(<App />);
  await userEvent.click(await screen.findByRole("button", { name: "Open workspace" }));
  const dialog = await screen.findByRole("dialog", { name: "Use existing OpenCode settings?" });
  await act(async () => { resolve({ available: true, consent: null }); });
  await userEvent.click(within(dialog).getByRole("button", { name: "Import from OpenCode" }));
  await waitFor(() => expect(native.open).toHaveBeenCalledExactlyOnceWith(".", false));
});

it.each([true, false])("does not prompt again for a remembered consent decision of %s", async consent => {
  vi.mocked(native.opencodeStatus).mockResolvedValue({ available: true, consent });
  render(<App />);
  await userEvent.click(await screen.findByRole("button", { name: "Open workspace" }));
  await waitFor(() => expect(native.open).toHaveBeenCalledWith(".", false));
  expect(screen.queryByRole("dialog", { name: "Use existing OpenCode settings?" })).toBeNull();
  expect(native.importOpencode).not.toHaveBeenCalled();
});

it("lets Settings import later, keeps cancellation inert, and refreshes provider data after consent", async () => {
  vi.mocked(native.snapshot).mockResolvedValue(snapshot);
  vi.mocked(native.opencodeStatus).mockResolvedValue({ available: true, consent: false });
  render(<App />);
  await screen.findByRole("status", { name: "Session state: IDLE" });
  await userEvent.click(screen.getByRole("tab", { name: "Settings" }));
  await waitFor(() => expect(native.query).toHaveBeenCalledWith("catalog"));
  const initialCatalogRequests = vi.mocked(native.query).mock.calls.filter(([name]) => name === "catalog").length;
  await userEvent.click(screen.getByRole("button", { name: "Import from OpenCode" }));
  let dialog = await screen.findByRole("dialog", { name: "Use existing OpenCode settings?" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
  expect(native.importOpencode).not.toHaveBeenCalled();
  await userEvent.click(screen.getByRole("button", { name: "Import from OpenCode" }));
  dialog = await screen.findByRole("dialog", { name: "Use existing OpenCode settings?" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Import from OpenCode" }));
  await waitFor(() => expect(native.importOpencode).toHaveBeenCalledExactlyOnceWith(snapshot.workspace, true));
  await waitFor(() => expect(vi.mocked(native.query).mock.calls.filter(([name]) => name === "catalog").length).toBeGreaterThan(initialCatalogRequests));
  expect(native.control).not.toHaveBeenCalled();
  expect(native.open).not.toHaveBeenCalled();
});
