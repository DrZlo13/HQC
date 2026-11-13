// The subagents, the background commands and the timers of the session (`subagents.live` and
// `schedules.live`, SPEC §6), pinned above the input while the feed scrolls: one line each,
// led by "Agent:", "Command:" (when the worker says which), "Monitor:", "Wakeup:", "Cron:" or
// "Scheduled:". A subagent has its task and what it does now (that it retries a request to the
// model API, while it does, or that it waits for permission), then its tokens, tool calls and
// run time, which ticks here between the lists the hub sends; a command or a watcher has only
// its description and run time. A timer, a wakeup or a cron job of the agent and a message
// scheduled by the hub, has "in 12m 34s" instead, counting down to "due"; a scheduled message
// opens the schedules of the chat. A `stoppable` line ends with a stop button.

import { useState } from 'preact/hooks';
import type { LiveKind, LiveSchedule, LiveSubagent } from '../proto';
import { useNow } from './hooks';
import { IconStop } from './icons';
import { count, retryLabel } from './labels';

const PREFIX: Record<LiveKind, string> = {
  agent: 'Agent: ',
  command: 'Command: ',
  monitor: 'Monitor: ',
  wakeup: 'Wakeup: ',
  cron: 'Cron: ',
};

/** What the line says: the word before, the task, what follows it (the task's activity, or a
 *  cron job's schedule), what stands on the right, and whether the dot stays still (a timer
 *  waits, it does not run). */
interface Row {
  key: string;
  prefix: string;
  task: string;
  after: string;
  right: string;
  timer: boolean;
  open?: () => void;
  /** The `task_id` its stop button stops: a `stoppable` row's. */
  stop?: string;
}

/** `agents`: the latest list, which came at `receivedAt` (`Date.now()`); `schedules`: the
 *  latest `schedules.live`, which came at `schedulesAt`; `onSchedules` opens the schedules;
 *  `asking`: the `task_id`s an open permission request names (since 0.175), whose row says it
 *  waits for it instead of what it does; `now`: the clock (tests). Nothing when both lists are
 *  empty. A `stoppable` row (a subagent's, a command's or a Monitor's) gets a stop button,
 *  which calls `onStop` with its `task_id` and stays disabled until that settles; the row goes
 *  when the task ends. */
export function LiveSubagents({
  agents,
  receivedAt,
  schedules,
  schedulesAt,
  onSchedules,
  asking,
  now,
  onStop,
}: {
  agents: LiveSubagent[];
  receivedAt: number;
  schedules: LiveSchedule[];
  schedulesAt: number;
  onSchedules: () => void;
  asking?: ReadonlySet<string>;
  now?: () => number;
  onStop?: (taskId: string) => Promise<void>;
}) {
  const t = useNow(1000, agents.length + schedules.length > 0, now);
  const rows: Row[] = [
    ...agents.map((a) => agentRow(a, t - receivedAt, asking)),
    ...schedules.map((s) => scheduleRow(s, t - schedulesAt, onSchedules)),
  ];
  const [stopping, setStopping] = useState<ReadonlySet<string>>(new Set());
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
      <ul class="live-agents-list">
        {rows.map((r) => (
          <li key={r.key} class="live-agents-row">
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
            <span class="live-agents-stats">{r.right}</span>
            {r.stop && onStop && (
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
            )}
          </li>
        ))}
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
    right: waits ? countdown(a.due_in_ms, age) : stats(a, a.duration_ms + Math.max(0, age)),
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
    right: countdown(s.due_in_ms, age),
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
