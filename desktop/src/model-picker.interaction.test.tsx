// @vitest-environment jsdom
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "@testing-library/jest-dom/vitest";
import { ModelPicker } from "./ModelPicker";
import { native } from "./bridge";

vi.mock("./bridge", () => ({ native: { query: vi.fn() } }));
const catalog = [
  { id: "openai", name: "OpenAI", configured: true, models: [
    { id: "gpt-a", name: "GPT A", variants: ["high"] },
    { id: "gpt-b", name: "GPT B", variants: [] },
    { id: "blocked", name: "Blocked", variants: [], available: false },
  ] },
  { id: "anthropic", name: "Anthropic", configured: true, models: [
    { id: "claude", name: "Claude", variants: ["extended"] },
  ] },
];
beforeAll(() => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  Element.prototype.scrollIntoView = vi.fn();
});
beforeEach(() => { vi.mocked(native.query).mockResolvedValue(catalog); });
afterEach(cleanup);

async function choose(label: string, search: string, option: string) {
  const input = await screen.findByRole("combobox", { name: label });
  await userEvent.clear(input); await userEvent.type(input, search);
  await userEvent.click(await screen.findByRole("option", { name: option }));
}
describe("integrated searchable model selection", () => {
  it("changes provider, resets the model and variant, and applies typed controls", async () => {
    const apply = vi.fn().mockResolvedValue(undefined);
    render(<ModelPicker provider="openai" model="gpt-a" variant="high" busy={false} onSelect={apply} />);
    await screen.findByText("2 providers available");
    await choose("Provider", "anth", "Anthropic · anthropic");
    expect(screen.getByRole("combobox", { name: "Model" })).toHaveValue("Claude · claude");
    expect(screen.getByRole("combobox", { name: "Reasoning variant" })).toHaveValue("Default · provider preference");
    await choose("Reasoning variant", "extended", "extended");
    await userEvent.click(screen.getByRole("button", { name: "Apply model selection" }));
    expect(apply).toHaveBeenCalledWith("anthropic", "claude", "extended");
    expect(document.querySelectorAll('.model-picker input[type="search"], .model-picker select')).toHaveLength(0);
  });
  it("clears dependent variants when choosing another model", async () => {
    render(<ModelPicker provider="openai" model="gpt-a" variant="high" busy={false} onSelect={async () => {}} />);
    await screen.findByText("2 providers available");
    await choose("Model", "gpt-b", "GPT B · gpt-b");
    const variant = screen.getByRole("combobox", { name: "Reasoning variant" });
    expect(variant).toHaveValue("Default · provider preference");
    expect(variant).toBeDisabled();
  });
  it("marks unavailable models disabled and reports catalog failures with retry", async () => {
    const view = render(<ModelPicker provider="openai" model="gpt-a" variant={null} busy={false} onSelect={async () => {}} />);
    await screen.findByText("2 providers available");
    const input = screen.getByRole("combobox", { name: "Model" });
    await userEvent.clear(input); await userEvent.type(input, "blocked");
    expect(await screen.findByRole("option", { name: "Blocked · blocked · unavailable" })).toHaveAttribute("aria-disabled", "true");
    view.unmount();
    vi.mocked(native.query).mockRejectedValueOnce(new Error("Catalog unavailable"));
    render(<ModelPicker provider="openai" model="gpt-a" variant={null} busy={false} onSelect={async () => {}} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("Catalog unavailable");
    await userEvent.click(screen.getByRole("button", { name: "Refresh catalog" }));
    await screen.findByText("2 providers available");
    expect(screen.queryByRole("alert")).toBeNull();
  });
});
