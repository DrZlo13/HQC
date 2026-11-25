import { useEffect, useRef, useState } from 'preact/hooks';
import { IconBack } from './icons';
import { KEY_ACTIONS, defaultKey, keyFromEvent, keyLabel, sameKey, type KeyAction } from '../keys';
import { onBack } from '../nav';
import type { AppState } from '../app';
import { ApiError, type WorkerInfo } from '../api';
import type { LookName, Theme } from '../proto';
import { pushStatus, subscribePush, type PushStatus } from '../push';
import { BASES, LOOKS, THEMES } from '../theme';
import { WEB_SEARCHES } from '../websearch';
import { projectName } from './Appearance';
import { ProvidersSection } from './Providers';
import { McpServersSection } from './McpServers';
import { SkillsSection } from './Skills';
import { ClaudeSignIn, signsIn } from './ClaudeSignIn';

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

/** The system as people write it; anything else as the worker reports it. */
const OS_LABELS: Record<string, string> = { macos: 'macOS', linux: 'Linux', windows: 'Windows' };

/** An action's key and the button that records another: while it waits ("Press keys…") the window
 *  takes every key before the actions do. Esc cancels, Delete or Backspace leaves the action
 *  without a key, a key with ⌘, Ctrl or Alt is the new one; a bare key or a modifier alone keeps
 *  waiting. A press anywhere but on the button cancels (a click does not focus a button in WebKit,
 *  so its blur is no sign), and so does its blur where it had the focus. One row records at a time:
 *  `recording` is the section's. */
function KeyRow({
  app,
  action,
  label,
  recording,
  onRecording,
}: {
  app: AppState;
  action: KeyAction;
  label: string;
  recording: boolean;
  onRecording: (on: boolean) => void;
}) {
  const key = app.keyFor(action);
  const button = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (!recording) return;
    const record = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopImmediatePropagation();
      const bare = !e.metaKey && !e.ctrlKey && !e.altKey && !e.shiftKey;
      if (bare && e.code === 'Escape') onRecording(false);
      else if (bare && (e.code === 'Backspace' || e.code === 'Delete')) {
        onRecording(false);
        app.setKey(action, null);
      } else {
        const picked = keyFromEvent(e);
        if (!picked) return;
        onRecording(false);
        app.setKey(action, picked);
      }
    };
    const away = (e: PointerEvent) => {
      if (!button.current?.contains(e.target as Node)) onRecording(false);
    };
    addEventListener('keydown', record, true);
    addEventListener('pointerdown', away, true);
    return () => {
      removeEventListener('keydown', record, true);
      removeEventListener('pointerdown', away, true);
    };
  }, [app, action, recording, onRecording]);
  const shown = recording ? 'Press keys…' : key ? keyLabel(key) : 'None';
  return (
    <div class="ap-row key-row">
      <span class="ap-label">{label}</span>
      {!sameKey(key, defaultKey(action)) && (
        <button type="button" class="btn btn-small btn-key-default" onClick={() => app.setKey(action, undefined)}>
          Default
        </button>
      )}
      <button
        ref={button}
        type="button"
        class={`btn btn-small btn-key ${recording ? 'is-recording' : ''}`}
        aria-label={`Key of «${label}»: ${shown}`}
        onClick={() => onRecording(true)}
        onBlur={() => onRecording(false)}
      >
        {shown}
      </button>
    </div>
  );
}

