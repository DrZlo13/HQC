// @vitest-environment happy-dom
// T33: history search (SPEC §15) — the hub's hits under the session list (desktop) and on
// the phone's search screen, paging, fuzzy, and a hit that opens its session at the event.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import type { Api } from '../src/api';
import { AppState } from '../src/app';
import { parseEvent, type HqcEvent, type SearchHit, type SearchPage, type SessionInfo } from '../src/proto';
import type { HubSocket } from '../src/ws';
import { parseRoute } from '../src/ui/App';
import { Chat } from '../src/ui/Chat';
import { SEARCH_DELAY_MS, SearchScreen, hitHref } from '../src/ui/Search';
import { SessionList } from '../src/ui/SessionList';

const info = (id: string, title: string, more: Partial<SessionInfo> = {}): SessionInfo => ({
  id,
  project_id: 'p1',
  workspace_id: 'w1',
  worker_id: 'wk1',
  cwd: '.',
  title,
  slug: id,
  state: 'idle',
  queue_len: 0,
  unread: 0,
  last_seq: 1,
  last_read_seq: 1,
  archived: false,
  updated_at: '2026-10-24T19:47:00Z',
  system: false,
  ...more,
});

const hit = (session: string, seq: number | undefined, snippet: string, more: Partial<SearchHit> = {}): SearchHit => ({
  session_id: session,
  address: `hqc/${session}`,
  title: `Session ${session}`,
  archived: false,
  seq,
  type: seq === undefined ? undefined : 'agent.message',
  ts: '2026-10-24T18:00:00Z',
  snippet,
  ...more,
});

let root: HTMLElement;
beforeEach(() => {
  root = document.createElement('div');
  document.body.appendChild(root);
});
afterEach(() => {
  render(null, root);
  document.body.innerHTML = '';
  vi.useRealTimers();
});

function makeApp(search: (q: string, opts: { fuzzy?: boolean; cursor?: string }) => Promise<SearchPage>, events?: HqcEvent[]) {
  const api = {
    search: vi.fn(search),
    workspaces: async () => [],
    events: vi.fn(async (_id: string, after: number, limit = 200) => {
      const page = (events ?? []).filter((e) => e.seq! > after).slice(0, limit);
      return { events: page, invalid: [], maxSeq: Math.max(after, ...page.map((e) => e.seq!)), count: page.length };
    }),
    eventsBefore: vi.fn(async (_id: string, before = Number.MAX_SAFE_INTEGER, limit = 200) => {
      const found = (events ?? []).filter((e) => e.seq! < before).slice(-limit);
      const seqs = found.map((e) => e.seq!);
      return { events: found, invalid: [], maxSeq: Math.max(0, ...seqs), minSeq: seqs[0], count: found.length };
    }),
    readCursor: async () => {},
    session: async () => info('s1', 'Long one'),
  };
  const socket = { onState: () => () => {}, subscribe: () => () => {} } as unknown as HubSocket;
  const app = new AppState(api as unknown as Api, socket);
  app.me = { id: 'u', email: 'who@example.com', personal_project_id: 'p1', theme: 'warm' };
  app.projects.set('p1', { id: 'p1', name: 'hqc', slug: 'hqc', description: '', is_personal: false, color: 'teal', created_at: '2026-10-01T00:00:00Z' });
  app.currentProjectId = 'p1';
  app.conn = 'open';
  app.sessions.set('s1', info('s1', 'Long one'));
  app.sessions.set('s2', info('s2', 'Deploy notes'));
  return { app, api };
}

async function type(label: string, value: string) {
  // The desktop column's field opens from the header's magnifier.
  const open = root.querySelector<HTMLButtonElement>('.side-head .btn-search');
  if (!root.querySelector(`input[aria-label="${label}"]`) && open) await act(async () => open.click());
  const input = root.querySelector<HTMLInputElement>(`input[aria-label="${label}"]`)!;
  await act(async () => {
    input.value = value;
    input.dispatchEvent(new Event('input', { bubbles: true }));
  });
}

/** Past the typing pause and the request. */
const settle = () => act(async () => void (await new Promise((r) => setTimeout(r, SEARCH_DELAY_MS + 50))));

