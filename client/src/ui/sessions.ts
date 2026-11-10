// The session list of a project (T26, T50): three orders — by state ("Needs you",
// "Running", then the idle ones by recency), by recency ("Today", "Yesterday", "Last 7 days",
// "This month", "Earlier") and the user's own ("Custom"), the choice and the custom order kept
// on the hub per user and project — a search over title and path, the last line of a turn for
// a row's card, the session a project reopens. No DOM.

import type { AppState } from '../app';
import type { HqcEvent, ListMode, SessionInfo } from '../proto';
import { callPeek } from './feed';
import { daysAgo, formatDay, oneLine } from './labels';

export const LIST_MODES: { id: ListMode; label: string }[] = [
  { id: 'state', label: 'State' },
  { id: 'recent', label: 'Recent' },
  { id: 'custom', label: 'Custom' },
];

/** `list` (newest activity first) in the order of `placed`, as the hub orders it: ids not in
 *  `list` dropped, sessions missing from `placed` (one created since) first. */
export function arrange(list: SessionInfo[], placed: string[]): SessionInfo[] {
  const at = new Map(placed.map((id, i) => [id, i]));
  const missing = list.filter((s) => !at.has(s.id));
  const kept = list.filter((s) => at.has(s.id)).sort((a, b) => at.get(a.id)! - at.get(b.id)!);
  return [...missing, ...kept];
}

/** `order` with `id` moved to position `to` (clamped); the same array when nothing moves. */
export function moveTo(order: string[], id: string, to: number): string[] {
  const from = order.indexOf(id);
  const j = Math.max(0, Math.min(order.length - 1, to));
  if (from < 0 || from === j) return order;
  const out = order.filter((x) => x !== id);
  out.splice(j, 0, id);
  return out;
}

/** The session last open in each project, on this device: a convenience, so storage that
 *  fails or is empty only means the project opens without one. */
const LAST_KEY = 'hqc.lastSession';
function lastSessions(): Record<string, string> {
  try {
    return JSON.parse(localStorage.getItem(LAST_KEY) ?? '{}') ?? {};
  } catch {
    return {};
  }
}
export function rememberSession(projectId: string, sessionId: string) {
  try {
    localStorage.setItem(LAST_KEY, JSON.stringify({ ...lastSessions(), [projectId]: sessionId }));
  } catch {
    // Not kept: the project opens without it.
  }
}

/** The session a project opens on a desktop: the one last open in it, while it is still in
 *  the project and not archived. */
export function reopenedSession(app: AppState, projectId: string): string | undefined {
  const last = app.sessions.get(lastSessions()[projectId] ?? '');
  return last && last.project_id === projectId && !last.archived ? last.id : undefined;
}

/** Which group of "State" a session is in: it waits for the user (a request or an
 *  error to retry), works (a turn, queued input, a subagent or a background task still at
 *  work after the turn ended, VS Code busy with it) or is free. */
export type Bucket = 'wait' | 'run' | 'idle';

/** The turn ended and a background subagent or task still works: the session counts as
 *  working. */
export function atWorkAfterTurn(s: Pick<SessionInfo, 'state' | 'subagents' | 'background_tasks'>): boolean {
  return s.state === 'idle' && ((s.subagents ?? 0) > 0 || (s.background_tasks ?? 0) > 0);
}

export function bucketOf(s: SessionInfo): Bucket {
  if (s.state === 'waiting' || s.state === 'error') return 'wait';
  if (s.state === 'running' || s.state === 'queued') return 'run';
  if (atWorkAfterTurn(s)) return 'run';
  if (s.state === 'locked_vscode' && s.held_by?.status === 'busy') return 'run';
  return 'idle';
}

/** "Archived today at 23:45"; only "Archived" for a session archived before the hub kept
 *  the time (T50). */
export function archivedLabel(s: SessionInfo, now: number): string {
  return s.archived_at ? `Archived ${formatDay(s.archived_at, now)}` : 'Archived';
}

/** The line under the title. */
export function subLabel(s: SessionInfo): string {
  let t: string;
  switch (s.state) {
    case 'waiting':
      t = 'Waiting for reply';
      break;
    case 'error':
      t = 'Error';
      break;
    case 'running':
      t = 'Running';
      break;
    case 'queued':
      t = 'Queued';
      break;
    case 'locked_vscode':
      t = s.held_by ? (s.held_by.status === 'busy' ? 'Running in VS Code' : 'Open in VS Code') : 'In VS Code';
      break;
    case 'worker_offline':
      t = 'Worker offline';
      break;
    case 'archived':
      t = 'Archived';
      break;
    case 'idle':
      // Running, as its group, while a background subagent or task works.
      t = atWorkAfterTurn(s) ? 'Running' : 'Idle';
      break;
    default:
      t = 'Unknown';
  }
  return s.queue_len > 0 && s.state !== 'queued' ? `${t} · ${s.queue_len} queued` : t;
}

