// @vitest-environment happy-dom
// T33: history search (SPEC §15) — the search page in the place of the session column and the
// chat (the project bar stays): the field with the counts, the kinds with theirs, the project,
// the time, "Also typos", best match or newest, "Also finds", one card per session with "N more
// in this session"; back to where it was opened from; a hit that opens its session at the event.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import type { Api, SearchOptions } from '../src/api';
import { AppState } from '../src/app';
import { parseEvent, type HqcEvent, type SearchCounts, type SearchHit, type SearchPage, type SessionInfo } from '../src/proto';
import type { HubSocket } from '../src/ws';
import { App, parseRoute } from '../src/ui/App';
import { Chat } from '../src/ui/Chat';
import { CARD_HITS, SEARCH_DELAY_MS, SearchPage as Page, hitHref, readSearch, searchHash } from '../src/ui/Search';

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
  // Two days ago: a card says "2 d ago".
  updated_at: new Date(Date.now() - 2 * 86_400_000 - 60_000).toISOString(),
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
  session_hits: 1,
  ...more,
});

const COUNTS: SearchCounts = {
  all: { hits: 9, sessions: 2 },
  message: { hits: 5, sessions: 2 },
  command: { hits: 2, sessions: 1 },
  output: { hits: 1, sessions: 1 },
  answer: { hits: 0, sessions: 0 },
};

let root: HTMLElement;
beforeEach(() => {
  root = document.createElement('div');
  document.body.appendChild(root);
  history.replaceState(null, '', '#/search');
});
afterEach(() => {
  render(null, root);
  document.body.innerHTML = '';
  location.hash = '';
  vi.useRealTimers();
});

function makeApp(search: (q: string, opts: SearchOptions) => Promise<SearchPage>, events?: HqcEvent[]) {
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
    sessionOrder: async () => ({ mode: 'state', session_ids: [] }),
  };
  const socket = { onState: () => () => {}, subscribe: () => () => {} } as unknown as HubSocket;
  const app = new AppState(api as unknown as Api, socket);
  app.me = { id: 'u', email: 'who@example.com', personal_project_id: 'p1', theme: 'warm', look: 'spacious', project_sort: 'custom' };
  app.projects.set('p1', { id: 'p1', name: 'hqc', slug: 'hqc', description: '', is_personal: false, color: 'teal', created_at: '2026-10-01T00:00:00Z' });
  app.projects.set('p2', { id: 'p2', name: 'home-network', slug: 'home', description: '', is_personal: false, color: 'blue', created_at: '2026-10-02T00:00:00Z' });
  app.currentProjectId = 'p1';
  app.conn = 'open';
  app.sessions.set('s1', info('s1', 'Long one'));
  app.sessions.set('s2', info('s2', 'Deploy notes'));
  return { app, api };
}

const mountPage = (app: AppState, back = '#/s/s2', phone = false) => act(async () => render(<Page app={app} back={back} phone={phone} />, root));

async function type(value: string) {
  const input = root.querySelector<HTMLInputElement>('input[aria-label="Search sessions and messages"]')!;
  await act(async () => {
    input.value = value;
    input.dispatchEvent(new Event('input', { bubbles: true }));
  });
}

/** Past the typing pause and the request. */
const settle = () => act(async () => void (await new Promise((r) => setTimeout(r, SEARCH_DELAY_MS + 50))));
const click = (el: Element | null) => act(async () => void (el as HTMLElement).click());
const select = (label: string, value: string) =>
  act(async () => {
    const s = root.querySelector<HTMLSelectElement>(`select[aria-label="${label}"]`)!;
    s.value = value;
    s.dispatchEvent(new Event('change', { bubbles: true }));
  });
