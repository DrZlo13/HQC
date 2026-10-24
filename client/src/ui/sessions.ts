// The session list of a project (T26): two orders — by state («Ждёт вас», «Работает»,
// «Свободны») and by recency («Сегодня», «Вчера», «Раньше»), the choice kept in this
// browser's localStorage — a search over title and path, the counters of the header.
// Pure logic, no DOM.

import type { SessionInfo } from '../proto';
import { daysAgo, plural } from './labels';

export type ListMode = 'state' | 'recent';

export const LIST_MODES: { id: ListMode; label: string }[] = [
  { id: 'state', label: 'Состояние' },
  { id: 'recent', label: 'Недавние' },
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

/** Which group of «Состояние» a session is in: it waits for the user (a request or an
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
      t = 'Ждёт ответа';
      break;
    case 'error':
      t = 'Ошибка';
      break;
    case 'running':
      t = 'Работает';
      break;
    case 'queued':
      t = 'В очереди';
      break;
    case 'locked_vscode':
      t = s.held_by ? (s.held_by.status === 'busy' ? 'Работает в VS Code' : 'Открыта в VS Code') : 'Ведётся в VS Code';
      break;
    case 'worker_offline':
      t = 'Воркер офлайн';
      break;
    case 'archived':
      t = 'В архиве';
      break;
    case 'idle':
      t = 'Свободна';
      break;
    default:
      t = 'Неизвестно';
  }
  return s.queue_len > 0 && s.state !== 'queued' ? `${t} · в очереди ${s.queue_len}` : t;
}

export interface SessionGroup {
  id: string;
  label: string;
  /** «Ждёт вас» is drawn in amber. */
  wait?: boolean;
  items: SessionInfo[];
}

/** Groups of the list in `mode`, empty ones left out; inside a group the newest first. */
export function groupSessions(list: SessionInfo[], mode: ListMode, now: number): SessionGroup[] {
  const sorted = [...list].sort((a, b) => b.updated_at.localeCompare(a.updated_at) || b.last_seq - a.last_seq);
  const groups: SessionGroup[] =
    mode === 'state'
      ? [
          { id: 'wait', label: 'Ждёт вас', wait: true, items: sorted.filter((s) => bucketOf(s) === 'wait') },
          { id: 'run', label: 'Работает', items: sorted.filter((s) => bucketOf(s) === 'run') },
          { id: 'idle', label: 'Свободны', items: sorted.filter((s) => bucketOf(s) === 'idle') },
        ]
      : [
          { id: 'today', label: 'Сегодня', items: sorted.filter((s) => daysAgo(s.updated_at, now) <= 0) },
          { id: 'yesterday', label: 'Вчера', items: sorted.filter((s) => daysAgo(s.updated_at, now) === 1) },
          { id: 'earlier', label: 'Раньше', items: sorted.filter((s) => daysAgo(s.updated_at, now) > 1) },
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

/** The project's counters: «1 ждёт вас · 2 работают», the free ones only when nothing works. */
export function headCounts(list: SessionInfo[]): string {
  const n = { wait: 0, run: 0, idle: 0 };
  for (const s of list) n[bucketOf(s)]++;
  const parts: string[] = [];
  if (n.wait) parts.push(`${n.wait} ждёт вас`);
  if (n.run) parts.push(`${n.run} ${plural(n.run, ['работает', 'работают', 'работают'])}`);
  if (n.idle && (!n.run || !n.wait)) parts.push(`${n.idle} ${plural(n.idle, ['свободна', 'свободны', 'свободны'])}`);
  return parts.length ? parts.join(' · ') : 'Сессий пока нет';
}

/** «1 активная», «2 активные», «5 активных», «нет активных». */
export function activeLabel(n: number): string {
  return n ? `${n} ${plural(n, ['активная', 'активные', 'активных'])}` : 'нет активных';
}

/** Initials for the project rail: «physics-mp» → «PM», «HQC» → «HQ», «Личный» → «Л». */
export function projectInitials(name: string, personal = false): string {
  if (personal) return name.slice(0, 1).toUpperCase();
  const words = name.split(/[\s\-_./]+/).filter(Boolean);
  if (words.length >= 2) return (words[0][0] + words[1][0]).toUpperCase();
  return (words[0] ?? name).slice(0, 2).toUpperCase();
}
