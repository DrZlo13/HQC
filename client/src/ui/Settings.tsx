import { useEffect, useState } from 'preact/hooks';
import type { AppState } from '../app';
import { ApiError, type WorkerInfo } from '../api';
import { pushStatus, subscribePush, type PushStatus } from '../push';

const PUSH_TEXT: Record<PushStatus, string> = {
  unsupported:
    'Браузер не поддерживает Web Push. На iPhone уведомления работают только из приложения, добавленного на экран «Домой».',
  insecure: 'Уведомления требуют HTTPS: откройте клиент через адрес хаба, а не по IP локальной сети.',
  denied: 'Уведомления запрещены в настройках браузера для этого сайта.',
  hub_off: 'На хабе не настроены ключи VAPID: уведомления выключены.',
  subscribed: 'Уведомления включены на этом устройстве.',
  available: 'Уведомления о запросах разрешений и ответах агента.',
};

export function Settings({ app }: { app: AppState }) {
  const [push, setPush] = useState<PushStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [workers, setWorkers] = useState<WorkerInfo[] | null>(null);

  useEffect(() => {
    pushStatus().then(setPush, () => setPush('unsupported'));
    app.api.workers({ includeRevoked: true }).then(setWorkers, () => setWorkers(null));
  }, [app]);

  const revoke = async (w: WorkerInfo) => {
    if (!confirm(`Отозвать токен воркера «${w.name}»? Машину придётся регистрировать заново.`)) return;
    setError(null);
    try {
      await app.api.revokeWorker(w.id);
      setWorkers(await app.api.workers({ includeRevoked: true }));
      // Its service session is archived now: the «Воркеры» group drops it.
      await app.refreshSessions().catch(() => {});
    } catch (e) {
      setError((e as Error).message);
    }
  };

  /** Deletes a revoked worker (SPEC §6); sessions still on it have to be moved or archived. */
  const purge = async (w: WorkerInfo) => {
    if (!confirm(`Удалить воркер «${w.name}» вместе с его служебной сессией и workspace?`)) return;
    setError(null);
    try {
      await app.api.purgeWorker(w.id);
      setWorkers(await app.api.workers({ includeRevoked: true }));
      await app.refreshSessions().catch(() => {});
    } catch (e) {
      if (e instanceof ApiError && e.code === 'worker_has_sessions') {
        const n = (e.body as { sessions_count?: number } | undefined)?.sessions_count;
        setError(
          `На воркере «${w.name}» ещё ${n ?? 'есть'} неархивных сессий. Перенесите их: запустите на машине ` +
            `hqc-worker enroll и при подтверждении выберите «Это та же машина, заменить «${w.name}»», ` +
            'или архивируйте их.',
        );
      } else {
        setError((e as Error).message);
      }
    }
  };

  const subscribe = async () => {
    setBusy(true);
    setError(null);
    try {
      setPush(await subscribePush(app.api));
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <section class="page">
      <header class="bar">
        <a class="back" href="#/" aria-label="К списку">
          ‹
        </a>
        <div class="bar-title">
          <div class="title">Настройки</div>
          <div class="state">{app.me?.email}</div>
        </div>
      </header>
      <div class="page-body">
        <h2>Уведомления</h2>
        <p class="muted">{push ? PUSH_TEXT[push] : '…'}</p>
        {(push === 'available' || push === 'subscribed') && (
          <button type="button" class="btn btn-primary" disabled={busy} onClick={subscribe}>
            {push === 'subscribed' ? 'Подписаться заново' : 'Подписаться на уведомления'}
          </button>
        )}
        {error && <div class="error-text">{error}</div>}

        <h2>Воркеры</h2>
        {workers && workers.length > 0 && (
          <ul class="plain-list">
            {workers.map((w) => (
              <li key={w.id} class={w.status === 'revoked' ? 'worker-revoked' : undefined}>
                <b>{w.name}</b>{' '}
                <span class="muted">
                  {[w.os, w.status === 'revoked' ? 'отозван' : w.status].filter(Boolean).join(' · ')}
                </span>{' '}
                {w.status === 'revoked' ? (
                  <button type="button" class="btn btn-revoke btn-purge" onClick={() => purge(w)}>
                    Удалить
                  </button>
                ) : (
                  <button type="button" class="btn btn-revoke" onClick={() => revoke(w)}>
                    Отозвать
                  </button>
                )}
              </li>
            ))}
          </ul>
        )}
        <a class="btn" href="#/enroll">
          Подтвердить код воркера
        </a>
      </div>
    </section>
  );
}
