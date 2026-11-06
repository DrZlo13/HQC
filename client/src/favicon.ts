// The tab's icon marks what the session list marks on the projects: an amber dot when a
// session waits for the user (a request or an error), a light one when nothing waits but
// something is unread (archived and service sessions aside).

import type { AppState } from './app';
import { STATUS } from './theme';

const MARK = '/hqc-mark-small.svg';

export type TabMark = 'waiting' | 'unread' | undefined;

export function tabMark(app: AppState): TabMark {
  let unread = false;
  for (const s of app.sessions.values()) {
    if (s.archived || s.system) continue;
    if (s.state === 'waiting' || s.state === 'error') return 'waiting';
    if (s.unread > 0) unread = true;
  }
  return unread ? 'unread' : undefined;
}

/** The mark with a small dot in its lower right corner and a clear gap around it, as GitHub
 *  marks its tab, as a `data:` URL (the CSP allows no `blob:`). */
export function markedIcon(svg: string, mark: Exclude<TabMark, undefined>): string {
  const fill = mark === 'waiting' ? STATUS.waiting : '#F4F1EC';
  const open = svg.indexOf('>', svg.indexOf('<svg')) + 1;
  const cut =
    '<defs><mask id="hqc-tab-cut"><rect width="200" height="200" fill="#fff"/>' +
    `<circle cx="${DOT.x}" cy="${DOT.y}" r="${DOT.r + DOT.gap}" fill="#000"/></mask></defs><g mask="url(#hqc-tab-cut)">`;
  const dot = `</g><circle cx="${DOT.x}" cy="${DOT.y}" r="${DOT.r}" fill="${fill}"/>`;
  const marked = svg.slice(0, open) + cut + svg.slice(open).replace(/<\/svg>\s*$/, `${dot}</svg>`);
  return `data:image/svg+xml,${encodeURIComponent(marked)}`;
}

/** The dot in the mark's 200 × 200 view box. */
const DOT = { x: 162, y: 162, r: 30, gap: 14 };

/** Keeps the tab's icons in step with `tabMark`; the plain ones while nothing is marked. */
export function watchTabIcon(app: AppState, doc: Document = document): () => void {
  const links = [...doc.querySelectorAll<HTMLLinkElement>('link[rel="icon"]')];
  const plain = links.map((l) => [l.getAttribute('href'), l.getAttribute('type'), l.getAttribute('sizes')]);
  let svg: Promise<string> | undefined;
  let shown: TabMark;
  const update = () => {
    const mark = tabMark(app);
    if (mark === shown) return;
    shown = mark;
    if (!mark) {
      links.forEach((l, i) => {
        const [href, type, sizes] = plain[i];
        l.setAttribute('href', href ?? '');
        set(l, 'type', type);
        set(l, 'sizes', sizes);
      });
      return;
    }
    svg ??= fetch(MARK).then((r) => (r.ok ? r.text() : Promise.reject(new Error(`${MARK}: ${r.status}`))));
    svg.then(
      (text) => {
        if (shown !== mark) return;
        const href = markedIcon(text, mark);
        for (const l of links) {
          l.setAttribute('href', href);
          l.setAttribute('type', 'image/svg+xml');
          l.removeAttribute('sizes');
        }
      },
      (e: Error) => {
        // The next change tries again.
        svg = undefined;
        shown = undefined;
        console.warn('tab icon', e);
      },
    );
  };
  update();
  return app.onChange(update);
}

function set(l: Element, name: string, value: string | null): void {
  if (value === null) l.removeAttribute(name);
  else l.setAttribute(name, value);
}
