import type { ApprovalCanonical, ApprovalOption, CancelReason, PermissionMode, SessionState } from '../proto';

/** Button captions by `canonical` (SPEC §10.1); an unknown canonical keeps the agent's label. */
export const CANONICAL_LABELS: Record<Exclude<ApprovalCanonical, 'unknown'>, string> = {
  once: 'Allow',
  session: 'Always in this session',
  always: 'Always allow',
  deny: 'Deny',
  deny_always: 'Always deny',
};

/** What became of a request after the answer, for its collapsed row. */
export const CANONICAL_OUTCOMES: Record<Exclude<ApprovalCanonical, 'unknown'>, string> = {
  once: 'allowed',
  session: 'allowed for the rest of the session',
  always: 'always allowed',
  deny: 'denied',
  deny_always: 'always denied',
};

export function optionLabel(o: ApprovalOption): string {
  return o.canonical === 'unknown' ? o.label : CANONICAL_LABELS[o.canonical];
}

export function isDeny(o: ApprovalOption): boolean {
  return o.canonical === 'deny' || o.canonical === 'deny_always';
}

/** The one line asked before a session goes to Auto (T38); Ask needs no confirmation. */
export const AUTO_CONFIRM =
  "Switch to Auto? Claude Code then runs tool calls without asking; the worker's deny list is the only limit.";

/** The one line asked before a session goes to Allow all (SPEC 0.129), as for Auto. */
export const ALLOW_ALL_CONFIRM =
  "Switch to Allow all? Claude Code then runs every tool call without asking, with no classifier either; the worker's deny list and the ask rules of Claude Code's settings are the only limits.";

/** A permission mode's name in the UI (T38). */
export const MODE_LABELS: Record<PermissionMode, string> = { ask: 'Ask', auto: 'Auto', allow_all: 'Allow all' };

/** The confirmation a switch to `mode` asks first; none for Ask. */
export function modeConfirm(mode: PermissionMode): string | undefined {
  return mode === 'auto' ? AUTO_CONFIRM : mode === 'allow_all' ? ALLOW_ALL_CONFIRM : undefined;
}

/** The state of a session in its header pill and in the session list. */
export const STATE_LABELS: Record<SessionState, string> = {
  archived: 'Archived',
  locked_vscode: 'In VS Code',
  worker_offline: 'Worker offline',
  waiting: 'Waiting for reply',
  error: 'Error',
  running: 'Running',
  queued: 'Queued',
  idle: 'Idle',
  unknown: 'Unknown',
};

/** The subagents a session's agent runs now, shown next to its state; nothing at 0. */
export function agentsLabel(n: number | undefined): string | undefined {
  if (!n || n < 1) return undefined;
  return n === 1 ? '1 agent' : `${n} agents`;
}

/** `system.code` → caption before the text; other codes show the text alone (SPEC §4). */
export const SYSTEM_CODE_LABELS: Record<string, string> = {
  policy_denied: 'Blocked by worker policy',
  session_taken_over: 'Taken over from VS Code',
  session_released: 'Returned to VS Code',
  session_forked: 'Fork',
  ask_cancelled: 'Question cancelled',
  peer_evicted: 'Messages dropped',
};

/** `system.code`s drawn as a warning, with an icon (SPEC §9.3). */
export const SYSTEM_WARNING_CODES = new Set(['session_forked']);

/** `level` of a worker log line (`system {code: worker_log}`, SPEC §10.2). */
export const LOG_LEVEL_LABELS: Record<string, string> = {
  warn: 'warning',
  error: 'error',
};

/** `tool` of an `approval.request` in a worker's service session: the kind of
 *  `worker.approval.request` (SPEC §10.2); and of the hub's card of a move after the
 *  transcript (SPEC §9.2). Other values are shown as they are. */
export const WORKER_REQUEST_LABELS: Record<string, string> = {
  'workspace.use': 'sessions in a new folder',
  'workspace.create': 'create a workspace',
  'workspace.delete': 'delete a workspace',
  'session.follow': 'move after the transcript',
};

export const CANCEL_LABELS: Record<CancelReason, string> = {
  timeout: 'timed out',
  stopped: 'turn stopped',
  worker_restart: 'worker restarted',
  archived: 'session archived',
  unknown: 'cancelled',
};

/** A count with an English noun: `count(1, 'command')` → `1 command`, `count(2, 'command')` →
 *  `2 commands`; `many` for irregular plurals. */
export function count(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}

export function formatDuration(ms: number): string {
  if (ms < 1000) return `${Math.max(0, Math.round(ms))} ms`;
  if (ms < 60_000) return `${Math.round(ms / 1000)} s`;
  return `${Math.floor(ms / 60_000)} min ${Math.round((ms % 60_000) / 1000)} s`;
}

/** A running timer: `0:42`, `1:24`, `1:02:03`. */
export function formatTimer(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, '0');
  return h ? `${h}:${String(m).padStart(2, '0')}:${ss}` : `${m}:${ss}`;
}

