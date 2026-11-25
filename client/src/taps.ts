// iOS WebKit takes a tap for a hover, with no click, when a link or a button comes into view while
// it watches the tap (its ContentChangeObserver, over the whole page, offscreen parts included).
// It watches from the touchstart it dispatches to the page until the synthetic mouse events of the
// tap, and it dispatches touches only where the page listens to touch or pointer events.
//
// A scroll that began on a session row (the row listens to pointer events for the long press) left
// the watch open; the projects rendered after it were links that came into view, and the next tap
// on a project, where nothing listened, started no new watch: it only hovered the row, and a second
// tap opened the project. Every touch now reaches the page, so each tap is watched anew.
//
// While agents work the page renders new buttons all the time (a message's buttons on its
// delivery, the header's Stop on a change of state, a block in the feed), and any of them that
// appears during a tap takes its click: a press on Send was lost three times in a row. So a tap on
// a button or a link presses it itself when the finger is lifted, as a tap, and cancels the click
// iOS would or would not have made.

/** What a tap presses on its lift: a button, a link, a details summary, an element in the role of a
 *  button or a menu item. The innermost one under the finger. */
const PRESSABLE = 'button, a[href], summary, [role="button"], [role="menuitem"]';
/** A finger that goes farther than this from where it went down scrolls or drags: no press. */
const SLOP_PX = 10;
/** A touch held longer is a long press (a phone row opens its menu at 450 ms): no press, iOS and
 *  the page handle it as before. */
const HOLD_MS = 500;
/** Scroll events no farther apart than this after a swipe are its momentum (in the iOS simulator a
 *  feed's come every 16 ms, 50–80 ms apart as it slows down); a finger that comes down in them on
 *  what scrolls stops the scroll, which iOS does not take for a tap: no press. */
const COAST_GAP_MS = 100;

/** Listens to every touch on the page (passive where a listener could hold up a scroll) and
 *  presses the button or link a tap was on when its finger is lifted. */
export function listenForTaps(doc: Document = document): void {
  // The touch in progress: where it went down, when, and the pressable element it went down on
  // (none for a touch that stops a scroll).
  let down: { x: number; y: number; at: number; el: Element | null; moved: boolean } | null = null;
  // Whether scroll events have followed the last swipe without a pause, when the last came, and
  // what it scrolled (the document or an element).
  let coasting = false;
  let scrolledAt = -Infinity;
  let scrolled: EventTarget | null = null;
  const lifted = (moved: boolean) => {
    down = null;
    if (!moved) return;
    coasting = true;
    scrolledAt = performance.now();
  };
  doc.addEventListener(
    'scroll',
    (e) => {
      const now = performance.now();
      if (now - scrolledAt > COAST_GAP_MS) coasting = false;
      scrolledAt = now;
      scrolled = e.target;
    },
    { capture: true, passive: true },
  );
  doc.addEventListener(
    'touchstart',
    (e) => {
      const now = performance.now();
      // A finger on what coasts stops it; one elsewhere (Send under a coasting feed) is a tap.
      const on = e.target instanceof Node && (scrolled === doc || (scrolled instanceof Node && scrolled.contains(e.target)));
      const stops = coasting && now - scrolledAt <= COAST_GAP_MS && on;
      if (on) coasting = false;
      const t = e.touches[0];
      // A second finger is a pinch or a gesture: no press for either.
      if (e.touches.length !== 1 || !t) {
        down = null;
        return;
      }
      const el = !stops && e.target instanceof Element ? e.target.closest(PRESSABLE) : null;
      down = { x: t.clientX, y: t.clientY, at: now, el, moved: false };
    },
    { passive: true },
  );
  doc.addEventListener(
    'touchmove',
    (e) => {
      const t = e.touches[0];
      if (!down || down.moved || !t) return;
      if (Math.hypot(t.clientX - down.x, t.clientY - down.y) > SLOP_PX) down.moved = true;
    },
    { passive: true },
  );
  doc.addEventListener('touchcancel', () => lifted(down?.moved ?? false), { passive: true });
  // Not passive: its preventDefault is what cancels the click of iOS.
  doc.addEventListener('touchend', (e) => {
    const d = down;
    if (!d) return;
    lifted(d.moved);
    const el = d.el;
    const t = e.changedTouches[0];
    if (d.moved || !el || !t || e.touches.length > 0) return;
    // A handler of the page took the touch, or iOS already made it a gesture of its own.
    if (e.defaultPrevented || !e.cancelable) return;
    if (!el.isConnected || performance.now() - d.at > HOLD_MS) return;
    if (el.matches(':disabled') || el.getAttribute('aria-disabled') === 'true') return;
    // Lifted off the element (it moved from under the finger, or the finger slid off it).
    const r = el.getBoundingClientRect();
    if (t.clientX < r.left || t.clientX > r.right || t.clientY < r.top || t.clientY > r.bottom) return;
    e.preventDefault();
    (el as HTMLElement).click();
  });
}