const hits = () =>
  [...root.querySelectorAll<HTMLAnchorElement>('.search-hit')].map((a) => [
    a.getAttribute('href'),
    a.querySelector('.search-snippet')!.textContent,
  ]);

describe('routes', () => {
  it('opens a session at an event, and the search screen', () => {
    expect(parseRoute('#/s/s1?seq=42')).toEqual({ name: 'session', id: 's1', seq: 42 });
    expect(parseRoute('#/s/s1')).toEqual({ name: 'session', id: 's1' });
    expect(parseRoute('#/s/s1?seq=nope')).toEqual({ name: 'session', id: 's1' });
    expect(parseRoute('#/search')).toEqual({ name: 'search' });
    expect(hitHref(hit('s1', 7, ''))).toBe('#/s/s1?seq=7');
    expect(hitHref(hit('s1', undefined, ''))).toBe('#/s/s1');
  });
});

describe('search in the left column', () => {
  it('shows the hub hits under the sessions, pages on, and asks again with "Similar words"', async () => {
    const { app, api } = makeApp(async (_q, { cursor }) =>
      cursor
        ? { results: [hit('s2', 3, 'deploy again')] }
        : {
            results: [hit('s1', 120, '…the deploy is done…'), hit('s2', undefined, 'Deploy notes', { archived: true })],
            next_cursor: 'c1',
          },
    );
    await act(async () => render(<SessionList app={app} />, root));
    await type('Search sessions', 'd');
    await settle();
    expect(api.search).not.toHaveBeenCalled();
    expect(root.querySelector('.search-hits')).toBeNull();

    await type('Search sessions', 'deploy');
    // The local filter by title still runs at once.
    expect([...root.querySelectorAll('.session-group .session-title')].map((t) => t.textContent)).toEqual(['Deploy notes']);
    await settle();
    expect(api.search).toHaveBeenCalledTimes(1);
    expect(api.search).toHaveBeenLastCalledWith('deploy', { fuzzy: false });
    expect(hits()).toEqual([
      ['#/s/s1?seq=120', '…the deploy is done…'],
      ['#/s/s2', 'Deploy notes'],
    ]);
    expect([...root.querySelectorAll('.search-snippet mark')].map((m) => m.textContent)).toEqual(['deploy', 'Deploy']);
    expect(root.querySelectorAll('.search-archived').length).toBe(1);

    const more = [...root.querySelectorAll<HTMLButtonElement>('.search-more')][0];
    await act(async () => more.click());
    await settle();
    expect(api.search).toHaveBeenLastCalledWith('deploy', { fuzzy: false, cursor: 'c1' });
    expect(hits().map((h) => h[0])).toEqual(['#/s/s1?seq=120', '#/s/s2', '#/s/s2?seq=3']);
    expect(root.querySelector('.search-more')).toBeNull();

    const fuzzy = root.querySelector<HTMLInputElement>('.search-fuzzy input')!;
    await act(async () => {
      fuzzy.checked = true;
      fuzzy.dispatchEvent(new Event('change', { bubbles: true }));
    });
    await settle();
    expect(api.search).toHaveBeenLastCalledWith('deploy', { fuzzy: true });
  });

  it('shows the hub error and "No messages found"', async () => {
    let fail = true;
    const { app } = makeApp(async () => {
      if (fail) throw new Error('a fuzzy query needs at least 3 characters');
      return { results: [] };
    });
    await act(async () => render(<SessionList app={app} />, root));
    await type('Search sessions', 'xy');
    await settle();
    expect(root.querySelector('.search-hits .error-text')!.textContent).toBe('a fuzzy query needs at least 3 characters');
    fail = false;
    await type('Search sessions', 'xyz');
    await settle();
    expect(root.querySelector('.search-hits .empty')!.textContent).toBe('No messages found');
  });
});

describe('the phone search screen', () => {
  it('lists matching sessions of every project, then the hits', async () => {
    const { app } = makeApp(async () => ({ results: [hit('s1', 5, 'deploy log')] }));
    app.sessions.set('s3', info('s3', 'Deploy elsewhere', { project_id: 'p2' }));
    await act(async () => render(<SearchScreen app={app} />, root));
    await type('Search sessions and messages', 'deploy');
    await settle();
    expect([...root.querySelectorAll('.session-group .session-title')].map((t) => t.textContent)).toEqual([
      'Deploy notes',
      'Deploy elsewhere',
    ]);
    expect(hits()).toEqual([['#/s/s1?seq=5', 'deploy log']]);
  });
});

