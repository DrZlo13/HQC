import { useEffect, useReducer, useState } from 'preact/hooks';

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
  const run = (fn: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    fn()
      .catch((e: Error) => setError(e.message))
      .finally(() => setBusy(false));
  };
  return { busy, error, run };
}
