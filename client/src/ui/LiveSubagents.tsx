// The subagents and the background commands the agent runs now (`subagents.live`, SPEC §6),
// pinned above the input while the feed scrolls: one line each, "Agent:" or "Command:" (when
// the worker says which), its task and what it does now, then its tokens, tool calls and run
// time, which ticks here between the lists the hub sends; a command has only its description
// and run time. A subagent's line ends with a stop button.

import { useState } from 'preact/hooks';
import type { LiveSubagent } from '../proto';
import { useNow } from './hooks';
import { IconStop } from './icons';
import { count } from './labels';

/** `agents`: the latest list, which came at `receivedAt` (`Date.now()`); `now`: the clock
 *  (tests). Nothing for an empty list. A `stoppable` row (a subagent's) gets a stop button,
 *  which calls `onStop` with its `task_id` and stays disabled until that settles; the row goes
 *  when the subagent ends. */
export function LiveSubagents({
  agents,
  receivedAt,
  now,
  onStop,
}: {
  agents: LiveSubagent[];
  receivedAt: number;
  now?: () => number;
  onStop?: (taskId: string) => Promise<void>;
}) {
  const t = useNow(1000, agents.length > 0, now);
  const [stopping, setStopping] = useState<ReadonlySet<string>>(new Set());
  if (!agents.length) return null;
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
    <section class="live-agents" aria-label="Running subagents and commands">
      <ul class="live-agents-list">
        {agents.map((a) => (
          <li key={a.task_id} class="live-agents-row">
            <span class="pulse-dot live-agents-dot" aria-hidden="true" />
            <span class="live-agents-what">
              {a.kind && <span class="live-agents-kind">{a.kind === 'agent' ? 'Agent: ' : 'Command: '}</span>}
              <span class="live-agents-task">{a.description}</span>
              {a.activity ? `: ${a.activity}` : ''}
            </span>
            <span class="live-agents-stats">{stats(a, a.duration_ms + Math.max(0, t - receivedAt))}</span>
            {a.stoppable && onStop && (
              <button
                type="button"
                class="icon-btn live-agents-stop"
                aria-label="Stop the subagent"
                title="Stop the subagent"
                disabled={stopping.has(a.task_id)}
                onClick={() => stop(a.task_id)}
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

/** `155.3k tokens · 188 tools · 4m 13s`; what the subagent has not reported yet is left out.
 *  Also the status line of its card in the feed (cards.tsx). */
export function stats(a: LiveSubagent, ms: number): string {
  const parts: string[] = [];
  if (a.total_tokens !== undefined) parts.push(`${tokens(a.total_tokens)} tokens`);
  if (a.tool_uses !== undefined) parts.push(count(a.tool_uses, 'tool'));
  parts.push(elapsed(ms));
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
