import { describe, expect, it } from "vitest";
import { cleanCode, driveName, duration, gb, jobLabel, hasRoom, sizeText, spaceNeededGb, speedup, timeAgo, usedFraction } from "./format";

describe("format helpers", () => {
  it("formats gigabytes", () => {
    expect(gb(16 * 1024 ** 3)).toBe("16.0 GB");
    expect(gb(undefined)).toBe("–");
  });
  it("formats durations in plain words", () => {
    expect(duration(0.3)).toBe("under 1 s");
    expect(duration(42)).toBe("42 s");
    expect(duration(125)).toBe("2 min 5 s");
    expect(duration(3 * 3600 + 60)).toBe("3 h 1 min");
  });
  it("formats time ago", () => {
    expect(timeAgo(1000, 1030)).toBe("just now");
    expect(timeAgo(1000, 1000 + 7200)).toBe("2 h ago");
  });
  it("computes memory use", () => {
    expect(usedFraction(16, 4)).toBe(0.75);
    expect(usedFraction(undefined, 4)).toBe(0);
  });
  it("describes speedups", () => {
    expect(speedup(10, 40)).toBe("about 4.0× faster");
    expect(speedup(10, 11)).toBe("about the same speed");
  });
  it("cleans pairing codes", () => {
    expect(cleanCode("12 34-56 7")).toBe("123456");
  });
  it("checks free space before installing a program", () => {
    expect(spaceNeededGb(5)).toBe(7);
    expect(spaceNeededGb(0.3)).toBe(3);
    expect(hasRoom(7, 5)).toBe(true);
    expect(hasRoom(6.9, 5)).toBe(false);
    expect(hasRoom(-1, 5)).toBe(true);
  });
  it("describes program sizes and drives", () => {
    expect(sizeText(0.3)).toBe("about 300 MB");
    expect(sizeText(0.2)).toBe("about 200 MB");
    expect(sizeText(5)).toBe("about 5 GB");
    expect(sizeText(2.5)).toBe("about 2.5 GB");
    expect(driveName("C:\\")).toBe("C:");
    expect(driveName("D:")).toBe("D:");
    expect(driveName("/")).toBe("/");
  });
  it("drops env setup from job names", () => {
    expect(jobLabel('$env:REEL_CHROME="$env:LOCALAPPDATA\\PCBridgeTools\\a b.exe"; $env:REEL_PYTHON=\'C:\\x\\python.exe\'; node engine/reel.mjs render v01')).toBe("node engine/reel.mjs render v01");
    expect(jobLabel("FOO=1 BAR=\"a b\" python run.py")).toBe("python run.py");
    expect(jobLabel("set FOO=1 && npm test")).toBe("npm test");
    expect(jobLabel("ffmpeg -i a.mov b.mp4")).toBe("ffmpeg -i a.mov b.mp4");
    expect(jobLabel('$env:A="x";')).toBe('$env:A="x";');
    expect(jobLabel('$d="$env:LOCALAPPDATA\\Tools\\x"; & "$d\\render.exe" --out a.mp4')).toBe('"$d\\render.exe" --out a.mp4');
    expect(jobLabel("")).toBe("job");
  });
});
