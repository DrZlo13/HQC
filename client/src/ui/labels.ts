import type { ApprovalCanonical, ApprovalOption, CancelReason, PolicyHint, SessionState } from '../proto';

/** Button captions by `canonical` (SPEC §10.1); an unknown canonical keeps the agent's label. */
export const CANONICAL_LABELS: Record<Exclude<ApprovalCanonical, 'unknown'>, string> = {
  once: 'Разрешить',
  session: 'Разрешить в сессии',
  always: 'Разрешать всегда',
  deny: 'Отклонить',
  deny_always: 'Запрещать всегда',
};

export function optionLabel(o: ApprovalOption): string {
  return o.canonical === 'unknown' ? o.label : CANONICAL_LABELS[o.canonical];
}

export function isDeny(o: ApprovalOption): boolean {
  return o.canonical === 'deny' || o.canonical === 'deny_always';
}

export const STATE_LABELS: Record<SessionState, string> = {
  archived: 'в архиве',
  locked_vscode: 'ведётся в VS Code',
  worker_offline: 'воркер офлайн',
  waiting: 'ждёт ответа',
  error: 'ошибка',
  running: 'работает',
  queued: 'в очереди',
  idle: 'свободна',
  unknown: 'неизвестно',
};

/** The line under the chat while a turn is on. There is no event for it: it follows the
 *  session `state` (SPEC §17.1) and, while running, the last `tool.call` without a result. */
export function activityLabel(state: SessionState, tool?: string): string | null {
  switch (state) {
    case 'running':
      return tool ? `выполняет ${tool}` : 'агент работает';
    case 'queued':
      return 'ввод в очереди';
    case 'waiting':
      return 'ждёт подтверждения';
    default:
      return null;
  }
}

/** `approval.request.policy_hint` (SPEC §8.1, §14). */
export const POLICY_HINT_LABELS: Record<PolicyHint, string> = {
  allow: 'по правилу allow, но Claude Code спрашивает сам (например, путь вне рабочей папки)',
  deny: 'запрещено политикой воркера',
  unknown: 'есть правило в политике воркера',
};

/** `system.code` → caption before the text; other codes show the text alone (SPEC §4). */
export const SYSTEM_CODE_LABELS: Record<string, string> = {
  policy_denied: 'Запрещено политикой воркера',
  session_taken_over: 'Подхвачена из VS Code',
  session_released: 'Возвращена в VS Code',
  session_forked: 'Развилка',
};

/** `system.code`s drawn as a warning, with an icon (SPEC §9.3). */
export const SYSTEM_WARNING_CODES = new Set(['session_forked']);

/** `level` of a worker log line (`system {code: worker_log}`, SPEC §10.2). */
export const LOG_LEVEL_LABELS: Record<string, string> = {
  warn: 'предупреждение',
  error: 'ошибка',
};

/** `tool` of an `approval.request` in a worker's service session: the kind of
 *  `worker.approval.request` (SPEC §10.2). Other values are shown as they are. */
export const WORKER_REQUEST_LABELS: Record<string, string> = {
  'workspace.use': 'сессии в новой папке',
  'workspace.create': 'создание workspace',
  'workspace.delete': 'удаление workspace',
};

export const CANCEL_LABELS: Record<CancelReason, string> = {
  timeout: 'истёк срок',
  stopped: 'ход остановлен',
  worker_restart: 'воркер перезапущен',
  archived: 'сессия в архиве',
  unknown: 'отменён',
};

export function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms} мс`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)} с`;
  return `${Math.floor(ms / 60_000)} мин ${Math.round((ms % 60_000) / 1000)} с`;
}

/** Time with seconds, for log lines. */
export function formatClock(ts: string): string {
  const d = new Date(ts);
  if (Number.isNaN(d.getTime())) return '';
  return d.toLocaleTimeString('ru-RU', { hour: '2-digit', minute: '2-digit', second: '2-digit' });
}

export function formatTime(ts: string): string {
  const d = new Date(ts);
  if (Number.isNaN(d.getTime())) return '';
  return d.toLocaleTimeString('ru-RU', { hour: '2-digit', minute: '2-digit' });
}

/** First line, cut to `max` characters, for collapsed previews. */
export function oneLine(s: string, max = 80): string {
  const line = s.replace(/\s+/g, ' ').trim();
  return line.length > max ? `${line.slice(0, max - 1)}…` : line;
}
