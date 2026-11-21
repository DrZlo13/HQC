// The subagents, the background commands and the timers of the session (`subagents.live` and
// `schedules.live`, SPEC §6), pinned above the input while the feed scrolls: one line each,
// led by "Agent:", "Command:" (when the worker says which), "Monitor:", "Wakeup:", "Cron:" or
// "Scheduled:". A subagent has its task and what it does now (that it retries a request to the
// model API, while it does, or that it waits for permission), then its tokens, tool calls and
// run time, which ticks here between the lists the hub sends (a phone leaves out the word
// "tokens" and the tool calls); a command or a watcher has only its description and run time.
// A timer, a wakeup or a cron job of the agent and a message scheduled by the hub, has
// "in 12m 34s" instead, counting down to "due"; a scheduled message opens the schedules of the
// chat. A `stoppable` line ends with a stop button. A subagent's line opens what it has done
// under it, drawn as the feed draws the agent's own steps.

import type { ComponentChildren } from 'preact';
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'preact/hooks';
import type { LiveKind, LiveSchedule, LiveSubagent, SubagentHistory } from '../proto';
import { Timeline, type Item } from '../timeline';
import { FeedBlocks, type ChatActions } from './cards';
import { buildFeed } from './feed';
import { useNow } from './hooks';
import { IconStop } from './icons';
import { count, retryLabel } from './labels';

/** How often an open history is fetched again. */
const REFRESH_MS = 2000;

/** Most events an open history keeps, as many as the worker sends in one page (SPEC §5.3). */
const MAX_SUBAGENT_HISTORY = 200;

/** What a subagent's line needs to open its history (since 0.181): `load` fetches the history,
 *  only what came after event `after` when it is given; `actions` draw it as the feed does. A
 *  command's line (`kind: command`), a Monitor's, a wakeup's and a cron job's open none. */
export interface SubagentHistoryOpener {
  load(taskId: string, after?: string): Promise<SubagentHistory>;
  actions: ChatActions;
}

const PREFIX: Record<LiveKind, string> = {
  agent: 'Agent: ',
  command: 'Command: ',
  monitor: 'Monitor: ',
  wakeup: 'Wakeup: ',
  cron: 'Cron: ',
};

/** What the line says: the word before, the task, what follows it (the task's activity, or a
 *  cron job's schedule), what stands on the right (its `.live-agents-stats` span), and whether
 *  the dot stays still (a timer waits, it does not run). */
interface Row {
  key: string;
  prefix: string;
  task: string;
  after: string;
  right: ComponentChildren;
  timer: boolean;
  open?: () => void;
  /** The line opens what its subagent has done under it (since 0.181). */
  history?: boolean;
  /** The `task_id` its stop button stops: a `stoppable` row's. */
  stop?: string;
}

/** `agents`: the latest list, which came at `receivedAt` (`Date.now()`); `schedules`: the
 *  latest `schedules.live`, which came at `schedulesAt`; `onSchedules` opens the schedules;
 *  `asking`: the `task_id`s an open permission request names (since 0.181), whose row says it
 *  waits for it instead of what it does; `history`: the lines that open what their subagent has
 *  done (one at a time; it goes with its line); `now`: the clock (tests). Nothing when both
 *  lists are empty. A `stoppable` row (a subagent's, a command's or a Monitor's) gets a stop
 *  button, which calls `onStop` with its `task_id` and stays disabled until that settles; the
 *  row goes when the task ends. */
export function LiveSubagents({
  agents,
  receivedAt,
  schedules,
  schedulesAt,
  onSchedules,
  asking,
  history,
  now,
  onStop,
}: {
  agents: LiveSubagent[];
  receivedAt: number;
  schedules: LiveSchedule[];
  schedulesAt: number;
  onSchedules: () => void;
  asking?: ReadonlySet<string>;
  history?: SubagentHistoryOpener;
  now?: () => number;
  onStop?: (taskId: string) => Promise<void>;
}) {
  const t = useNow(1000, agents.length + schedules.length > 0, now);
  const rows: Row[] = [
    // A subagent's line: `kind` `agent`, or none (an older worker, a value this client does not
    // know); a command's, a Monitor's, a wakeup's and a cron job's are not.
    ...agents.map((a) => ({
      ...agentRow(a, t - receivedAt, asking),
      history: !!history && (a.kind === undefined || a.kind === 'agent'),
    })),
    ...schedules.map((s) => scheduleRow(s, t - schedulesAt, onSchedules)),
  ];
  const [stopping, setStopping] = useState<ReadonlySet<string>>(new Set());
  const [open, setOpen] = useState<string | null>(null);
  const shown = open !== null && history && rows.some((r) => r.key === open && r.history) ? open : null;
  // The subagent ended: its history goes with its line, and does not open again by itself.
  useEffect(() => {
    if (open !== null && shown === null) setOpen(null);
  }, [open, shown]);
  if (!rows.length) return null;
  const stop = (id: string) => {
    setStopping((s) => new Set(s).add(id));
    void onStop!(id).finally(() =>
      setStopping((s) => {
        const left = new Set(s);
        left.delete(id);
        return left;
      }),
    );
  };
  return (
    <section class="live-agents" aria-label="Running subagents and commands, and timers">
      <ul class={shown ? 'live-agents-list live-agents-list-open' : 'live-agents-list'}>
        {rows.map((r) => {
          const line = (
            <>
              <span class={`pulse-dot live-agents-dot ${r.timer ? 'live-agents-dot-still' : ''}`} aria-hidden="true" />
              {r.open ? (
                <button type="button" class="live-agents-what live-agents-open" title="Open the schedules" onClick={r.open}>
                  <RowText row={r} />
                </button>
              ) : (
                <span class="live-agents-what">
                  <RowText row={r} />
                </span>
              )}
              {r.right}
            </>
          );
          // Beside the line that opens the history, not in it: a button holds no other.
          const stopButton = r.stop && onStop && (
            <button
              type="button"
              class="icon-btn live-agents-stop"
              aria-label="Stop this task"
              aria-describedby={`live-task-${r.key}`}
              title="Stop this task"
              disabled={stopping.has(r.stop)}
              onClick={() => stop(r.stop!)}
            >
              <IconStop size={10} />
            </button>
          );
          return (
            <li key={r.key}>
              <div class="live-agents-row">
                {r.history ? (
                  <button
                    type="button"
                    class="live-agents-opens"
                    aria-expanded={shown === r.key}
                    onClick={() => setOpen((o) => (o === r.key ? null : r.key))}
                  >
                    {line}
                  </button>
                ) : (
                  line
                )}
                {stopButton}
              </div>
              {shown === r.key && history && <HistoryPanel taskId={r.key} history={history} />}
            </li>
          );
        })}
      </ul>
    </section>
  );
}

