import type { ApprovalCanonical, ApprovalOption, CancelReason, SessionState } from '../proto';

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
  locked_vscode: 'занята VS Code',
  worker_offline: 'воркер офлайн',
  waiting: 'ждёт ответа',
  error: 'ошибка',
  running: 'работает',
  queued: 'в очереди',
  idle: 'свободна',
  unknown: 'неизвестно',
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
