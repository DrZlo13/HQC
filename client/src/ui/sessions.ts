// The session list of a project (T26): two orders — by state ("Needs you", "Running",
// "Idle") and by recency ("Today", "Yesterday", "Earlier"), the choice kept in this
// browser's localStorage — a search over title and path, the counters of the header.
// Pure logic, no DOM.

import type { SessionInfo } from '../proto';
import { daysAgo } from './labels';

export type ListMode = 'state' | 'recent';

export const LIST_MODES: { id: ListMode; label: string }[] = [
  { id: 'state', label: 'State' },
  { id: 'recent', label: 'Recent' },
];

const MODE_KEY = 'hqc.sessionOrder';

function storage(): Storage | undefined {
  try {
    return globalThis.localStorage ?? undefined;
  } catch {
    // Storage may be off (privacy settings): the default order then.
    return undefined;
  }
}

export function loadMode(): ListMode {
  const v = storage()?.getItem(MODE_KEY);
  return v === 'recent' ? 'recent' : 'state';
}

export function saveMode(mode: ListMode): void {
  try {
    storage()?.setItem(MODE_KEY, mode);
  } catch {
    // Full or off: the choice lives until the page reloads.
  }
}

/** Which group of "State" a session is in: it waits for the user (a request or an
 *  error to retry), works (a turn, queued input, VS Code busy with it) or is free. */
export type Bucket = 'wait' | 'run' | 'idle';

export function bucketOf(s: SessionInfo): Bucket {
  if (s.state === 'waiting' || s.state === 'error') return 'wait';
  if (s.state === 'running' || s.state === 'queued') return 'run';
  if (s.state === 'locked_vscode' && s.held_by?.status === 'busy') return 'run';
  return 'idle';
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
      t = 'Idle';
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
  items: SessionInfo[];
}

/** Groups of the list in `mode`, empty ones left out; inside a group the newest first. */
export function groupSessions(list: SessionInfo[], mode: ListMode, now: number): SessionGroup[] {
  const sorted = [...list].sort((a, b) => b.updated_at.localeCompare(a.updated_at) || b.last_seq - a.last_seq);
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
