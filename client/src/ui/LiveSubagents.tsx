// The subagents and the background commands the agent runs now (`subagents.live`, SPEC §6),
// pinned above the input while the feed scrolls: one line each, its task and what it does now,
// then its tokens, tool calls and run time, which ticks here between the lists the hub sends;
// a command has only its description and run time.

import type { LiveSubagent } from '../proto';
import { useNow } from './hooks';
import { count } from './labels';

/** `agents`: the latest list, which came at `receivedAt` (`Date.now()`); `now`: the clock
 *  (tests). Nothing for an empty list. */
export function LiveSubagents({
  agents,
  receivedAt,
  now,
}: {
  agents: LiveSubagent[];
  receivedAt: number;
  now?: () => number;
}) {
  const t = useNow(1000, agents.length > 0, now);
  if (!agents.length) return null;
  return (
    <section class="live-agents" aria-label="Running subagents and commands">
      <ul class="live-agents-list">
        {agents.map((a) => (
          <li key={a.task_id} class="live-agents-row">
            <span class="pulse-dot live-agents-dot" aria-hidden="true" />
            <span class="live-agents-what">
              <span class="live-agents-task">{a.description}</span>
              {a.activity ? `: ${a.activity}` : ''}
            </span>
            <span class="live-agents-stats">{stats(a, a.duration_ms + Math.max(0, t - receivedAt))}</span>
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
