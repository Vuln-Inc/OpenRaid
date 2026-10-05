// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "@testing-library/jest-dom/vitest";
import { ConfirmDialog } from "./ConfirmDialog";
import { MessageDialog } from "./MessageDialog";
import { SettingsPanel } from "./SettingsPanel";

afterEach(cleanup);

describe("desktop safety and utility panels", () => {
  it("uses a labeled Untitled UI modal with a safe cancel action", async () => {
    const onConfirm = vi.fn(); const onClose = vi.fn();
    render(<ConfirmDialog title="Stop this run?" description="Cancel current operations." confirmLabel="Stop run" onConfirm={onConfirm} onClose={onClose} />);
    const dialog = screen.getByRole("dialog", { name: "Stop this run?" });
    expect(dialog).toHaveAccessibleDescription("Cancel current operations.");
    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onClose).toHaveBeenCalledOnce(); expect(onConfirm).not.toHaveBeenCalled();
  });
  it("renders full message content safely, without interpreting HTML", () => {
    render(<MessageDialog message={{ seq: 1, sender: "agent-001", body: "<script>unsafe()</script>\nfull message", owner: false, created_at_ms: 1 }} onClose={() => {}} />);
    const dialog = screen.getByRole("dialog", { name: "agent-001 · #1" });
    expect(dialog).toHaveTextContent("<script>unsafe()</script>");
    expect(dialog.querySelector("script")).toBeNull();
    expect(dialog).toHaveTextContent("full message");
  });
  it("confirms once and dismisses with Escape", async () => {
    const onConfirm = vi.fn(); const onClose = vi.fn();
    render(<ConfirmDialog title="Create session?" description="History stays saved." confirmLabel="Create" onConfirm={onConfirm} onClose={onClose} />);
    await userEvent.click(screen.getByRole("button", { name: "Create" }));
    expect(onConfirm).toHaveBeenCalledOnce(); expect(onClose).toHaveBeenCalledOnce();
    await userEvent.keyboard("{Escape}");
    expect(onClose).toHaveBeenCalledTimes(2);
  });
  it("exposes named export and MCP actions with a loading state, not raw JSON", () => {
    const html = renderToStaticMarkup(<SettingsPanel busy={false} onControl={async () => {}} />);
    expect(html).toContain("Export location");
    expect(html).toContain("Export board");
    expect(html).toContain("MCP connections");
    expect(html).toContain('role="status"');
    expect(html).not.toContain("Configured server name");
  });
});