function RowText({ row }: { row: Row }) {
  return (
    <>
      {row.prefix && <span class="live-agents-kind">{row.prefix}</span>}
      <span class="live-agents-task" id={`live-task-${row.key}`}>
        {row.task}
      </span>
      {row.after}
    </>
  );
}

/** `age`: how long the list has been here; `asking`: as for `LiveSubagents`. */
function agentRow(a: LiveSubagent, age: number, asking?: ReadonlySet<string>): Row {
  const waits = a.kind === 'wakeup' || a.kind === 'cron';
  return {
    key: a.task_id,
    prefix: a.kind ? PREFIX[a.kind] : '',
    task: a.description,
    // A cron job's schedule is a part of what it is, not what it does now.
    after: asking?.has(a.task_id)
      ? ': waiting for permission'
      : a.retry
        ? `: ${retryLabel(a.retry)}`
        : a.activity
          ? `${a.kind === 'cron' ? ' · ' : ': '}${a.activity}`
          : '',
    right: waits ? (
      <span class="live-agents-stats">{countdown(a.due_in_ms, age)}</span>
    ) : (
      <Stats cls="live-agents-stats" a={a} ms={a.duration_ms + Math.max(0, age)} />
    ),
    timer: waits,
    stop: a.stoppable ? a.task_id : undefined,
  };
}

function scheduleRow(s: LiveSchedule, age: number, open: () => void): Row {
  return {
    key: s.schedule_id,
    prefix: 'Scheduled: ',
    task: s.message,
    after: '',
    right: <span class="live-agents-stats">{countdown(s.due_in_ms, age)}</span>,
    timer: true,
    open,
  };
}

/** `in 12m 34s` until the fire, `due` once it is; nothing when its time is not known. */
function countdown(dueInMs: number | undefined, age: number): string {
  if (dueInMs === undefined) return '';
  const left = dueInMs - Math.max(0, age);
  return left > 0 ? `in ${elapsed(left)}` : 'due';
}

/** What subagent `taskId` has done, newest at the bottom, fetched again `REFRESH_MS` after each
 *  answer while it is open and the page is not hidden, and at once when the page is shown again,
 *  each time only what came after its last event; it stays at the bottom while the user has not
 *  scrolled up. */
