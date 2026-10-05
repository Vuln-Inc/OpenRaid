import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { ThemePicker, filterThemes, themeName } from "./ThemePicker";

const themes = [
  { id: "midnight", name: "Midnight", dark: true },
  { id: "soft_light", dark: false },
  { id: "another-dark", name: "Aurora", dark: true },
];

describe("theme selection", () => {
  it("uses the shared human-readable name with a friendly fallback", () => {
    expect(themeName(themes[0])).toBe("Midnight");
    expect(themeName(themes[1])).toBe("Soft Light");
  });

  it("searches names, identifiers, and light/dark appearance without case sensitivity", () => {
    expect(filterThemes(themes, " AURORA ")).toEqual([themes[2]]);
    expect(filterThemes(themes, "soft_light")).toEqual([themes[1]]);
    expect(filterThemes(themes, "Light")).toEqual([themes[1]]);
    expect(filterThemes(themes, "dark")).toEqual([themes[0], themes[2]]);
    expect(filterThemes(themes, "unknown")).toEqual([]);
    expect(filterThemes(themes, "")).toEqual(themes);
  });

  it("announces initial loading and exposes a labeled search without raw identifiers", () => {
    const html = renderToStaticMarkup(<ThemePicker onSelect={async () => {}} />);
    expect(html).toContain('aria-busy="true"');
    expect(html).toContain('role="status"');
    expect(html).toContain("Loading available themes");
    expect(html).toContain("Find a theme");
    expect(html).toContain('type="search"');
    expect(html).not.toContain("Theme name");
  });
});
