// @vitest-environment jsdom
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "@testing-library/jest-dom/vitest";
import { SearchSelect, Button } from "./ui";

beforeAll(() => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  Element.prototype.scrollIntoView = vi.fn();
});
afterEach(cleanup);

describe("Untitled UI controls", () => {
  const options = [{ id: "openai", label: "OpenAI" }, { id: "anthropic", label: "Anthropic" }, { id: "offline", label: "Offline · unavailable", disabled: true }];
  it("searches and selects in one accessible combobox", async () => {
    const onChange = vi.fn();
    render(<SearchSelect label="Provider" value="openai" options={options} onChange={onChange} />);
    const input = screen.getByRole("combobox", { name: "Provider" });
    expect(input).toHaveValue("OpenAI");
    await userEvent.clear(input); await userEvent.type(input, "anth");
    expect(await screen.findByRole("option", { name: "Anthropic" })).toBeVisible();
    expect(screen.queryByRole("option", { name: "OpenAI" })).toBeNull();
    await userEvent.keyboard("{ArrowDown}{Enter}");
    expect(onChange).toHaveBeenLastCalledWith("anthropic");
  });
  it("opens the full list with its arrow and reports no search matches", async () => {
    render(<SearchSelect label="Model" value="" options={options} onChange={() => {}} />);
    await userEvent.click(screen.getByRole("button", { name: /Show options/ }));
    expect(await screen.findByRole("option", { name: "OpenAI" })).toBeVisible();
    expect(screen.getByRole("option", { name: "Offline · unavailable" })).toHaveAttribute("aria-disabled", "true");
    await userEvent.type(screen.getByRole("combobox"), "no-such-model");
    expect(await screen.findByText("No matching options.")).toBeVisible();
  });
  it("keeps disabled selections and actions non-interactive", async () => {
    const change = vi.fn(); const action = vi.fn();
    render(<><SearchSelect label="Model" value="openai" options={options} disabled onChange={change} /><Button disabled onClick={action}>Apply</Button></>);
    expect(screen.getByRole("combobox")).toBeDisabled();
    await userEvent.click(screen.getByRole("button", { name: "Apply" }));
    expect(action).not.toHaveBeenCalled(); expect(change).not.toHaveBeenCalled();
  });
  it("keeps disabled primary actions opaque with a separate readable neutral palette", () => {
    render(<Button className="primary" disabled>Apply model selection</Button>);
    const button = screen.getByRole("button", { name: "Apply model selection" });
    expect(button).toBeDisabled();
    expect(button).toHaveClass("disabled:opacity-100", "disabled:bg-secondary", "disabled:text-secondary");
    expect(button).not.toHaveClass("disabled:opacity-50");
  });
});
