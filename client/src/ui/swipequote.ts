// On a phone a message dragged to the left is quoted into the input, as in Telegram: it follows the
// finger, a quote mark comes out at its right, and a lift past the threshold quotes it.
//
// Touch events, not pointer events: Safari on iOS fires `pointercancel` as soon as a horizontal pan
// strays a little vertically (w3c/pointerevents#303). All listeners are passive, as the feed's own
// are: a listener that may cancel takes the scroll off the browser's thread. The vertical scroll is
// kept off a swipe by `touch-action: pan-y pinch-zoom` on the block (styles.css).

/** A block that can be swiped: the message of the agent, of the user, of another session. */
const BLOCK = '.feed-block.feed-swipe[data-seq]';
/** A finger that goes farther than this from where it went down has chosen scroll or swipe. */
const SLOP_PX = 10;
/** A touch held this long before it moves is a long press, which selects text: no swipe (as in
 *  taps.ts, where a lift after it presses nothing). */
const HOLD_MS = 500;
/** The pull that quotes on a lift; the block stands out as armed from here. */
const THRESHOLD_PX = 60;
/** Past the threshold the block follows the finger this much, so that a long pull feels held back. */
const RESISTANCE = 0.25;

/** Whether an element between `from` and `block` scrolls sideways (a code block, a table): a drag
 *  along it is that scroll, not a swipe. */
function scrollsSideways(from: Element, block: Element): boolean {
  for (let el: Element | null = from; el && el !== block; el = el.parentElement) {
    if (el.scrollWidth <= el.clientWidth) continue;
    const overflow = getComputedStyle(el).overflowX;
    if (overflow === 'auto' || overflow === 'scroll') return true;
  }
  return false;
}

/** Listens to swipes to the left on the blocks of `feed` and calls `quote` with the `data-seq` of
 *  the block a swipe past the threshold was lifted on. Returns what removes the listeners. */
export function listenForSwipeQuote(feed: HTMLElement, quote: (seq: number) => void): () => void {
  // The touch in progress: its block and the node it went down on (the rest of its events go to
  // that node, also after it was removed from the page, and then bubble nowhere), where and when
  // it went down, and what it has become: undecided (before the slop), a swipe, or something else
  // (a scroll, a long press: ignored to its end).
  let touch: {
    block: HTMLElement;
    target: Element;
    icon: HTMLElement | null;
    x: number;
    y: number;
    at: number;
    mode: 'undecided' | 'swipe' | 'ignored';
    d: number;
  } | null = null;

  const snapBack = () => {
    const t = touch;
    touch = null;
    if (!t) return;
    t.target.removeEventListener('touchmove', move);
    t.target.removeEventListener('touchend', end);
    t.target.removeEventListener('touchcancel', end);
    t.block.classList.remove('is-swiping', 'is-armed');
    t.block.style.removeProperty('transform');
    for (const v of ['--swipe-x', '--swipe', '--swipe-y']) t.icon?.style.removeProperty(v);
  };

  const start = (e: TouchEvent) => {
    // A touch whose end never came is left over from before.
    snapBack();
    const t = e.touches[0];
    if (e.touches.length !== 1 || !t || !(e.target instanceof Element)) return;
    const block = e.target.closest<HTMLElement>(BLOCK);
    if (!block || !feed.contains(block)) return;
    touch = { block, target: e.target, icon: null, x: t.clientX, y: t.clientY, at: performance.now(), mode: 'undecided', d: 0 };
    e.target.addEventListener('touchmove', move, { passive: true });
    e.target.addEventListener('touchend', end, { passive: true });
    e.target.addEventListener('touchcancel', end, { passive: true });
  };

  // An `Event` for the listeners of an element, which has no touch events of its own in the types.
  function move(ev: Event) {
    const e = ev as TouchEvent;
    if (!touch || touch.mode === 'ignored') return;
    // A second finger is a pinch: the block goes back and the rest of the touch is nothing.
    if (e.touches.length !== 1) return snapBack();
    const t = e.touches[0];
    const dx = t.clientX - touch.x;
    const dy = t.clientY - touch.y;
    if (touch.mode === 'undecided') {
      if (Math.hypot(dx, dy) <= SLOP_PX) return;
      if (dx >= 0 || Math.abs(dx) <= Math.abs(dy) || performance.now() - touch.at > HOLD_MS || scrollsSideways(touch.target, touch.block)) {
        touch.mode = 'ignored';
        return;
      }
      touch.mode = 'swipe';
      touch.icon = touch.block.querySelector<HTMLElement>(':scope > .feed-swipe-icon');
    }
    const d = Math.max(0, -dx);
    const shown = d <= THRESHOLD_PX ? d : THRESHOLD_PX + (d - THRESHOLD_PX) * RESISTANCE;
    const { block, icon } = touch;
    touch.d = d;
    // The layout is read before anything is written. The variables are on the icon: one changed
    // on the block would restyle the whole message under it on every move.
    const y = t.clientY - block.getBoundingClientRect().top;
    block.classList.add('is-swiping');
    block.classList.toggle('is-armed', d >= THRESHOLD_PX);
    block.style.transform = `translateX(-${shown}px)`;
    icon?.style.setProperty('--swipe-x', `${shown}`);
    icon?.style.setProperty('--swipe', `${Math.min(1, d / THRESHOLD_PX)}`);
    icon?.style.setProperty('--swipe-y', `${y}`);
  }

  function end(ev: Event) {
    const e = ev as TouchEvent;
    if (!touch) return;
    // The end of this touch's own finger (another finger's end goes to its own node, and one that
    // went down in the feed has already put the block back). It quotes only when no finger stays:
    // one left on the screen (a thumb by the input) made it no lone swipe.
    const lifted = e.type === 'touchend' && e.touches.length === 0;
    const seq = lifted && touch.mode === 'swipe' && touch.d >= THRESHOLD_PX ? Number(touch.block.dataset.seq) : undefined;
    // The block goes back first: a quote that throws leaves it where it was.
    snapBack();
    // The quote goes here, in the handler of the touch: iOS shows the keyboard only for a focus
    // that a touch handler gives.
    if (seq !== undefined) quote(seq);
  }

  feed.addEventListener('touchstart', start, { passive: true });
  return () => {
    snapBack();
    feed.removeEventListener('touchstart', start);
  };
}
