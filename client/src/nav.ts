// Back arrows step back through the tab's history instead of adding an entry: an arrow that
// added one left the screen it closed under the one it opened, and a phone's back gesture from
// a project's list brought back the session just closed with "‹".

/** Each entry of the tab's history keeps in its `history.state` the address it was opened
 *  from (the entry under it): `null` for the first one, which has none of the app's under it. */
type Stamp = { hqcFrom?: string | null };

let tracking = false;

/** "" and "#" are the projects screen, as `#/`. */
const same = (a: string, b: string) => (a === '' || a === '#' ? '#/' : a) === (b === '' || b === '#' ? '#/' : b);

function openedFrom(): string | null | undefined {
  return (history.state as Stamp | null)?.hqcFrom;
}

function stamp(from: string | null): void {
  history.replaceState({ ...((history.state as object | null) ?? {}), hqcFrom: from }, '');
}

/** Stamps the entries of this tab: the first one with `null`, every new address (a link,
 *  `location.hash = …`) with the one it was opened from. Before the app's own `hashchange`
 *  listener; an entry stepped back or forward to is stamped already. */
export function trackHistory(): void {
  if (tracking) return;
  tracking = true;
  if (openedFrom() === undefined) stamp(null);
  addEventListener('hashchange', (e) => {
    if (openedFrom() !== undefined) return;
    stamp(e.oldURL ? new URL(e.oldURL).hash || '#/' : null);
  });
}

/** Goes up to `hash`: back through the history when the entry under this one is that address,
 *  else in place of this entry. */
export function goUp(hash: string): void {
  const from = openedFrom();
  if (from != null && same(from, hash)) history.back();
  else replaceHash(hash);
}

/** Opens `hash` in place of the current entry; the new one keeps where that one was opened from. */
export function replaceHash(hash: string): void {
  const from = openedFrom() ?? null;
  // A fragment navigation updates the entry at once; its `hashchange` comes later, stamped.
  location.replace(hash);
  stamp(from);
}

/** A back arrow's click: up to its address, unless the click opens a new tab or window. */
export function onBack(e: MouseEvent, hash: string): void {
  if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
  e.preventDefault();
  goUp(hash);
}