const chip = (label: string) => [...root.querySelectorAll('.sp-chip')].find((c) => c.textContent!.startsWith(label))!;
const newest = () => [...root.querySelectorAll('.sp-seg button')].find((b) => b.textContent === 'Newest')!;
const text = (sel: string) => [...root.querySelectorAll(sel)].map((e) => e.textContent);
/** Each card: its title, its meta, its hits as `[href, kind, snippet, mono]`, its "more". */
const cards = () =>
  [...root.querySelectorAll('.sp-card')].map((c) => ({
    title: c.querySelector('.sp-title')!.textContent,
    meta: c.querySelector('.sp-meta')!.textContent,
    hits: [...c.querySelectorAll('.sp-hit')].map((h) => [
      h.getAttribute('href'),
      h.querySelector('.sp-kind')!.textContent,
      h.querySelector('.sp-x')!.textContent,
      h.querySelector('.sp-x')!.classList.contains('mono'),
    ]),
    more: c.querySelector('.sp-more')?.textContent,
  }));

describe('routes', () => {
  it('opens a session at an event, and the search page with its query', () => {
    expect(parseRoute('#/s/s1?seq=42')).toEqual({ name: 'session', id: 's1', seq: 42 });
    expect(parseRoute('#/s/s1')).toEqual({ name: 'session', id: 's1' });
    expect(parseRoute('#/s/s1?seq=nope')).toEqual({ name: 'session', id: 's1' });
    expect(parseRoute('#/search')).toEqual({ name: 'search' });
    expect(parseRoute('#/search?q=deploy&kind=command')).toEqual({ name: 'search' });
    expect(hitHref(hit('s1', 7, ''))).toBe('#/s/s1?seq=7');
    expect(hitHref(hit('s1', undefined, ''))).toBe('#/s/s1');
    const state = { q: 'переезд хаба', kind: 'output' as const, project: 'p2', time: 'week' as const, newest: true, fuzzy: true };
    expect(readSearch(searchHash(state))).toEqual(state);
    expect(searchHash(readSearch('#/search'))).toBe('#/search');
    // What is not a filter of the page is dropped.
    expect(readSearch('#/search?q=x&kind=nope&time=year')).toEqual({ q: 'x', kind: undefined, project: undefined, time: '', newest: false, fuzzy: false });
  });
});

