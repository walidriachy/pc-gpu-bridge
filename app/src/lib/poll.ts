import { useEffect, useRef, useState } from "react";
import { errText } from "./api";

/** Last answer per page, so coming back to a tab shows it at once while a fresh one loads. */
const last = new Map<string, unknown>();

/** How many "not OK" answers in a row before the page believes them (about 15 s at 5 s polling). */
export const MISSES_BEFORE_BAD = 3;

/**
 * Keep the last good answer through a few bad ones: one slow reply while a job keeps the PC busy
 * shouldn't flip the status to red and back. Returns the answer to show and the new miss count.
 */
export function steady<T>(prev: T | null, next: T, misses: number, isOk?: (d: T) => boolean): [T, number] {
  if (!isOk || isOk(next)) return [next, 0];
  const n = misses + 1;
  if (prev != null && isOk(prev) && n < MISSES_BEFORE_BAD) return [prev, n];
  return [next, n];
}

/** Poll `fn` every `ms`; `isOk` marks answers that mean "all fine" (see {@link steady}). */
export function usePoll<T>(
  key: string,
  fn: () => Promise<T>,
  ms: number,
  isOk?: (d: T) => boolean,
): { data: T | null; err: string; reload: () => void } {
  const [data, setData] = useState<T | null>(() => (last.get(key) as T | undefined) ?? null);
  const [err, setErr] = useState("");
  const misses = useRef(0);
  const shown = useRef<T | null>(data);
  const alive = useRef(true);
  const tick = () =>
    fn()
      .then((d) => {
        if (!alive.current) return;
        const [show, n] = steady(shown.current, d, misses.current, isOk);
        misses.current = n;
        shown.current = show;
        last.set(key, show);
        setData(show);
        setErr("");
      })
      .catch((e) => alive.current && setErr(errText(e)));
  useEffect(() => {
    alive.current = true;
    tick();
    const t = setInterval(tick, ms);
    return () => {
      alive.current = false;
      clearInterval(t);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);
  return { data, err, reload: tick };
}