/** `back`: where the arrow goes, the address before Settings (the list when there was none). */
export function Settings({ app, back = '#/' }: { app: AppState; back?: string }) {
  const [push, setPush] = useState<PushStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [themeError, setThemeError] = useState<string | null>(null);
  const [newSessionError, setNewSessionError] = useState<string | null>(null);
  const [lookError, setLookError] = useState<string | null>(null);
  const [workers, setWorkers] = useState<WorkerInfo[] | null>(null);
  const [forgetting, setForgetting] = useState<'idle' | 'busy' | 'done'>('idle');
  // The action whose key is being recorded: one at a time.
  const [recordingKey, setRecordingKey] = useState<KeyAction | null>(null);

  useEffect(() => {
    pushStatus().then(setPush, () => setPush('unsupported'));
    app.api.workers({ includeRevoked: true }).then(setWorkers, () => setWorkers(null));
  }, [app]);

  const forgetDevice = async () => {
    setForgetting('busy');
    await app.forgetDevice();
    setForgetting('done');
  };

  const revoke = async (w: WorkerInfo) => {
    if (!confirm(`Revoke the token of worker “${w.name}”? The machine will have to be enrolled again.`)) return;
    setError(null);
    try {
      await app.api.revokeWorker(w.id);
      setWorkers(await app.api.workers({ includeRevoked: true }));
      // Its service session is archived now: the "Workers" list drops it.
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

  /** The look for every device: applied as soon as the hub has it, no "Save". */
  const pickLook = async (l: LookName) => {
    setLookError(null);
    try {
      await app.setLookName(l);
    } catch (e) {
      setLookError((e as Error).message);
    }
  };

  /** The project of the quick-start button, for every device: kept as soon as the hub has it. */
  const pickNewSessionProject = async (id: string) => {
    setNewSessionError(null);
    try {
      await app.setNewSessionProject(id);
    } catch (e) {
      setNewSessionError((e as Error).message);
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
  const look = app.lookName();
  return (
    <section class="page">
      <header class="bar">
        <a class="back icon-btn" href={back} aria-label="Back" title="Back" onClick={(e) => onBack(e, back)}>
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

        <h2>Look</h2>
        <div class="seg-group settings-look" role="group" aria-label="Look">
          {LOOKS.map((l) => (
            <button
              type="button"
              key={l.id}
              class={`seg ${look === l.id ? 'is-on' : ''}`}
              aria-pressed={look === l.id}
              onClick={() => void pickLook(l.id)}
            >
              {l.label}
            </button>
          ))}
        </div>
        <p class="muted">
          Compact: the feed across the whole chat column, your messages as plates on the left, the
          system font, a line per session and each project's full name on the project bar. Spacious:
          the feed at most 780 px wide, your messages as bubbles on the right, Inter, two lines per
          session and a tile per project. The same on every device.
        </p>
        {lookError && <div class="error-text">{lookError}</div>}

        <h2>New session</h2>
        <select
          class="input settings-new-session"
          aria-label="Project of a new session"
          value={app.newSessionProject()?.id ?? ''}
          onChange={(e) => void pickNewSessionProject((e.target as HTMLSelectElement).value)}
        >
          {app.sortedProjects().map((p) => (
            <option key={p.id} value={p.id}>
              {projectName(p)}
            </option>
          ))}
        </select>
        <p class="muted">
          The floating button on a phone's first screen and the desktop app's shortcut open a new
          session in this project. The same on every device.
        </p>
        {newSessionError && <div class="error-text">{newSessionError}</div>}

        <h2>Web search</h2>
        <div class="seg-group settings-web-search" role="group" aria-label="Web search">
          {WEB_SEARCHES.map((w) => (
            <button
              type="button"
              key={w.id}
              class={`seg ${app.webSearch.id === w.id ? 'is-on' : ''}`}
              aria-pressed={app.webSearch.id === w.id}
              onClick={() => app.setWebSearch(w)}
            >
              {w.label}
            </button>
          ))}
        </div>
        <p class="muted">
          Where "Search in …" of the menu on a right click, and its key, search the text selected in the
          feed, in a new tab. On this device.
        </p>

        <h2>Keys</h2>
        <div class="settings-keys">
          {KEY_ACTIONS.map((a) => (
            <KeyRow
              key={a.id}
              app={app}
              action={a.id}
              label={a.label}
              recording={recordingKey === a.id}
              onRecording={(on) => setRecordingKey(on ? a.id : (now) => (now === a.id ? null : now))}
            />
          ))}
        </div>
        <p class="muted">
          Click a key and press the new one with ⌘, ⌃ or ⌥ (Ctrl or Alt off a Mac); Esc cancels,
          Delete leaves the action without a key. A key given to another action leaves that one
          without. On this device.
        </p>

        <ProvidersSection app={app} />

        <McpServersSection app={app} />

        <SkillsSection app={app} />

        <h2>Notifications</h2>
        <p class="muted">{push ? PUSH_TEXT[push] : '…'}</p>
        {(push === 'available' || push === 'subscribed') && (
          <button type="button" class="btn btn-primary" disabled={busy} onClick={subscribe}>
            {push === 'subscribed' ? 'Subscribe again' : 'Subscribe to notifications'}
          </button>
        )}
        {error && <div class="error-text">{error}</div>}

        <h2>This device</h2>
        <p class="muted">The copies of chats and attachments this device keeps, so that chats open without downloading them again.</p>
        <button type="button" class="btn" disabled={forgetting !== 'idle' || !app.deviceUser()} onClick={forgetDevice}>
          {forgetting === 'done' ? 'Forgotten' : 'Forget this device'}
        </button>

        <h2>Workers</h2>
        {workers && workers.length > 0 && (
          <ul class="plain-list worker-table">
            {workers.map((w) => (
              <li
                key={w.id}
                class={[w.status === 'revoked' && 'worker-revoked', signsIn(w) && 'worker-has-claude'].filter(Boolean).join(' ') || undefined}
              >
                <span class={`dot ${w.status === 'online' ? 'dot-online' : 'dot-offline'}`} aria-hidden="true" />
                <span class="worker-item-name">{w.name}</span>
                <span class="worker-item-meta">
                  {[w.os && (OS_LABELS[w.os] ?? w.os), w.status].filter(Boolean).join(' · ')}
                </span>
                {w.status === 'revoked' ? (
                  <button type="button" class="btn btn-small btn-revoke btn-purge" onClick={() => purge(w)}>
                    Delete
                  </button>
                ) : (
                  <button type="button" class="btn btn-small btn-revoke" onClick={() => revoke(w)}>
                    Revoke
                  </button>
                )}
                {signsIn(w) && <ClaudeSignIn app={app} worker={w} />}
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
