import { useEffect, useState } from 'preact/hooks';
import { IconBack } from './icons';
import type { AppState } from '../app';
import { ApiError, type WorkerInfo } from '../api';
import type { Theme } from '../proto';
import { pushStatus, subscribePush, type PushStatus } from '../push';
import { BASES, THEMES } from '../theme';

const THEME_GROUPS = [
  { id: 'dark', label: 'Dark' },
  { id: 'light', label: 'Light' },
] as const;

/** A theme tile's swatch: the background at 60 %, then the cards (the border for both
 *  high-contrast themes). */
function swatch(id: Theme): string {
  const b = BASES[id];
  const side = id === 'contrast' || id === 'contrast_light' ? b.border : b.surface;
  return `linear-gradient(90deg, ${b.bg} 0 60%, ${side} 60% 100%)`;
}

const PUSH_TEXT: Record<PushStatus, string> = {
  unsupported:
    'This browser does not support Web Push. On an iPhone notifications only work from the app added to the Home Screen.',
  insecure: 'Notifications need HTTPS: open the client at the hub\'s address, not at a local network IP.',
  denied: 'Notifications are blocked for this site in the browser settings.',
  hub_off: 'The hub has no VAPID keys configured: notifications are off.',
  subscribed: 'Notifications are on for this device.',
  available: 'Notifications about permission requests and agent replies.',
};

export function Settings({ app }: { app: AppState }) {
  const [push, setPush] = useState<PushStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [themeError, setThemeError] = useState<string | null>(null);
  const [workers, setWorkers] = useState<WorkerInfo[] | null>(null);

  useEffect(() => {
    pushStatus().then(setPush, () => setPush('unsupported'));
    app.api.workers({ includeRevoked: true }).then(setWorkers, () => setWorkers(null));
  }, [app]);

  const revoke = async (w: WorkerInfo) => {
    if (!confirm(`Revoke the token of worker “${w.name}”? The machine will have to be enrolled again.`)) return;
    setError(null);
    try {
      await app.api.revokeWorker(w.id);
      setWorkers(await app.api.workers({ includeRevoked: true }));
      // Its service session is archived now: the "Workers" group drops it.
      await app.reloadSessions().catch(() => {});
    } catch (e) {
      setError((e as Error).message);
    }
  };

  /** Deletes a revoked worker (SPEC §6); sessions still on it have to be moved or archived. */
  const purge = async (w: WorkerInfo) => {
    if (!confirm(`Delete worker “${w.name}” with its service session and workspaces?`)) return;
    setError(null);
    try {
      await app.api.purgeWorker(w.id);
      setWorkers(await app.api.workers({ includeRevoked: true }));
      await app.reloadSessions().catch(() => {});
    } catch (e) {
      if (e instanceof ApiError && e.code === 'worker_has_sessions') {
        const n = (e.body as { sessions_count?: number } | undefined)?.sessions_count;
        setError(
          `Worker “${w.name}” still has ${n ?? 'some'} unarchived sessions. Move them: run ` +
            `hqc-worker enroll on the machine and, when confirming, choose “The same machine, replace “${w.name}””, ` +
            'or archive them.',
        );
      } else {
        setError((e as Error).message);
      }
    }
  };

  /** The app theme (T49) for every project and device: applied at once, no "Save". */
  const pickTheme = async (t: Theme) => {
    setThemeError(null);
    try {
      await app.setTheme(t);
    } catch (e) {
      setThemeError((e as Error).message);
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

  const theme = app.theme();
  return (
    <section class="page">
      <header class="bar">
        <a class="back icon-btn" href="#/" aria-label="Back to the list">
          <IconBack />
        </a>
        <div class="bar-title">
          <div class="title">Settings</div>
          <div class="state">{app.me?.email}</div>
        </div>
      </header>
      <div class="page-body">
        <h2>Theme</h2>
        <div class="theme-picks">
          {THEME_GROUPS.map((g) => (
            <div key={g.id} class="theme-group" role="group" aria-label={`${g.label} themes`}>
              <span class="theme-group-label">{g.label}</span>
              <div class="base-picks">
                {THEMES.filter((t) => t.group === g.id).map((t) => (
                  <button
                    type="button"
                    key={t.id}
                    class={`base-pick ${theme === t.id ? 'is-on' : ''}`}
                    aria-pressed={theme === t.id}
                    onClick={() => void pickTheme(t.id)}
                  >
                    <span class="base-swatch" style={{ background: swatch(t.id) }} />
                    <span>{t.label}</span>
                  </button>
                ))}
              </div>
            </div>
          ))}
        </div>
        {themeError && <div class="error-text">{themeError}</div>}

        <h2>Notifications</h2>
        <p class="muted">{push ? PUSH_TEXT[push] : '…'}</p>
        {(push === 'available' || push === 'subscribed') && (
          <button type="button" class="btn btn-primary" disabled={busy} onClick={subscribe}>
            {push === 'subscribed' ? 'Subscribe again' : 'Subscribe to notifications'}
          </button>
        )}
        {error && <div class="error-text">{error}</div>}

        <h2>Workers</h2>
        {workers && workers.length > 0 && (
          <ul class="plain-list">
            {workers.map((w) => (
              <li key={w.id} class={w.status === 'revoked' ? 'worker-revoked' : undefined}>
                <b>{w.name}</b>{' '}
                <span class="muted">
                  {[w.os, w.status === 'revoked' ? 'revoked' : w.status].filter(Boolean).join(' · ')}
                </span>{' '}
                {w.status === 'revoked' ? (
                  <button type="button" class="btn btn-revoke btn-purge" onClick={() => purge(w)}>
                    Delete
                  </button>
                ) : (
                  <button type="button" class="btn btn-revoke" onClick={() => revoke(w)}>
                    Revoke
                  </button>
                )}
              </li>
            ))}
          </ul>
        )}
        <a class="btn" href="#/enroll">
          Confirm a worker code
        </a>
      </div>
    </section>
  );
}
