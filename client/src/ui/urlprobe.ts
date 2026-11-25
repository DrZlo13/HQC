// The link-first forms (SPEC 0.236): the URL is pasted or typed, the hub is asked what is
// there, and the form fills the fields the owner has not edited. Every field stays editable.

import { useEffect, useRef } from 'preact/hooks';

/** Typing settles for this long before the URL is checked; a paste or leaving the field
 *  checks it at once. */
export const PROBE_DELAY_MS = 500;

export function looksLikeHttpUrl(url: string): boolean {
  return /^https?:\/\/[^\s/?#]+/i.test(url.trim());
}

/** Calls `probe` with the URL in `url` once it settles, after a paste or a blur at once, and
 *  only when it differs from the last one checked (the form's initial URL counts as checked,
 *  so an edit form asks nothing until the URL is changed). `current()` is false once a newer
 *  check started or the form closed: its answer is then dropped. */
export function useUrlProbe(url: string, probe: (url: string, current: () => boolean) => void) {
  const last = useRef(url.trim());
  const pasted = useRef(false);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const seq = useRef(0);
  // The latest callback: it reads the form's latest state.
  const run = useRef(probe);
  run.current = probe;

  const fire = (u: string) => {
    clearTimeout(timer.current);
    const v = u.trim();
    if (v === last.current || !looksLikeHttpUrl(v)) return;
    last.current = v;
    const n = ++seq.current;
    run.current(v, () => n === seq.current);
  };

  useEffect(
    () => () => {
      seq.current++;
    },
    [],
  );

  useEffect(() => {
    const now = pasted.current;
    pasted.current = false;
    timer.current = setTimeout(() => fire(url), now ? 0 : PROBE_DELAY_MS);
    return () => clearTimeout(timer.current);
  }, [url]);

  return {
    onPaste: () => {
      pasted.current = true;
    },
    onBlur: (e: Event) => fire((e.currentTarget as HTMLInputElement).value),
  };
}
