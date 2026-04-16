import { describe, expect, it } from "vitest";
import { formatEditionDate, joinLines } from "./format";

describe("format", () => {
  it("keeps edition dates stable for date-only values", () => {
    expect(formatEditionDate("2026-04-16")).toBe("Thursday, April 16");
  });

  it("removes blank lines and preserves content order", () => {
    expect(joinLines(["  one  ", "", "two", "   "])).toBe("one\ntwo");
  });
});
