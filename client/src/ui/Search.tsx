// History search (SPEC §15): a page of its own in the place of the session column and the chat
// (the project bar stays; a phone's whole screen), opened by the magnifier of the column's
// head. The hub's hits, one card per session with its first few and "N more in this session";
// filters by kind with the counts of each, by project and by time; best match or newest first.
// A hit opens its session at the event; "‹ <project>" or Esc goes back to where the search was
// opened from.

import type { ComponentChildren } from 'preact';
import { useEffect, useRef, useState } from 'preact/hooks';
import type { SearchOptions } from '../api';
import type { AppState } from '../app';
import { SEARCH_KINDS, type Project, type SearchCounts, type SearchHit, type SearchKind } from '../proto';
import { ProjectGlyph, projectName, tileBackground } from './Appearance';
import { useSubscription } from './hooks';
import { IconBack, IconChevronDown, IconCommand, IconMessage, IconOutput, IconQuestion, IconSearch } from './icons';
import { count, formatAge, formatWhen } from './labels';

/** Shortest query sent to the hub, and shortest fuzzy one (the hub refuses shorter). */
export const MIN_QUERY = 2;
const MIN_FUZZY = 3;
/** Typing pause before a request. */
export const SEARCH_DELAY_MS = 300;
/** Hits a request asks for. */
const PAGE = 50;
/** Hits a card shows before "N more in this session": the hub sends no more of a session. */
export const CARD_HITS = 4;

/** `#/s/<id>?seq=<seq>`: the chat scrolls to the event; a title hit opens the session. */
export function hitHref(h: SearchHit): string {
  return `#/s/${encodeURIComponent(h.session_id)}${h.seq !== undefined ? `?seq=${h.seq}` : ''}`;
}

const TIMES = [
  { id: '', label: 'Any time', ms: 0 },
  { id: 'day', label: 'Past day', ms: 86_400_000 },
  { id: 'week', label: 'Past week', ms: 7 * 86_400_000 },
  { id: 'month', label: 'Past month', ms: 30 * 86_400_000 },
] as const;
type Time = (typeof TIMES)[number]['id'];

const KIND_TABS: { id: SearchKind | undefined; label: string }[] = [
  { id: undefined, label: 'All' },
  { id: 'message', label: 'Messages' },
  { id: 'command', label: 'Commands' },
  { id: 'output', label: 'Tool output' },
  { id: 'answer', label: 'Answers' },
];

/** What the page shows, kept in its address (`#/search?q=…&kind=…`): the browser's Back from
 *  an opened hit brings the same search back. */
export interface SearchState {
  q: string;
  kind?: SearchKind;
  /** A project id. */
  project?: string;
  time: Time;
  newest: boolean;
  fuzzy: boolean;
}

export function readSearch(hash: string): SearchState {
  const p = new URLSearchParams(hash.split('?')[1] ?? '');
  const kind = p.get('kind') as SearchKind | null;
  const time = p.get('time') as Time | null;
  return {
    q: p.get('q') ?? '',
    kind: kind && SEARCH_KINDS.includes(kind) ? kind : undefined,
    project: p.get('project') || undefined,
    time: TIMES.some((t) => t.id === time) ? time! : '',
    newest: p.get('order') === 'newest',
    fuzzy: p.get('fuzzy') === '1',
  };
}

export function searchHash(s: SearchState): string {
  const p = new URLSearchParams();
  if (s.q) p.set('q', s.q);
  if (s.kind) p.set('kind', s.kind);
  if (s.project) p.set('project', s.project);
  if (s.time) p.set('time', s.time);
  if (s.newest) p.set('order', 'newest');
  if (s.fuzzy) p.set('fuzzy', '1');
  const q = p.toString();
  return `#/search${q ? `?${q}` : ''}`;
}

/** How a hit names its kind, by its event's type; a title hit has none. */
function kindOf(type: string | undefined): { label: string; icon: ComponentChildren; mono: boolean } {
  if (type === undefined) return { label: 'Title', icon: null, mono: false };
  if (type === 'tool.call') return { label: 'Command', icon: <IconCommand />, mono: true };
  if (type === 'tool.result') return { label: 'Tool output', icon: <IconOutput />, mono: true };
  if (type === 'clarify.response') return { label: 'Answer', icon: <IconQuestion />, mono: false };
  return { label: 'Message', icon: <IconMessage />, mono: false };
}

