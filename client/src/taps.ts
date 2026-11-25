// iOS WebKit takes a tap for a hover, with no click, when a link or a button comes into view while
// it watches the tap (its ContentChangeObserver, over the whole page). It starts watching on a
// touchstart it dispatches to the page, and it dispatches touches only where the page listens to
// touch or pointer events. A scroll that began on a session row (the row listens to pointer
// events for the long press) left the watch open; the projects rendered after it were links that came into view, and the next tap on a
// project, where nothing listened, started no new watch: it only hovered the row, and a second
// tap opened the project.

/** Makes every touch reach the page, so that iOS starts watching each tap anew: a `touchstart`
 *  listener on the document that does nothing, passive, so it never holds up a scroll. */
export function listenForTaps(doc: Document = document): void {
  doc.addEventListener('touchstart', () => {}, { passive: true });
}
