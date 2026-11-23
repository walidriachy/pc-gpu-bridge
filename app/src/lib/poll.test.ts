import { describe, expect, it } from "vitest";
import { MISSES_BEFORE_BAD, steady } from "./poll";

const ok = (d: { up: boolean }) => d.up;

describe("steady status", () => {
  it("keeps the last good answer through a couple of bad ones", () => {
    const good = { up: true };
    const bad = { up: false };
    let shown: { up: boolean } = good;
    let misses = 0;
    for (let i = 1; i < MISSES_BEFORE_BAD; i++) {
      [shown, misses] = steady(shown, bad, misses, ok);
      expect(shown).toBe(good);
    }
    [shown, misses] = steady(shown, bad, misses, ok);
    expect(shown).toBe(bad);
    [shown, misses] = steady(shown, good, misses, ok);
    expect(shown).toBe(good);
    expect(misses).toBe(0);
  });
  it("shows a bad first answer straight away", () => {
    expect(steady(null, { up: false }, 0, ok)[0]).toEqual({ up: false });
  });
});
