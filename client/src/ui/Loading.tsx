/** "Loading…" with a spinner before it, in a note's muted colour; `center` stands the spinner over
 *  the word in the middle of its box (an empty feed, a viewer). */
export function Loading({ center }: { center?: boolean }) {
  return (
    <div class={center ? 'loading loading-center' : 'loading'} role="status">
      <span class="spinner" aria-hidden="true" />
      Loading…
    </div>
  );
}