/** The snippet with the query's words and the other forms the hub found (`also`) marked, the
 *  longest first, so a whole form is marked rather than the typed word inside it. */
export function Snippet({ text, query, also = [] }: { text: string; query: string; also?: string[] }) {
  const words = [
    ...new Set([
      ...query
        .toLowerCase()
        .split(/[^\p{L}\p{N}]+/u)
        .filter((w) => w && w !== 'or'),
      ...also,
    ]),
  ].sort((a, b) => b.length - a.length);
  if (!words.length) return <>{text}</>;
  const re = new RegExp(`(${words.map((w) => w.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|')})`, 'giu');
  return <>{text.split(re).map((part, i) => (i % 2 ? <mark key={i}>{part}</mark> : part))}</>;
}

/** A session's card: its hits shown, and those of "N more in this session" once opened. */
interface Opened {
  hits: SearchHit[];
  cursor?: string;
  busy: boolean;
}

/** The search page (`#/search`). `back` is where "‹ <project>" and Esc go. */
export function SearchPage({ app, phone, back }: { app: AppState; phone?: boolean; back: string }) {
  useSubscription((fn) => app.onChange(fn), [app]);
  const [state, setState] = useState(() => readSearch(location.hash));
  const [hits, setHits] = useState<SearchHit[]>([]);
  const [cursor, setCursor] = useState<string | undefined>();
  const [counts, setCounts] = useState<SearchCounts | undefined>();
  const [also, setAlso] = useState<string[]>([]);
  const [opened, setOpened] = useState<Map<string, Opened>>(new Map());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const root = useRef<HTMLElement>(null);
  const field = useRef<HTMLInputElement>(null);
  // Counts the searches: an answer to "More" of an earlier one is dropped.
  const generation = useRef(0);
  const q = state.q.trim();
  const fuzzy = state.fuzzy && q.length >= MIN_FUZZY;
  // The text and `since` of the last query asked (the address's text at first): a changed text
  // waits for the typing pause, a filter asks at once; its pages and "N more" keep its `since`.
  const asked = useRef<{ q: string; since?: string }>({ q });
  // What the counts held are of: the text, typos, project and time, not the kind or the order.
  const countsOf = useRef<string | null>(null);
  const set = (patch: Partial<SearchState>) => setState((s) => ({ ...s, ...patch }));

  // The filters of every request of this search.
  const options = (since: string | undefined): SearchOptions => ({
    fuzzy,
    kind: state.kind,
    project: state.project,
    since,
    newest: state.newest,
  });
  const addAlso = (words: string[]) => setAlso((cur) => [...cur, ...words.filter((w) => !cur.includes(w))]);

  useEffect(() => {
    field.current?.focus();
    field.current?.select();
  }, []);
  // The address follows the page without a new history entry.
  useEffect(() => {
    history.replaceState(history.state, '', searchHash(state));
  }, [state]);
  // Esc goes back, unless something else took it (the project bar's search, a menu).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || e.defaultPrevented) return;
      const t = e.target as Node | null;
      if (t && t !== document.body && !root.current?.contains(t)) return;
      e.preventDefault();
      location.hash = back;
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [back]);

  useEffect(() => {
    generation.current++;
    setHits([]);
    setCursor(undefined);
    setAlso([]);
    setOpened(new Map());
    setError(null);
    // A kind or the order keeps the counts while it asks (the tabs do not move under the
    // pointer).
    const of = JSON.stringify([q, fuzzy, state.project, state.time]);
    if (of !== countsOf.current) {
      setCounts(undefined);
      countsOf.current = null;
    }
    // Busy from the first key: no "Nothing found" while the typing pause runs.
    setBusy(q.length >= MIN_QUERY);
    if (q.length < MIN_QUERY) return;
    let live = true;
    const ask = () => {
      const t = TIMES.find((x) => x.id === state.time);
      const since = t?.ms ? new Date(Date.now() - t.ms).toISOString() : undefined;
      asked.current = { q, since };
      app.api
        .search(q, { ...options(since), perSession: CARD_HITS, limit: PAGE })
        .then(
          (page) => {
            if (!live) return;
            setHits(page.results);
            setCursor(page.next_cursor);
            setAlso(page.also);
            if (page.counts) {
              setCounts(page.counts);
              countsOf.current = of;
            }
          },
          (e: Error) => live && setError(e.message),
        )
        .finally(() => live && setBusy(false));
    };
    const typed = q !== asked.current.q;
    const timer = typed ? setTimeout(ask, SEARCH_DELAY_MS) : undefined;
    if (!typed) ask();
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [app, q, fuzzy, state.kind, state.project, state.time, state.newest]);

  const more = () => {
    const mine = generation.current;
    const live = () => mine === generation.current;
    setBusy(true);
    app.api
      .search(q, { ...options(asked.current.since), perSession: CARD_HITS, limit: PAGE, cursor })
      .then(
        (page) => {
          if (!live()) return;
          setHits((cur) => [...cur, ...page.results]);
          setCursor(page.next_cursor);
          addAlso(page.also);
        },
        (e: Error) => live() && setError(e.message),
      )
      .finally(() => live() && setBusy(false));
  };
  // "N more in this session": that session's hits, a page at a time.
  const openMore = (id: string) => {
    const was = opened.get(id);
    const mine = generation.current;
    const live = () => mine === generation.current;
    const put = (o: Opened) => live() && setOpened((m) => new Map(m).set(id, o));
    put({ hits: was?.hits ?? [], cursor: was?.cursor, busy: true });
    app.api.search(q, { ...options(asked.current.since), session: id, limit: PAGE, cursor: was?.cursor }).then(
      (page) => {
        put({ hits: [...(was?.hits ?? []), ...page.results], cursor: page.next_cursor, busy: false });
        if (live()) addAlso(page.also);
      },
      (e: Error) => {
        put({ hits: was?.hits ?? [], cursor: was?.cursor, busy: false });
        if (live()) setError(e.message);
      },
    );
  };

  // One card per session, in the order of its first hit.
  const cards = new Map<string, SearchHit[]>();
  for (const h of hits) cards.set(h.session_id, [...(cards.get(h.session_id) ?? []), h]);
  const projects = [...app.projects.values(), ...app.archivedProjects.values()];
  const projectOf = (h: SearchHit): Project | undefined => projects.find((p) => p.slug === h.address.split('/')[0]);
  const dense = app.lookName() === 'compact' && !phone;
  const now = Date.now();
  const current = app.currentProject();
  const total = counts?.[state.kind ?? 'all'];

  const tabs = (
    <div class="sp-kinds" role="group" aria-label="Kind of hit">
      {KIND_TABS.map((k) => (
        <button
          type="button"
          key={k.label}
          class={`sp-chip ${state.kind === k.id ? 'is-on' : ''}`}
          aria-pressed={state.kind === k.id}
          onClick={() => set({ kind: k.id })}
        >
          {k.label}
          {counts && <span class="sp-n">{counts[k.id ?? 'all'].hits}</span>}
        </button>
      ))}
    </div>
  );
  const selects = (
    <>
      <label class="sp-sel">
        <select
          aria-label="Project"
          value={state.project ?? ''}
          onChange={(e) => set({ project: (e.currentTarget as HTMLSelectElement).value || undefined })}
        >
          <option value="">All projects</option>
          {app.sortedProjects().map((p) => (
            <option key={p.id} value={p.id}>
              {projectName(p)}
            </option>
          ))}
        </select>
        <IconChevronDown />
      </label>
      <label class="sp-sel">
        <select aria-label="Time" value={state.time} onChange={(e) => set({ time: (e.currentTarget as HTMLSelectElement).value as Time })}>
          {TIMES.map((t) => (
            <option key={t.id} value={t.id}>
              {t.label}
            </option>
          ))}
        </select>
        <IconChevronDown />
      </label>
      <label class="sp-typos" title="Also words written with a typo; 3 characters at least">
        <input type="checkbox" checked={state.fuzzy} onChange={(e) => set({ fuzzy: (e.currentTarget as HTMLInputElement).checked })} />
        Also typos
      </label>
    </>
  );
  const order = (
    <div class="sp-seg" role="group" aria-label="Order">
      {[false, true].map((newest) => (
        <button
          type="button"
          key={String(newest)}
          class={state.newest === newest ? 'is-on' : ''}
          aria-pressed={state.newest === newest}
          onClick={() => set({ newest })}
        >
          {newest ? 'Newest' : 'Best match'}
        </button>
      ))}
    </div>
  );

  return (
    <section ref={root} class={`page search-page ${dense ? 'is-dense' : ''}`} aria-label="Search" aria-busy={busy}>
      <header class="bar sp-bar">
        <a class="sp-back" href={back} aria-label={`Back to ${current ? projectName(current) : 'the sessions'}`}>
          <IconBack size={16} />
          {!phone && <span class="sp-back-name">{current ? projectName(current) : 'Back'}</span>}
        </a>
        <label class="sp-q">
          <IconSearch size={phone ? 16 : 18} />
          <input
            ref={field}
            type="search"
            aria-label="Search sessions and messages"
            placeholder="Search sessions and messages"
            value={state.q}
            onInput={(e) => set({ q: (e.currentTarget as HTMLInputElement).value })}
          />
          {total && <span class="sp-total">{`${count(total.hits, 'result')} in ${count(total.sessions, 'session')}`}</span>}
        </label>
      </header>
      <div class="sp-scroll">
        <div class="sp-body">
          {dense ? (
            <div class="sp-fl">
              {tabs}
              {selects}
              {order}
            </div>
          ) : (
            <>
              <div class="sp-fl">{tabs}</div>
              <div class="sp-fl">
                {selects}
                {order}
              </div>
            </>
          )}
          {also.length > 0 && <div class="sp-also">Also finds: {also.join(', ')}</div>}
          {error && <div class="error-text">{error}</div>}
          {q.length < MIN_QUERY && <div class="sp-empty">Messages, commands, tool output and answers of every session</div>}
          {q.length >= MIN_QUERY && !busy && !error && hits.length === 0 && <div class="sp-empty">Nothing found</div>}
          {[...cards].map(([id, first]) => {
            const h0 = first[0];
            const p = projectOf(h0);
            const open = opened.get(id);
            const shown = open && open.hits.length ? open.hits : first;
            const left = h0.session_hits - shown.length;
            const age = app.sessions.get(id)?.updated_at;
            const name = p ? projectName(p) : h0.address.split('/')[0];
            const meta = [count(h0.session_hits, 'result'), age ? formatAge(age, now) : ''].filter(Boolean).join(' · ');
            return (
              <section key={id} class="sp-card" aria-label={h0.title || h0.address}>
                <div class="sp-head">
                  <span class="sp-tile" style={p ? { background: tileBackground(p) } : undefined} aria-hidden="true">
                    {p && <ProjectGlyph p={p} />}
                  </span>
                  <a class="sp-title" href={`#/s/${encodeURIComponent(id)}`}>
                    {h0.title || h0.address}
                  </a>
                  {h0.archived && <span class="sp-archived">archived</span>}
                  {dense && <span class="sp-proj">{name}</span>}
                  <span class="sp-meta">{dense ? meta : `${name} · ${meta}`}</span>
                </div>
                {shown.map((h) => {
                  const k = kindOf(h.type);
                  return (
                    <a key={`${h.seq ?? 'title'}`} class="sp-hit" href={hitHref(h)}>
                      <span class="sp-kind">
                        <span class="sp-kind-icon">{k.icon}</span>
                        {k.label}
                      </span>
                      <span class={`sp-x ${k.mono ? 'mono' : ''}`}>
                        <Snippet text={h.snippet} query={q} also={also} />
                      </span>
                      <span class="sp-when">{formatWhen(h.ts, now)}</span>
                    </a>
                  );
                })}
                {left > 0 && (
                  <button type="button" class="sp-more" disabled={open?.busy} onClick={() => openMore(id)}>
                    {`${left} more in this session ›`}
                  </button>
                )}
              </section>
            );
          })}
          {cursor && (
            <button type="button" class="btn btn-small sp-next" disabled={busy} onClick={more}>
              More results
            </button>
          )}
        </div>
      </div>
    </section>
  );
}
