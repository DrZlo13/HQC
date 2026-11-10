// The session list of a project (T26, T50): three orders — by state ("Needs you",
// "Running", "Idle"), by recency ("Today", "Yesterday", "Earlier") and the user's own
// ("Custom"), the choice and the custom order kept on the hub per user and project — a search
// over title and path, the counters of the header, the session a project reopens. No DOM.

import type { AppState } from '../app';
import type { ListMode, SessionInfo } from '../proto';
import { daysAgo, formatDay } from './labels';

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

/** Groups of the list in `mode`, empty ones left out; inside a group the newest first, in
 *  Custom the order of `placed`. */
export function groupSessions(list: SessionInfo[], mode: ListMode, now: number, placed: string[] = []): SessionGroup[] {
  const sorted = [...list].sort((a, b) => b.updated_at.localeCompare(a.updated_at) || b.last_seq - a.last_seq);
  if (mode === 'custom') {
    return sorted.length ? [{ id: 'custom', label: 'Custom order', plain: true, items: arrange(sorted, placed) }] : [];
  }
  const groups: SessionGroup[] =
    mode === 'state'
      ? [
          { id: 'wait', label: 'Needs you', wait: true, items: sorted.filter((s) => bucketOf(s) === 'wait') },
          { id: 'run', label: 'Running', items: sorted.filter((s) => bucketOf(s) === 'run') },
          { id: 'idle', label: 'Idle', items: sorted.filter((s) => bucketOf(s) === 'idle') },
        ]
      : [
          { id: 'today', label: 'Today', items: sorted.filter((s) => daysAgo(s.updated_at, now) <= 0) },
          { id: 'yesterday', label: 'Yesterday', items: sorted.filter((s) => daysAgo(s.updated_at, now) === 1) },
          { id: 'earlier', label: 'Earlier', items: sorted.filter((s) => daysAgo(s.updated_at, now) > 1) },
        ];
  return groups.filter((g) => g.items.length > 0);
}

/** The search: every word of `query` is in the title or the path, case aside. */
export function matchesQuery(s: SessionInfo, path: string, query: string): boolean {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return true;
  const hay = `${s.title} ${s.slug} ${path}`.toLowerCase();
  return words.every((w) => hay.includes(w));
}

/** The project's counters: "1 needs you · 2 running", the idle ones only when nothing runs. */
export function headCounts(list: SessionInfo[]): string {
  const n = { wait: 0, run: 0, idle: 0 };
  for (const s of list) n[bucketOf(s)]++;
  const parts: string[] = [];
  if (n.wait) parts.push(`${n.wait} needs you`);
  if (n.run) parts.push(`${n.run} running`);
  if (n.idle && (!n.run || !n.wait)) parts.push(`${n.idle} idle`);
  return parts.length ? parts.join(' · ') : 'No sessions yet';
}

/** "1 active", "5 active", "none active". */
export function activeLabel(n: number): string {
  return n ? `${n} active` : 'none active';
}

/** Initials for the project rail: "physics-mp" → "PM", "HQC" → "HQ", "Personal" → "P"; any script works,
 *  a project named in Russian gets Cyrillic initials. */
export function projectInitials(name: string, personal = false): string {
  if (personal) return name.slice(0, 1).toUpperCase();
  const words = name.split(/[\s\-_./]+/).filter(Boolean);
  if (words.length >= 2) return (words[0][0] + words[1][0]).toUpperCase();
  return (words[0] ?? name).slice(0, 2).toUpperCase();
}
