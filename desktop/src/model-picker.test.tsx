import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { catalogLabel, defaultModel, modelVariants, ModelPicker } from "./ModelPicker";

describe("model selector", () => {
  it("only exposes variants for the selected model", () => {
    expect(modelVariants(undefined)).toEqual([]);
    expect(modelVariants({ id: "a", name: "A", variants: ["low", "high"] })).toEqual(["low", "high"]);
    expect(modelVariants({ id: "b", name: "B", variants: [] })).toEqual([]);
  });
  it("picks an available model on provider changes and handles empty providers", () => {
    expect(defaultModel(undefined)).toBe("");
    expect(defaultModel({ id: "p", name: "Provider", models: [] })).toBe("");
    expect(defaultModel({ id: "p", name: "Provider", models: [
      { id: "blocked", name: "Unavailable", variants: [], available: false },
      { id: "ready", name: "Ready", variants: [] },
    ] })).toBe("ready");
  });
  it("distinguishes options with identical human names without repeating identical ids", () => {
    expect(catalogLabel({ id: "release-a", name: "Reasoner" })).toBe("Reasoner · release-a");
    expect(catalogLabel({ id: "release-b", name: "Reasoner" })).toBe("Reasoner · release-b");
    expect(catalogLabel({ id: "mock", name: "mock" })).toBe("mock");
  });
  it("shows current selection and accessible loading controls without secret inputs", () => {
    const html = renderToStaticMarkup(<ModelPicker provider="mock" model="demo" variant="high" busy={false} onSelect={async () => {}} />);
    expect(html).toContain("mock / demo");
    expect(html).toContain("Loading available providers and models");
    expect(html).toContain('role="combobox"');
    expect(html).toContain("Search provider…");
    expect(html).not.toContain("Find a provider");
    expect(html).not.toContain("Find a model");
    expect(html).not.toContain("<select");
    expect(html).toContain("Reasoning variant");
    expect(html).toContain('aria-busy="true"');
    expect(html).not.toContain('type="password"');
  });
});
