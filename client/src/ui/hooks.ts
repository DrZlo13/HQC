import { useEffect, useReducer } from 'preact/hooks';

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
