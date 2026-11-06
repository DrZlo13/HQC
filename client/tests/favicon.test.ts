// @vitest-environment happy-dom
// The tab's icon: an amber dot while a session waits, a light one while something is unread.

import { afterEach, describe, expect, it, vi } from 'vitest';
import type { Api } from '../src/api';
import { AppState } from '../src/app';
import { tabMark, watchTabIcon } from '../src/favicon';
import type { SessionInfo, SessionState } from '../src/proto';
import { STATUS } from '../src/theme';
import type { HubSocket } from '../src/ws';

const SVG = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 200"><circle r="1"/></svg>';

function session(id: string, state: SessionState, unread = 0, over: Partial<SessionInfo> = {}): SessionInfo {
  return {
    id,
    project_id: 'p1',
    workspace_id: 'w1',
    worker_id: 'wk1',
    cwd: '.',
    title: id,
    slug: id,
    state,
    queue_len: 0,
    unread,
    last_seq: 1,
    last_read_seq: 1,
    archived: false,
    updated_at: '2026-11-06T20:00:00Z',
    system: false,
    ...over,
  };
}

function app(...sessions: SessionInfo[]): AppState {
  const socket = { onState: () => () => {} } as unknown as HubSocket;
  const a = new AppState({} as Api, socket);
  for (const s of sessions) a.sessions.set(s.id, s);
  return a;
}

afterEach(() => {
  document.head.innerHTML = '';
  vi.unstubAllGlobals();
});

describe('tab icon', () => {
  it('waiting before unread; archived and service sessions do not count', () => {
    expect(tabMark(app(session('a', 'idle')))).toBeUndefined();
    expect(tabMark(app(session('a', 'idle', 2)))).toBe('unread');
    expect(tabMark(app(session('a', 'idle', 2), session('b', 'error')))).toBe('waiting');
    expect(tabMark(app(session('a', 'waiting', 0, { archived: true }), session('b', 'idle', 3, { system: true })))).toBeUndefined();
  });

  it('puts the dot on the mark and gives the plain icons back', async () => {
    document.head.innerHTML =
      '<link rel="icon" href="/favicon.ico" sizes="16x16 32x32 48x48" /><link rel="icon" href="/hqc-mark-small.svg" type="image/svg+xml" />';
    const fetch = vi.fn(async () => new Response(SVG));
    vi.stubGlobal('fetch', fetch);
    const a = app(session('a', 'waiting'));
    watchTabIcon(a);
    await new Promise((r) => setTimeout(r, 0));
    const hrefs = () => [...document.querySelectorAll('link[rel="icon"]')].map((l) => l.getAttribute('href')!);
    for (const h of hrefs()) {
      const svg = decodeURIComponent(h.replace('data:image/svg+xml,', ''));
      expect(svg).toContain(`fill="${STATUS.waiting}"`);
      // The mark under a mask that cuts the gap around the dot.
      expect(svg).toMatch(/^<svg[^>]*><defs><mask id="hqc-tab-cut">.*<g mask="url\(#hqc-tab-cut\)"><circle r="1"\/><\/g><circle /);
      expect(svg.endsWith('</svg>')).toBe(true);
    }
    expect(document.querySelector('link[rel="icon"]')!.hasAttribute('sizes')).toBe(false);

    a.sessions.set('a', session('a', 'idle'));
    (a as unknown as { changed(): void }).changed();
    expect(hrefs()).toEqual(['/favicon.ico', '/hqc-mark-small.svg']);
    expect(document.querySelector('link[rel="icon"]')!.getAttribute('sizes')).toBe('16x16 32x32 48x48');
    expect(document.querySelector('link[rel="icon"]')!.hasAttribute('type')).toBe(false);
    expect(fetch).toHaveBeenCalledTimes(1);
  });
});
