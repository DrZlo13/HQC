import { useState } from 'preact/hooks';
import type { AppState } from '../app';
import { useSubscription } from './hooks';
import { STATE_LABELS } from './labels';

export function SessionList({ app, current }: { app: AppState; current?: string }) {
  useSubscription((fn) => app.onChange(fn), [app]);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const list = app.sortedSessions();

  const create = async () => {
    const title = prompt('Название сессии', 'Новая сессия');
    if (title === null) return;
    setCreating(true);
    setError(null);
    try {
      const s = await app.createSession(title.trim() || 'Новая сессия');
      location.hash = `#/s/${s.id}`;
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setCreating(false);
    }
  };

  return (
    <section class="sessions">
      <header class="bar">
        <div class="bar-title">
          <div class="title">Сессии</div>
          <div class="state">
            {app.me?.email ?? ''}
            {app.conn !== 'open' && ' · нет связи'}
          </div>
        </div>
        <a class="btn btn-small btn-ghost btn-icon" href="#/settings" aria-label="Настройки" title="Настройки">
          <svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3h.1a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8v.1a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z" />
          </svg>
        </a>
        <button type="button" class="btn btn-small btn-primary" disabled={creating || !app.me} onClick={create}>
          Новая
        </button>
      </header>
      {app.error && <div class="notice">Хаб недоступен: {app.error}</div>}
      {error && <div class="notice">{error}</div>}
      <ul class="session-list">
        {list.map((s) => (
          <li key={s.id}>
            <a class={`session-row ${s.id === current ? 'is-current' : ''}`} href={`#/s/${s.id}`}>
              <span class={`dot state-${s.state}`} aria-hidden="true" />
              <span class="session-main">
                <span class="session-title">{s.title || s.slug || s.id.slice(-8)}</span>
                <span class={`session-state state-${s.state}`}>
                  {STATE_LABELS[s.state]}
                  {s.queue_len > 0 && ` · в очереди ${s.queue_len}`}
                </span>
              </span>
              {s.unread > 0 && (
                <span class="badge" aria-label={`непрочитанных: ${s.unread}`}>
                  {s.unread > 99 ? '99+' : s.unread}
                </span>
              )}
            </a>
          </li>
        ))}
        {app.me && list.length === 0 && <li class="empty">Сессий пока нет</li>}
      </ul>
    </section>
  );
}
