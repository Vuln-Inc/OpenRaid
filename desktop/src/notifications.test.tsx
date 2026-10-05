// @vitest-environment jsdom
import { afterEach, beforeAll, expect, it, vi } from "vitest";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "@testing-library/jest-dom/vitest";
import { toast } from "sonner";
import { Notifications, notify, notifyError } from "./notifications";

beforeAll(() => {
  vi.stubGlobal("matchMedia", vi.fn(() => ({ matches: false, addEventListener() {}, removeEventListener() {} })));
  // JSDOM does not implement browser pointer capture used by Sonner's swipe gestures.
  Element.prototype.setPointerCapture = vi.fn();
  Element.prototype.releasePointerCapture = vi.fn();
});
afterEach(() => { toast.dismiss(); cleanup(); });

it("uses a themed Sonner overlay with dismissible success notifications", async () => {
  const view = render(<Notifications theme="dark" />);
  act(() => { notify("Saved session opened."); });
  const title = await screen.findByText("Saved session opened.");
  const item = title.closest("[data-sonner-toast]")!;
  expect(item).toHaveAttribute("data-type", "success");
  expect(document.querySelector("[data-sonner-toaster]")).toHaveAttribute("data-sonner-theme", "dark");
  view.rerender(<Notifications theme="light" />);
  expect(document.querySelector("[data-sonner-toaster]")).toHaveAttribute("data-sonner-theme", "light");
  await userEvent.click(screen.getByRole("button", { name: "Close toast" }));
  await waitFor(() => expect(screen.queryByText("Saved session opened.")).toBeNull());
});

it("coalesces repeated errors and clears them without clearing success notifications", async () => {
  render(<Notifications theme="dark" />);
  act(() => { notify("Board exported."); notifyError("Native request failed."); notifyError("Native request failed."); });
  await screen.findByText("Native request failed.");
  expect(screen.getAllByText("Native request failed.")).toHaveLength(1);
  expect(document.querySelector('[data-sonner-toast][data-type="error"]')).not.toBeNull();
  act(() => { notifyError(""); });
  await waitFor(() => expect(screen.queryByText("Native request failed.")).toBeNull());
  expect(screen.getByText("Board exported.")).toBeInTheDocument();
});