describe('a hit opens the session at its event', () => {
  // 450 events: the newest page holds 100 of them, the hit is in the older history.
  const history: HqcEvent[] = [];
  for (let seq = 1; seq <= 450; seq++) {
    const user = seq % 3 === 1;
    history.push(
      parseEvent({
        session_id: 's1',
        event_id: `0199f3a2-7c10-7000-8000-${String(seq).padStart(12, '0')}`,
        seq,
        ts: '2026-10-24T18:00:00Z',
        author: user ? { kind: 'user', id: 'u' } : { kind: 'agent', id: 's1' },
        // Every third event is a hidden `status`.
        ...(seq % 3 === 0
          ? { type: 'status', payload: { state: 'running' } }
          : user
            ? { type: 'user.message', payload: { text: `question ${seq}`, artifact_ids: [] } }
            : { type: 'agent.message', payload: { text: `answer ${seq}`, final: true } }),
      }),
    );
  }

  // happy-dom lays nothing out: every element is 0 px high. The feed is taller than its window.
  beforeEach(() => {
    const h = (px: number) =>
      function (this: HTMLElement) {
        return this.classList.contains('timeline') ? px : 0;
      };
    vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockImplementation(h(5000));
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(h(600));
  });
  afterEach(() => vi.restoreAllMocks());

  async function open(focusSeq: number) {
    const { app } = makeApp(async () => ({ results: [] }), history);
    const scrolled: string[] = [];
    Element.prototype.scrollIntoView = function (this: Element) {
      scrolled.push((this as HTMLElement).dataset.seq ?? '');
    } as Element['scrollIntoView'];
    await act(async () => {
      render(<Chat app={app} sessionId="s1" focusSeq={focusSeq} />, root);
      await new Promise((r) => setTimeout(r, 50));
    });
    return scrolled;
  }

  it('scrolls to the event once its page is loaded and marks it', async () => {
    const scrolled = await open(320);
    // The feed renders from the hit's block on.
    expect(root.querySelector<HTMLElement>('.feed-block')!.dataset.seq).toBe('320');
    const found = root.querySelector<HTMLElement>('.feed-block.is-found')!;
    expect(found.dataset.seq).toBe('320');
    expect(found.textContent).toContain('answer 320');
    expect(scrolled).toEqual(['320']);
  });

  it('a hidden event: the block before it', async () => {
    await open(321);
    expect(root.querySelector<HTMLElement>('.feed-block.is-found')!.dataset.seq).toBe('320');
  });

  it('an answer drawn above an input that came while it was written (SPEC §6): its own block', async () => {
    const event = (seq: number, type: 'user.message' | 'agent.message', payload: object) =>
      parseEvent({
        session_id: 's1',
        event_id: `0199f3a2-7c10-7000-8000-${String(seq).padStart(12, '0')}`,
        seq,
        ts: '2026-10-24T18:00:00Z',
        author: type === 'user.message' ? { kind: 'user', id: 'u' } : { kind: 'agent', id: 's1' },
        type,
        payload,
      });
    const { app } = makeApp(async () => ({ results: [] }), [
      event(1, 'user.message', { text: 'question', artifact_ids: [] }),
      event(2, 'user.message', { text: 'meanwhile', artifact_ids: [], during_text: true }),
      event(3, 'agent.message', { text: 'answer', final: true }),
    ]);
    Element.prototype.scrollIntoView = () => {};
    await act(async () => render(<Chat app={app} sessionId="s1" focusSeq={3} />, root));
    // The first chat of a run takes longer to load.
    for (let i = 0; i < 50 && !root.querySelector('.feed-block.is-found'); i++) {
      await act(async () => void (await new Promise((r) => setTimeout(r, 20))));
    }
    expect(root.querySelector<HTMLElement>('.feed-block.is-found')!.dataset.seq).toBe('3');
  });
});