export interface SessionGroup {
  id: string;
  label: string;
  /** "Needs you" is drawn in amber. */
  wait?: boolean;
  /** One list in the user's order: no count. */
  plain?: boolean;
  items: SessionInfo[];
}

/** How long ago a session was last active, as the list's groups say it (a row has no time):
 *  today, yesterday, the five days before them, earlier in this month, before it. */
export type Recency = 'today' | 'yesterday' | 'week' | 'month' | 'earlier';

export const RECENCY: { id: Recency; label: string }[] = [
  { id: 'today', label: 'Today' },
  { id: 'yesterday', label: 'Yesterday' },
  { id: 'week', label: 'Last 7 days' },
  { id: 'month', label: 'This month' },
  { id: 'earlier', label: 'Earlier' },
];

export function recencyOf(ts: string, now: number): Recency {
  const days = daysAgo(ts, now);
  if (days <= 0) return 'today';
  if (days === 1) return 'yesterday';
  if (days < 7) return 'week';
  const d = new Date(ts);
  const n = new Date(now);
  return d.getFullYear() === n.getFullYear() && d.getMonth() === n.getMonth() ? 'month' : 'earlier';
}

/** Groups of the list in `mode`, empty ones left out; inside a group the newest first, in
 *  Custom the order of `placed`. In State the idle sessions go by recency ("Idle · Today"…);
 *  no group is ever collapsed. */
export function groupSessions(list: SessionInfo[], mode: ListMode, now: number, placed: string[] = []): SessionGroup[] {
  const sorted = [...list].sort((a, b) => b.updated_at.localeCompare(a.updated_at) || b.last_seq - a.last_seq);
  if (mode === 'custom') {
    return sorted.length ? [{ id: 'custom', label: 'Custom order', plain: true, items: arrange(sorted, placed) }] : [];
  }
  const byRecency = (items: SessionInfo[], id: string, label: string): SessionGroup[] =>
    RECENCY.map((r) => ({
      id: `${id}${r.id}`,
      label: `${label}${r.label}`,
      items: items.filter((s) => recencyOf(s.updated_at, now) === r.id),
    }));
  const groups: SessionGroup[] =
    mode === 'state'
      ? [
          { id: 'wait', label: 'Needs you', wait: true, items: sorted.filter((s) => bucketOf(s) === 'wait') },
          { id: 'run', label: 'Running', items: sorted.filter((s) => bucketOf(s) === 'run') },
          ...byRecency(
            sorted.filter((s) => bucketOf(s) === 'idle'),
            'idle-',
            'Idle · ',
          ),
        ]
      : byRecency(sorted, '', '');
  return groups.filter((g) => g.items.length > 0);
}

/** The last line of a session's turn, for its row's card: what the agent said, thought or
 *  ran last, or the user's message; undefined before its chat is loaded. */
export function lastLine(events: HqcEvent[]): string | undefined {
  for (let i = events.length - 1; i >= 0; i--) {
    const e = events[i];
    if (e.type === 'agent.message' || e.type === 'user.message') return oneLine(e.payload.text, 160);
    if (e.type === 'agent.thinking') return `Thinking · ${oneLine(e.payload.text, 160)}`;
    if (e.type === 'tool.call') return oneLine(callPeek(e.payload.tool, e.payload.args_preview, undefined), 160);
  }
  return undefined;
}

/** The search: every word of `query` is in the title or the path, case aside. */
export function matchesQuery(s: SessionInfo, path: string, query: string): boolean {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return true;
  const hay = `${s.title} ${s.slug} ${path}`.toLowerCase();
  return words.every((w) => hay.includes(w));
}

/** "1 running", "5 running", "none running": a worker's sessions in the list's Running
 *  group ([`bucketOf`] "run"). */
export function runningLabel(n: number): string {
  return n ? `${n} running` : 'none running';
}

/** Initials for the project rail: "physics-mp" → "PM", "HQC" → "HQ", "Personal" → "P"; any script works,
 *  a project named in Russian gets Cyrillic initials. */
export function projectInitials(name: string, personal = false): string {
  if (personal) return name.slice(0, 1).toUpperCase();
  const words = name.split(/[\s\-_./]+/).filter(Boolean);
  if (words.length >= 2) return (words[0][0] + words[1][0]).toUpperCase();
  return (words[0] ?? name).slice(0, 2).toUpperCase();
}
