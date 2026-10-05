import { describe, expect, it } from "vitest";
import { displayPath } from "./display-path";

describe("displayPath", () => {
  it("displays Windows drive paths without the verbatim prefix", () => {
    expect(displayPath("\\\\?\\D:\\vuln-inc\\openraid-playground")).toBe("D:\\vuln-inc\\openraid-playground");
  });
  it("preserves UNC network paths", () => {
    expect(displayPath("\\\\?\\UNC\\server\\share")).toBe("\\\\server\\share");
  });
  it("leaves ordinary paths and other Windows namespaces unchanged", () => {
    for (const path of ["C:\\workspace", "/home/workspace", "\\\\server\\share", "\\\\?\\Volume{abc}\\"]) {
      expect(displayPath(path)).toBe(path);
    }
  });
});
