import { useEffect, useReducer, useRef, useState } from 'preact/hooks';

/** Re-renders the component whenever `subscribe` fires. */
export function useSubscription(subscribe: (fn: () => void) => () => void, deps: unknown[]): void {
  const [, bump] = useReducer((n: number) => n + 1, 0);
  useEffect(() => {
    const off = subscribe(() => bump(0));
    // Effects run after the first paint: catch up on changes made in between.
    bump(0);
    return off;
  }, deps);
}

/** The wall clock, `now()` if given (tests), ticking every `ms` while `on`. */
export function useNow(ms: number, on = true, now: () => number = Date.now): number {
  const [t, setT] = useState(now);
  useEffect(() => {
    if (!on) return;
    setT(now());
    const id = setInterval(() => setT(now()), ms);
    return () => clearInterval(id);
  }, [ms, on, now]);
  return on ? t : now();
}

/** How long a count runs up to a new value: the time the previous value took to come,
 *  within these bounds (the VS Code tab's own for its thinking estimate). */
const RUN_MIN_MS = 300;
const RUN_MAX_MS = 3000;

/** A count that runs up to each new value instead of jumping to it (`useRunUp`): `update`
 *  takes the value at time `t`, `at` gives the number to show at time `t`. */
export interface RunUp {
  update(value: number | undefined, t: number): void;
  at(t: number): number | undefined;
}

/** As the VS Code tab runs its thinking estimate: a larger value runs up from the number shown
 *  now, linearly, over the time since the previous value came (`RUN_MIN_MS`–`RUN_MAX_MS`), so
 *  values that come every couple of seconds read as one steady count. The first value, a
 *  smaller one (a new thinking block counts from 0) and none are shown at once. */
export function runUp(): RunUp {
  let to: number | undefined;
  let from = 0;
  let start = 0;
  let span = 0;
  let last = 0;
  const at = (t: number): number | undefined => {
    if (to === undefined || span <= 0) return to;
    // `t` before `start`: `useNow`'s clock of the render before its interval started.
    return Math.floor(from + (to - from) * Math.min(1, Math.max(0, (t - start) / span)));
  };
  return {
    at,
    update(value, t) {
      if (value === to) return;
      if (value !== undefined && to !== undefined && value > to) {
        from = at(t)!;
        start = t;
        span = Math.min(RUN_MAX_MS, Math.max(RUN_MIN_MS, t - last));
      } else {
        span = 0;
      }
      to = value;
      last = t;
    },
  };
}

/** `value` shown as a count running up to it (`runUp`), redrawn every 100 ms while it runs. */
export function useRunUp(value: number | undefined, now: () => number = Date.now): number | undefined {
  const [run] = useState(runUp);
  // Idempotent for the same value: a render repeated with it changes nothing.
  run.update(value, now());
  const t = useNow(100, value !== undefined && run.at(now()) !== value, now);
  return run.at(t);
}

const PHONE = '(max-width: 899px)';

/** Phone layout: one screen at a time, the permission request as a bottom sheet. Without
 *  `matchMedia` (tests) it is the desktop. */
export function useIsPhone(): boolean {
  const mq = typeof matchMedia === 'function' ? matchMedia(PHONE) : null;
  const [phone, setPhone] = useState(mq?.matches ?? false);
  useEffect(() => {
    if (!mq) return;
    const on = () => setPhone(mq.matches);
    mq.addEventListener?.('change', on);
    return () => mq.removeEventListener?.('change', on);
  }, []);
  return phone;
}

const NARROW = '(min-width: 900px) and (max-width: 1079px)';

/** A window 900–1079 px wide: no session column in the layout, it slides out over the chat.
 *  Without `matchMedia` (tests) it is the desktop. */
export function useIsNarrow(): boolean {
  const mq = typeof matchMedia === 'function' ? matchMedia(NARROW) : null;
  const [narrow, setNarrow] = useState(mq?.matches ?? false);
  useEffect(() => {
    if (!mq) return;
    const on = () => setNarrow(mq.matches);
    mq.addEventListener?.('change', on);
    return () => mq.removeEventListener?.('change', on);
  }, []);
  return narrow;
}

/** A button's request: busy while it runs, its error after. */
export function useAction() {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Set at once, not through state: two clicks before a re-render send one request.
  const running = useRef(false);
  const run = (fn: () => Promise<void>) => {
    if (running.current) return;
    running.current = true;
    setBusy(true);
    setError(null);
    fn()
      .catch((e: Error) => setError(e.message))
      .finally(() => {
        running.current = false;
        setBusy(false);
      });
  };
  return { busy, error, run };
}