/** Time with seconds, for log lines. */
export function formatClock(ts: string): string {
  const d = new Date(ts);
  if (Number.isNaN(d.getTime())) return '';
  return d.toLocaleTimeString('en-US', { hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' });
}

export function formatTime(ts: string): string {
  const d = new Date(ts);
  if (Number.isNaN(d.getTime())) return '';
  return d.toLocaleTimeString('en-US', { hour: '2-digit', minute: '2-digit', hourCycle: 'h23' });
}

/** Whole days between the local dates of `ts` and `now` (0 — today, 1 — yesterday). */
export function daysAgo(ts: string, now: number): number {
  const d = new Date(ts);
  if (Number.isNaN(d.getTime())) return Infinity;
  const day = (t: Date) => new Date(t.getFullYear(), t.getMonth(), t.getDate()).getTime();
  return Math.round((day(new Date(now)) - day(d)) / 86_400_000);
}

/** A month and a day: `Oct 21`. */
function shortDate(ts: string): string {
  return new Date(ts).toLocaleDateString('en-US', { day: 'numeric', month: 'short' });
}

/** "today at 16:18", "yesterday at 09:05", "Oct 21 at 12:00". */
export function formatDay(ts: string, now: number): string {
  const days = daysAgo(ts, now);
  const at = `at ${formatTime(ts)}`;
  if (days === 0) return `today ${at}`;
  if (days === 1) return `yesterday ${at}`;
  return `${shortDate(ts)} ${at}`;
}

/** When a session was last active, short: "now", "4 min", "16:18", "yesterday", "Oct 21". */
export function formatWhen(ts: string, now: number): string {
  const t = Date.parse(ts);
  if (Number.isNaN(t)) return '';
  const min = Math.floor((now - t) / 60_000);
  if (min < 1) return 'now';
  if (min < 60) return `${min} min`;
  const days = daysAgo(ts, now);
  if (days === 0) return formatTime(ts);
  if (days === 1) return 'yesterday';
  return shortDate(ts);
}

/** How long ago, for a session's card: "just now", "4 min ago", "2 h ago", "3 d ago", then
 *  the date ("Oct 21"). */
export function formatAge(ts: string, now: number): string {
  const t = Date.parse(ts);
  if (Number.isNaN(t)) return '';
  const min = Math.floor((now - t) / 60_000);
  if (min < 1) return 'just now';
  if (min < 60) return `${min} min ago`;
  if (min < 24 * 60) return `${Math.floor(min / 60)} h ago`;
  if (min < 30 * 24 * 60) return `${Math.floor(min / (24 * 60))} d ago`;
  return shortDate(ts);
}

/** Token counts: `555`, `14.4k`, `1.2M`. */
export function formatTokens(n: number): string {
  if (n < 1000) return String(n);
  const [v, unit] = n < 1_000_000 ? [n / 1000, 'k'] : [n / 1_000_000, 'M'];
  return `${v.toFixed(v < 10 ? 1 : 0).replace(/\.0$/, '')}${unit}`;
}

/** Cost in dollars: `$0.07` (from a cent up; below it three digits), `precise`: always three,
 *  as under an answer (`$0.073`). */
export function formatCost(usd: number, precise = false): string {
  return `$${usd.toFixed(precise || usd < 0.01 ? 3 : 2)}`;
}

const MODEL_ID = /^claude-([a-z]+)-(\d+)(?:-(\d+))?(?:-\d{8})?$/;

/** `claude-opus-5-5` → `Opus 5.5`, an alias `fable` → `Fable`; other names as they are. */
export function modelName(model: string): string {
  const m = MODEL_ID.exec(model);
  if (!m) return /^[a-z]+$/.test(model) ? model[0].toUpperCase() + model.slice(1) : model;
  const family = m[1][0].toUpperCase() + m[1].slice(1);
  return `${family} ${m[2]}${m[3] ? `.${m[3]}` : ''}`;
}

/** The model the header names: the one the last turn used (`used`), unless the session was
 *  switched to another one since (`chosen`, an alias or a full name), which shows at once. */
export function shownModel(chosen: string | undefined, used: string | undefined): string | undefined {
  if (!chosen || !used) return chosen ?? used;
  return used === chosen || MODEL_ID.exec(used)?.[1] === chosen ? used : chosen;
}

/** First line, cut to `max` characters, for collapsed previews. */
export function oneLine(s: string, max = 80): string {
  const line = s.replace(/\s+/g, ' ').trim();
  return line.length > max ? `${line.slice(0, max - 1)}…` : line;
}

/** A path with the home folder as `~`: `/Users/me/work/hqc` → `~/work/hqc`. */
/** The path of a session row (T49): without `~/work/`; elsewhere in the home it keeps `~`. */
export function shortPath(p: string): string {
  return homePath(p).replace(/^~\/work\//, '');
}

export function homePath(p: string): string {
  return p.replace(/^\/(?:Users|home)\/[^/]+(?=\/|$)/, '~');
}
