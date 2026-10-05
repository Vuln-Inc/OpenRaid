import { describe, expect, it } from "vitest";
import { keyboardRow } from "./virtual-keyboard";

describe("virtual list keyboard navigation", () => {
  it("can reach either end of a 500-agent roster", () => {
    expect(keyboardRow("End", 0, 500, 5)).toBe(499);
    expect(keyboardRow("Home", 499, 500, 5)).toBe(0);
  });

  it("moves by rows and bounds navigation to available entries", () => {
    expect(keyboardRow("ArrowDown", 12, 500, 5)).toBe(13);
    expect(keyboardRow("ArrowUp", 12, 500, 5)).toBe(11);
    expect(keyboardRow("ArrowDown", 499, 500, 5)).toBe(499);
    expect(keyboardRow("ArrowUp", 0, 500, 5)).toBe(0);
    expect(keyboardRow("ArrowDown", -1, 500, 5)).toBe(0);
  });

  it("moves a visible page and handles smaller-than-one-row viewports", () => {
    expect(keyboardRow("PageDown", 12, 500, 5.9)).toBe(17);
    expect(keyboardRow("PageUp", 12, 500, 5.9)).toBe(7);
    expect(keyboardRow("PageDown", 499, 500, 5)).toBe(499);
    expect(keyboardRow("PageUp", 3, 500, 5)).toBe(0);
    expect(keyboardRow("PageDown", 0, 500, 0.2)).toBe(1);
  });

  it("does not capture ordinary controls or navigate an empty list", () => {
    for (const key of ["Tab", "Enter", "Escape", " ", "toString"]) expect(keyboardRow(key, 0, 500, 5)).toBeNull();
    expect(keyboardRow("ArrowDown", 0, 0, 5)).toBeNull();
    expect(keyboardRow("End", 0, 0, 5)).toBeNull();
  });
});
