// History search (SPEC §15): the hub's hits for a query, under the session list on a
// desktop and on a screen of its own on a phone. A hit opens its session at the event.

import { useEffect, useState } from 'preact/hooks';
import type { AppState } from '../app';
import type { SearchHit } from '../proto';
import { useSubscription } from './hooks';
import { IconBack, IconSearch } from './icons';
import { formatWhen, homePath } from './labels';
import { matchesQuery } from './sessions';

/** Shortest query sent to the hub, and shortest fuzzy one (the hub refuses shorter). */
export const MIN_QUERY = 2;
const MIN_FUZZY = 3;
/** Typing pause before a request. */
export const SEARCH_DELAY_MS = 300;

/** `#/s/<id>?seq=<seq>`: the chat scrolls to the event; a title hit opens the session. */
export function hitHref(h: SearchHit): string {
  return `#/s/${encodeURIComponent(h.session_id)}${h.seq !== undefined ? `?seq=${h.seq}` : ''}`;
}

/** The snippet with the query's words marked. */
export function Snippet({ text, query }: { text: string; query: string }) {
  const words = query
    .toLowerCase()
    .split(/[^\p{L}\p{N}]+/u)
    .filter((w) => w && w !== 'or');
  if (!words.length) return <>{text}</>;
  const re = new RegExp(`(${words.map((w) => w.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|')})`, 'giu');
  return <>{text.split(re).map((part, i) => (i % 2 ? <mark key={i}>{part}</mark> : part))}</>;
}

/** The hub's hits for `query`, newest first, with "More" for the next page. */
export function SearchHits({ app, query }: { app: AppState; query: string }) {
  const [fuzzy, setFuzzy] = useState(false);
  const [hits, setHits] = useState<SearchHit[]>([]);
  const [cursor, setCursor] = useState<string | undefined>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const q = query.trim();
  const useFuzzy = fuzzy && q.length >= MIN_FUZZY;

  useEffect(() => {
    setHits([]);
    setCursor(undefined);
    setError(null);
    if (q.length < MIN_QUERY) return;
    let live = true;
    const timer = setTimeout(() => {
      setBusy(true);
      app.api
        .search(q, { fuzzy: useFuzzy })
        .then(
          (page) => {
            if (!live) return;
            setHits(page.results);
            setCursor(page.next_cursor);
          },
          (e: Error) => live && setError(e.message),
        )
        .finally(() => live && setBusy(false));
    }, SEARCH_DELAY_MS);
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [app, q, useFuzzy]);

  if (q.length < MIN_QUERY) return null;
  const more = () => {
    setBusy(true);
    app.api
      .search(q, { fuzzy: useFuzzy, cursor })
      .then(
        (page) => {
          setHits((cur) => [...cur, ...page.results]);
          setCursor(page.next_cursor);
        },
        (e: Error) => setError(e.message),
      )
      .finally(() => setBusy(false));
  };
  const now = Date.now();
  return (
    <div class="search-hits" aria-busy={busy}>
      <div class="group-head">
        <span class="group-label">In messages</span>
        <label class="search-fuzzy" title="Also other word forms and typos">
          <input type="checkbox" checked={fuzzy} onChange={(e) => setFuzzy((e.currentTarget as HTMLInputElement).checked)} />
          Similar words
        </label>
      </div>
      {error && <div class="error-text">{error}</div>}
      <ul class="session-list">
        {hits.map((h) => (
          <li key={`${h.session_id}:${h.seq ?? 'title'}`}>
            <a class="session-row search-hit" href={hitHref(h)}>
              <span class="session-main">
                <span class="session-top">
                  <span class="session-title">{h.title || h.address}</span>
                  {h.archived && <span class="search-archived">archived</span>}
                  <span class="session-when">{formatWhen(h.ts, now)}</span>
                </span>
                <span class="search-snippet">
                  <Snippet text={h.snippet} query={q} />
                </span>
                <span class="session-where mono">{h.address}</span>
              </span>
            </a>
          </li>
        ))}
      </ul>
      {!busy && !error && hits.length === 0 && <div class="empty">No messages found</div>}
      {cursor && (
        <button type="button" class="btn btn-small search-more" disabled={busy} onClick={more}>
          More
        </button>
      )}
    </div>
  );
}

/** The phone's search screen (`#/search`): the sessions whose title or path match, from all
 *  projects, then the hub's hits. */
export function SearchScreen({ app }: { app: AppState }) {
  useSubscription((fn) => app.onChange(fn), [app]);
  const [query, setQuery] = useState('');
  const pathOf = (s: Parameters<AppState['sessionPath']>[0]) => homePath(app.sessionPath(s) ?? '');
  const sessions = query.trim()
    ? [...app.sessions.values()].filter((s) => !s.archived && !s.system && matchesQuery(s, pathOf(s), query))
    : [];
  return (
    <section class="sessions sessions-phone search-screen">
      <header class="m-head">
        <div class="m-head-row">
          <a class="icon-btn icon-btn-round" href="#/" aria-label="Back to the sessions">
            <IconBack size={20} />
          </a>
          <label class="search">
            <IconSearch size={14} />
            <input
              type="search"
              aria-label="Search sessions and messages"
              placeholder="Search sessions and messages"
              autoFocus
              value={query}
              onInput={(e) => setQuery((e.currentTarget as HTMLInputElement).value)}
            />
          </label>
        </div>
      </header>
      <div class="session-scroll">
        {sessions.length > 0 && (
          <div class="session-group">
            <div class="group-head">
              <span class="group-label">Sessions</span>
              <span class="group-count">{sessions.length}</span>
            </div>
            <ul class="session-list">
              {sessions.map((s) => (
                <li key={s.id}>
                  <a class="session-row" href={`#/s/${s.id}`}>
                    <span class="session-main">
                      <span class="session-title">{s.title || s.slug}</span>
                      <span class="session-where mono">{pathOf(s)}</span>
                    </span>
                  </a>
                </li>
              ))}
            </ul>
          </div>
        )}
        <SearchHits app={app} query={query} />
      </div>
    </section>
  );
}