function HistoryPanel({ taskId, history }: { taskId: string; history: SubagentHistoryOpener }) {
  const [page, setPage] = useState<SubagentHistory | null>(null);
  const [error, setError] = useState<string | null>(null);
  // The page shown, for the fetches: its last event is where the next one goes on from.
  const held = useRef<SubagentHistory | null>(null);
  // The chat makes a new opener on every render: the fetches follow the line, not it.
  const load = useRef(history.load);
  load.current = history.load;
  useEffect(() => {
    let alive = true;
    let busy = false;
    let next: ReturnType<typeof setTimeout> | undefined;
    const get = () => {
      clearTimeout(next);
      busy = true;
      void load
        .current(taskId, held.current?.events.at(-1)?.event_id)
        .then(
          (p) => {
            if (!alive) return;
            held.current = held.current && p.continues ? appended(held.current, p) : p;
            setPage(held.current);
            setError(null);
          },
          (e: Error) => {
            if (alive) setError(e.message);
          },
        )
        .finally(() => {
          busy = false;
          // Not while the page is hidden: shown again, it is fetched at once.
          if (alive) next = setTimeout(() => !document.hidden && get(), REFRESH_MS);
        });
    };
    const shown = () => {
      if (!document.hidden && !busy) get();
    };
    get();
    document.addEventListener('visibilitychange', shown);
    return () => {
      alive = false;
      clearTimeout(next);
      document.removeEventListener('visibilitychange', shown);
    };
  }, [taskId]);
  // Its own timeline: the calls find their results in it. The events come without `seq`; their
  // order is it, and their ids keep the rows in place as the window moves.
  const feed = useMemo(() => {
    const tl = new Timeline();
    const events = (page?.events ?? []).map((e, i) => ({ ...e, seq: i + 1 }));
    tl.applyAll(events);
    const items = events.map((e): Item => ({ kind: 'event', key: e.event_id, event: e }));
    return { timeline: tl, blocks: buildFeed(items, tl) };
  }, [page]);
  const box = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const lastTop = useRef(0);
  useLayoutEffect(() => {
    const el = box.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  });
  return (
    <div
      class="live-agents-history"
      ref={box}
      role="region"
      aria-label="What the subagent has done"
      onScroll={(e) => {
        // As the feed: only a move up lets go of the bottom.
        const el = e.currentTarget as HTMLDivElement;
        const near = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
        if (near || el.scrollTop < lastTop.current) stick.current = near;
        lastTop.current = el.scrollTop;
      }}
    >
      {page?.truncated && <div class="live-agents-note">Earlier steps are not shown.</div>}
      {page && !page.events.length && <div class="live-agents-note">Nothing yet.</div>}
      {!page && !error && <div class="live-agents-note">Loading…</div>}
      {error && <div class="live-agents-note">{error}</div>}
      <div class="feed live-agents-feed">
        <FeedBlocks blocks={feed.blocks} actions={{ ...history.actions, timeline: feed.timeline }} />
      </div>
    </div>
  );
}

/** `page` with the events of `more`, a reply with `continues`, after it: an event it already
 *  shows is not added again, only the newest `MAX_SUBAGENT_HISTORY` stay, and it is `truncated`
 *  when it dropped any or already was. Nothing new: `page` itself. */
function appended(page: SubagentHistory, more: SubagentHistory): SubagentHistory {
  const shown = new Set(page.events.map((e) => e.event_id));
  const added = more.events.filter((e) => !shown.has(e.event_id));
  if (!added.length) return page;
  const events = [...page.events, ...added];
  const drop = Math.max(0, events.length - MAX_SUBAGENT_HISTORY);
  return { events: events.slice(drop), truncated: page.truncated || drop > 0 };
}

/** `155.3k tokens · 188 tools · 4m 13s`; what the subagent has not reported yet is left out,
 *  and the run time without `ms`. Also the status line of its card in the feed, its end card
 *  and the end of a task's line (cards.tsx). */
export function stats(a: Pick<LiveSubagent, 'total_tokens' | 'tool_uses'>, ms?: number): string {
  const parts: string[] = [];
  if (a.total_tokens !== undefined) parts.push(`${tokens(a.total_tokens)} tokens`);
  if (a.tool_uses !== undefined) parts.push(count(a.tool_uses, 'tool'));
  if (ms !== undefined) parts.push(elapsed(ms));
  return parts.join(' · ');
}

/** `stats(a, ms)` in a span of class `cls`, after `lead`: a phone hides the word "tokens"
 *  (`.stats-word`) and the tool calls with their separator (`.stats-tools`, styles.css) for the
 *  room of the task, and keeps `155.3k · 4m 13s`; with only the tool calls, the whole span. */
export function Stats({
  cls,
  a,
  ms,
  lead = '',
}: {
  cls: string;
  a: Pick<LiveSubagent, 'total_tokens' | 'tool_uses'>;
  ms?: number;
  lead?: string;
}) {
  const shown = a.total_tokens !== undefined || ms !== undefined;
  // The separator that goes with the tool calls is hidden with them: before them after the
  // tokens, after them before the run time when they come first.
  const tools =
    a.tool_uses === undefined
      ? null
      : a.total_tokens !== undefined
        ? ` · ${count(a.tool_uses, 'tool')}`
        : `${count(a.tool_uses, 'tool')}${ms !== undefined ? ' · ' : ''}`;
  return (
    <span class={shown ? cls : `${cls} stats-tools`}>
      {lead}
      {a.total_tokens !== undefined && (
        <>
          {tokens(a.total_tokens)}
          <span class="stats-word"> tokens</span>
        </>
      )}
      {tools !== null && <span class="stats-tools">{tools}</span>}
      {ms !== undefined && `${a.total_tokens !== undefined ? ' · ' : ''}${elapsed(ms)}`}
    </span>
  );
}

/** As VS Code shows them: `950`, `155.3k`, `1.2M`. */
function tokens(n: number): string {
  if (n < 1000) return String(n);
  return n < 1_000_000 ? `${(n / 1000).toFixed(1)}k` : `${(n / 1_000_000).toFixed(1)}M`;
}

/** `13s`, `4m 13s`, `1h 4m 13s`. */
export function elapsed(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (h) return `${h}h ${m}m ${s % 60}s`;
  return m ? `${m}m ${s % 60}s` : `${s % 60}s`;
}
