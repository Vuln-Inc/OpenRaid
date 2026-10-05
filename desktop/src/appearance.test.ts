import { describe, expect, it } from "vitest";
import { appearanceStyle, desktopPalette } from "./appearance";

describe("desktop design tokens", () => {
  it("uses neutral reference palettes for built-in desktop defaults", () => {
    for (const id of ["openraid", "dark"]) {
      expect(desktopPalette({ id, dark: true, palette: { accent: "purple" } })).toMatchObject({ background: "#161616", surface: "#202020", accent: "#ffffff" });
    }
    expect(desktopPalette({ id: "light", dark: false })).toMatchObject({ surface: "#ffffff", accent: "#000000" });
  });
  it("preserves other shared runtime theme colors", () => {
    const palette = { accent: "#ebbcba", surface: "#191724" };
    expect(desktopPalette({ id: "rose-pine", dark: true, palette })).toBe(palette);
    expect(appearanceStyle(palette)).toEqual({ "--accent": "#ebbcba", "--panel": "#191724" });
  });
});