describe('the search page', () => {
  /** s1: 6 hits, the first four on the page; s2: 2 hits; s1's other two come with "more". */
  const first = (): SearchPage => ({
    results: [
      hit('s1', 120, '…the deploy is done, deployed at 10…', { session_hits: 6 }),
      hit('s2', undefined, 'Deploy notes', { title: 'Deploy notes', address: 'home/s2', session_hits: 2 }),
      hit('s1', 7, 'deploy --tunnel', { type: 'tool.call', session_hits: 6 }),
      hit('s1', 8, 'tunnel deploy ok', { type: 'tool.result', session_hits: 6 }),
      hit('s1', 9, 'Deploy; then test', { type: 'clarify.response', session_hits: 6 }),
      hit('s2', 3, 'deploy again', { title: 'Deploy notes', address: 'home/s2', session_hits: 2 }),
    ],
    next_cursor: '50',
    also: ['deployed'],
    counts: COUNTS,
  });

  it('shows the counts, the kinds with theirs, "Also finds" and a card per session', async () => {
    const { app, api } = makeApp(async () => first());
    await mountPage(app);
    // Back to where it was opened from, named by the project.
    const back = root.querySelector('.sp-back')!;
    expect(back.getAttribute('href')).toBe('#/s/s2');
    expect(back.textContent).toBe('hqc');
    expect(document.activeElement).toBe(root.querySelector('input[aria-label="Search sessions and messages"]'));
    expect(root.querySelector('.sp-empty')!.textContent).toBe('Messages, commands, tool output and answers of every session');

    await type('d');
    await settle();
    expect(api.search).not.toHaveBeenCalled();
    await type('deploy');
    await settle();
    expect(api.search).toHaveBeenCalledTimes(1);
    expect(api.search).toHaveBeenLastCalledWith('deploy', {
      fuzzy: false,
      kind: undefined,
      project: undefined,
      since: undefined,
      newest: false,
      perSession: CARD_HITS,
      limit: 50,
    });
    expect(root.querySelector('.sp-total')!.textContent).toBe('9 results in 2 sessions');
    expect(text('.sp-chip')).toEqual(['All9', 'Messages5', 'Commands2', 'Tool output1', 'Answers0']);
    expect(root.querySelector('.sp-chip.is-on')!.textContent).toBe('All9');
    expect(root.querySelector('.sp-also')!.textContent).toBe('Also finds: deployed');
    // In the order of each session's first hit; the kind of each hit, commands and output in
    // mono; a title hit is "Title".
    expect(cards()).toEqual([
      {
        title: 'Session s1',
        meta: 'hqc · 6 results · 2 d ago',
        hits: [
          ['#/s/s1?seq=120', 'Message', '…the deploy is done, deployed at 10…', false],
          ['#/s/s1?seq=7', 'Command', 'deploy --tunnel', true],
          ['#/s/s1?seq=8', 'Tool output', 'tunnel deploy ok', true],
          ['#/s/s1?seq=9', 'Answer', 'Deploy; then test', false],
        ],
        more: '2 more in this session ›',
      },
      {
        title: 'Deploy notes',
        meta: 'home-network · 2 results · 2 d ago',
        hits: [
          ['#/s/s2', 'Title', 'Deploy notes', false],
          ['#/s/s2?seq=3', 'Message', 'deploy again', false],
        ],
        more: undefined,
      },
    ]);
    // The typed word and the whole forms the hub found are marked.
    expect(text('.sp-card:first-of-type .sp-hit:first-of-type mark')).toEqual(['deploy', 'deployed']);
    expect(root.querySelector('.sp-card .sp-title')!.getAttribute('href')).toBe('#/s/s1');
    // The address keeps the search, without a new history entry.
    expect(location.hash).toBe('#/search?q=deploy');
  });

  it('"N more in this session" brings the rest of that session; "More results" the next page', async () => {
    const { app, api } = makeApp(async (_q, o) => {
      if (o.session) {
        return o.cursor
          ? { results: [hit('s1', 2, 'deploy two', { session_hits: 6 })], also: [] }
          : {
              results: [5, 4, 3, 1, 120].map((n) => hit('s1', n, `deploy ${n}`, { session_hits: 6 })),
              next_cursor: '5',
              also: ['deploying', 'deployed'],
            };
      }
      return o.cursor ? { results: [hit('s3', 1, 'deploy far', { session_hits: 1 })], also: ['deploys'] } : first();
    });
    history.replaceState(null, '', '#/search?time=week');
    await mountPage(app);
    await type('deploy');
    await settle();
    const since = api.search.mock.lastCall![1].since;
    expect(since).toEqual(expect.any(String));
    // Later: the pages and "N more" keep the query's time window.
    await act(async () => void (await new Promise((r) => setTimeout(r, 20))));
    await click(root.querySelector('.sp-card .sp-more'));
    expect(api.search).toHaveBeenLastCalledWith('deploy', expect.objectContaining({ session: 's1', limit: 50, cursor: undefined, since }));
    expect(api.search.mock.lastCall![1].perSession).toBeUndefined();
    expect(cards()[0].hits.map((h) => h[0])).toEqual(['#/s/s1?seq=5', '#/s/s1?seq=4', '#/s/s1?seq=3', '#/s/s1?seq=1', '#/s/s1?seq=120']);
    expect(cards()[0].more).toBe('1 more in this session ›');
    // The forms its hits have join "Also finds".
    expect(root.querySelector('.sp-also')!.textContent).toBe('Also finds: deployed, deploying');
    await click(root.querySelector('.sp-card .sp-more'));
    expect(api.search).toHaveBeenLastCalledWith('deploy', expect.objectContaining({ session: 's1', cursor: '5', since }));
    expect(cards()[0].hits).toHaveLength(6);
    expect(cards()[0].more).toBeUndefined();

    await click(root.querySelector('.sp-next'));
    expect(api.search).toHaveBeenLastCalledWith('deploy', expect.objectContaining({ cursor: '50', perSession: CARD_HITS, since }));
    expect(cards().map((c) => c.title)).toEqual(['Session s1', 'Deploy notes', 'Session s3']);
    expect(root.querySelector('.sp-also')!.textContent).toBe('Also finds: deployed, deploying, deploys');
    expect(root.querySelector('.sp-next')).toBeNull();
    // The counts are the first page's.
    expect(root.querySelector('.sp-total')!.textContent).toBe('9 results in 2 sessions');
  });

  it("a kind or the order keeps the counts while it asks, then takes its answer's; the text, project or time drop them at once", async () => {
    const later: ((p: SearchPage) => void)[] = [];
    let asked = 0;
    const { app } = makeApp(() => (asked++ ? new Promise<SearchPage>((r) => later.push(r)) : Promise.resolve(first())));
    const answer = (n: number, all: number) =>
      act(async () => {
        later[n]({ results: [], also: [], counts: { ...COUNTS, all: { hits: all, sessions: 3 } } });
        await new Promise((r) => setTimeout(r, 0));
      });
    await mountPage(app);
    await type('deploy');
    await settle();
    const counted = ['All9', 'Messages5', 'Commands2', 'Tool output1', 'Answers0'];
    expect(text('.sp-chip')).toEqual(counted);
    // A kind: the counts stay while it asks, then its answer's (fresher) are taken.
    await click(chip('Commands'));
    await settle();
    expect(later).toHaveLength(1);
    expect(text('.sp-chip')).toEqual(counted);
    expect(root.querySelector('.sp-total')!.textContent).toBe('2 results in 1 session');
    await answer(0, 99);
    const counted99 = ['All99', ...counted.slice(1)];
    expect(text('.sp-chip')).toEqual(counted99);
    // The order, too.
    await click(newest());
    await settle();
    expect(text('.sp-chip')).toEqual(counted99);
    await answer(1, 98);
    expect(text('.sp-chip')).toEqual(['All98', ...counted.slice(1)]);
    // A project: other counts, none while it asks.
    await select('Project', 'p2');
    await settle();
    expect(root.querySelector('.sp-n')).toBeNull();
    expect(root.querySelector('.sp-total')).toBeNull();
    await answer(2, 7);
    expect(text('.sp-chip')[0]).toBe('All7');
    // A kind asked before the project's answer came keeps none: the next answer brings them.
    await select('Time', 'week');
    await settle();
    await click(chip('Messages'));
    await settle();
    await answer(4, 5);
    expect(text('.sp-chip')[0]).toBe('All5');
  });

  it('"More" of an earlier search answering late adds nothing to the new one', async () => {
    const late: ((p: SearchPage) => void)[] = [];
    const { app } = makeApp(async (q, o) => {
      if (o.cursor || o.session) return new Promise<SearchPage>((r) => late.push(r));
      return q === 'deploy' ? first() : { results: [hit('s3', 1, 'tunnel up', { session_hits: 1 })], also: [], counts: COUNTS };
    });
    await mountPage(app);
    await type('deploy');
    await settle();
    await click(root.querySelector('.sp-next'));
    await click(root.querySelector('.sp-card .sp-more'));
    await type('tunnel');
    await settle();
    await act(async () => {
      for (const r of late) r({ results: [hit('s1', 2, 'deploy two', { session_hits: 6 })], also: ['deploys'] });
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(late).toHaveLength(2);
    expect(cards().map((c) => [c.title, c.hits.length])).toEqual([['Session s3', 1]]);
    expect(root.querySelector('.sp-also')).toBeNull();
  });

  it('asks again for a kind, a project, a time, the newest first and typos; the address keeps them', async () => {
    const { app, api } = makeApp(async () => first());
    await mountPage(app);
    await type('de');
    await settle();
    expect(api.search).toHaveBeenLastCalledWith('de', expect.objectContaining({ fuzzy: false }));
    // "Also typos" waits for three characters, as the hub does.
    await act(async () => {
      const typos = root.querySelector<HTMLInputElement>('.sp-typos input')!;
      typos.checked = true;
      typos.dispatchEvent(new Event('change', { bubbles: true }));
    });
    await settle();
    expect(api.search).toHaveBeenCalledTimes(1);
    // Typing waits for the pause; a filter asks at once.
    await type('deploy');
    expect(api.search).toHaveBeenCalledTimes(1);
    await settle();
    expect(api.search).toHaveBeenCalledTimes(2);
    expect(api.search).toHaveBeenLastCalledWith('deploy', expect.objectContaining({ fuzzy: true }));

    await click(chip('Commands'));
    expect(api.search).toHaveBeenCalledTimes(3);
    expect(api.search).toHaveBeenLastCalledWith('deploy', expect.objectContaining({ kind: 'command' }));
    expect(root.querySelector('.sp-chip.is-on')!.textContent).toBe('Commands2');
    // The count in the field is the kind's.
    expect(root.querySelector('.sp-total')!.textContent).toBe('2 results in 1 session');

    expect(text('select[aria-label="Project"] option')).toEqual(['All projects', 'hqc', 'home-network']);
    await select('Project', 'p2');
    expect(api.search).toHaveBeenCalledTimes(4);
    expect(api.search).toHaveBeenLastCalledWith('deploy', expect.objectContaining({ project: 'p2' }));
    await select('Time', 'week');
    expect(api.search).toHaveBeenCalledTimes(5);
    const since = Date.parse(api.search.mock.lastCall![1].since!);
    expect(Math.abs(Date.now() - 7 * 86_400_000 - since)).toBeLessThan(5_000);
    await click(newest());
    expect(api.search).toHaveBeenCalledTimes(6);
    await act(async () => {
      const typos = root.querySelector<HTMLInputElement>('.sp-typos input')!;
      typos.checked = false;
      typos.dispatchEvent(new Event('change', { bubbles: true }));
    });
    expect(api.search).toHaveBeenCalledTimes(7);
    expect(api.search).toHaveBeenLastCalledWith('deploy', expect.objectContaining({ fuzzy: false }));
    await act(async () => {
      const typos = root.querySelector<HTMLInputElement>('.sp-typos input')!;
      typos.checked = true;
      typos.dispatchEvent(new Event('change', { bubbles: true }));
    });
    await settle();
    expect(api.search).toHaveBeenCalledTimes(8);
    expect(api.search).toHaveBeenLastCalledWith('deploy', {
      fuzzy: true,
      kind: 'command',
      project: 'p2',
      since: expect.any(String),
      newest: true,
      perSession: CARD_HITS,
      limit: 50,
    });
    expect(location.hash).toBe('#/search?q=deploy&kind=command&project=p2&time=week&order=newest&fuzzy=1');

    // Opened again at that address (the browser's Back from a hit): the same search.
    render(null, root);
    api.search.mockClear();
    await mountPage(app);
    expect(root.querySelector<HTMLInputElement>('input[aria-label="Search sessions and messages"]')!.value).toBe('deploy');
    // Not typed: at once, and only once.
    expect(api.search).toHaveBeenCalledTimes(1);
    await settle();
    expect(api.search).toHaveBeenCalledTimes(1);
    expect(api.search).toHaveBeenLastCalledWith('deploy', expect.objectContaining({ kind: 'command', project: 'p2', newest: true, fuzzy: true }));
  });

  it('says what the hub refused, and "Nothing found"', async () => {
    let fail = true;
    const { app } = makeApp(async () => {
      if (fail) throw new Error('a fuzzy query needs at least 3 characters');
      return { results: [], also: [], counts: { ...COUNTS, all: { hits: 0, sessions: 0 } } };
    });
    await mountPage(app);
    await type('xy');
    await settle();
    expect(root.querySelector('.search-page .error-text')!.textContent).toBe('a fuzzy query needs at least 3 characters');
    fail = false;
    await type('xyz');
    // Not before the hub answered.
    expect(root.querySelector('.sp-empty')).toBeNull();
    await settle();
    expect(root.querySelector('.sp-empty')!.textContent).toBe('Nothing found');
    expect(root.querySelector('.sp-total')!.textContent).toBe('0 results in 0 sessions');
  });

  it('Esc goes back; not while the key is in the project bar', async () => {
    const { app } = makeApp(async () => first());
    const rail = document.createElement('input');
    document.body.appendChild(rail);
    await mountPage(app, '#/p/p1');
    await act(async () => {
      rail.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    });
    expect(location.hash).toBe('#/search');
    await act(async () => {
      root.querySelector('input')!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    });
    expect(location.hash).toBe('#/p/p1');
  });

  it('in the Compact look: the body across the page, the filters in one row, the project apart', async () => {
    const { app } = makeApp(async () => first());
    app.me = { ...app.me!, look: 'compact' };
    await mountPage(app);
    await type('deploy');
    await settle();
    expect(root.querySelector('.search-page')!.classList.contains('is-dense')).toBe(true);
    expect(root.querySelectorAll('.sp-fl')).toHaveLength(1);
    expect(root.querySelector('.sp-card .sp-proj')!.textContent).toBe('hqc');
    expect(cards()[0].meta).toBe('6 results · 2 d ago');
    // Spacious: two rows, the project in the meta.
    app.me = { ...app.me!, look: 'spacious' };
    await act(async () => (app as unknown as { changed(): void }).changed());
    expect(root.querySelectorAll('.sp-fl')).toHaveLength(2);
    expect(root.querySelector('.sp-proj')).toBeNull();
  });

  it('on a phone too: Compact is dense, the project in the line under the title', async () => {
    const { app } = makeApp(async () => first());
    app.me = { ...app.me!, look: 'compact' };
    await mountPage(app, '#/s/s2', true);
    await type('deploy');
    await settle();
    expect(root.querySelector('.search-page')!.classList.contains('is-dense')).toBe(true);
    expect(root.querySelectorAll('.sp-fl')).toHaveLength(1);
    expect(root.querySelector('.sp-proj')).toBeNull();
    expect(cards()[0].meta).toBe('hqc · 6 results · 2 d ago');
  });
});

describe('the page in the app', () => {
  // A desktop window: happy-dom's own 1024 px is the narrow layout (900–1079 px), without the
  // session column.
  const windowWidth = (width: number) =>
    (window as unknown as { happyDOM: { setViewport(v: { width: number }): void } }).happyDOM.setViewport({ width });
  beforeEach(() => windowWidth(1280));
  afterEach(() => windowWidth(1024));

  it('takes the place of the session column and the chat; back goes where it was opened from', async () => {
    const { app } = makeApp(async () => ({ results: [], also: [] }));
    location.hash = '#/s/s2';
    await act(async () => render(<App app={app} />, root));
    expect(root.querySelector('.sidebar')).not.toBeNull();
    // The magnifier of the column's head.
    const open = root.querySelector<HTMLAnchorElement>('.side-head .btn-search')!;
    expect(open.getAttribute('href')).toBe('#/search');
    await act(async () => {
      location.hash = '#/search';
      dispatchEvent(new HashChangeEvent('hashchange'));
    });
    expect(root.querySelector('.layout')!.classList.contains('route-search')).toBe(true);
    expect(root.querySelector('.sidebar, .col-grip, .chat')).toBeNull();
    expect(root.querySelector('.rail')).not.toBeNull();
    expect(root.querySelector('.main .search-page')).not.toBeNull();
    expect(root.querySelector('.sp-back')!.getAttribute('href')).toBe('#/s/s2');
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
    const { app } = makeApp(async () => ({ results: [], also: [] }), history);
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
    const { app } = makeApp(async () => ({ results: [], also: [] }), [
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
