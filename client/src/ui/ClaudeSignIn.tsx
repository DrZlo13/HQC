// The sign-in of a worker's own Claude Code from hqc (SPEC 0.185): a line under the worker in
// Settings → Workers with how it is signed in, and the flow that signs it in again. The worker
// runs `claude auth login` itself; the page it gives is opened here and the code that page
// shows is sent back. The code is used once and kept nowhere: not in a draft, not in storage.

import { useEffect, useState } from 'preact/hooks';
import type { AppState } from '../app';
import type { ClaudeAuthStatus, ClaudeLogin, WorkerInfo } from '../api';

/** The hosts a sign-in page may be on (and their subdomains). */
const SIGN_IN_HOSTS = ['claude.com', 'claude.ai', 'platform.claude.com'];

/** `url` if it is an https page of Anthropic's sign-in, else `null`. */
export function signInHref(url: string): string | null {
  try {
    const u = new URL(url);
    if (u.protocol !== 'https:') return null;
    const host = u.hostname.toLowerCase();
    return SIGN_IN_HOSTS.some((h) => host === h || host.endsWith(`.${h}`)) ? u.href : null;
  } catch {
    return null;
  }
}

/** Whether the worker's Claude Code can be signed in from hqc. */
export function signsIn(w: WorkerInfo): boolean {
  return w.status === 'online' && w.capabilities?.claude_code?.login === true;
}

const capital = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

/** "Claude: owner@x · Team · until 12 Dec", or "Claude: not signed in". */
export function claudeLine(s: ClaudeAuthStatus): string {
  if (!s.logged_in) return 'Claude: not signed in';
  const until = s.expires_at && !Number.isNaN(Date.parse(s.expires_at))
    ? `until ${new Date(s.expires_at).toLocaleDateString('en-GB', { day: 'numeric', month: 'short' })}`
    : undefined;
  return ['Claude: ' + (s.email ?? 'signed in'), s.subscription_type && capital(s.subscription_type), until]
    .filter(Boolean)
    .join(' · ');
}

const errorText = (e: unknown) => (e instanceof Error ? e.message : String(e));

export function ClaudeSignIn({ app, worker }: { app: AppState; worker: WorkerInfo }) {
  const [status, setStatus] = useState<ClaudeAuthStatus | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [login, setLogin] = useState<ClaudeLogin | null>(null);
  const [code, setCode] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    app.api.claudeAuth(worker.id).then(
      (s) => live && setStatus(s),
      (e) => live && setLoadError(errorText(e)),
    );
    return () => {
      live = false;
    };
  }, [app, worker.id]);

  const start = async () => {
    setError(null);
    setBusy(true);
    try {
      const l = await app.api.claudeLogin(worker.id);
      if (!signInHref(l.url)) {
        setError('The worker gave a sign-in page that is not Anthropic’s: not opened.');
        await app.api.claudeLoginCancel(worker.id, l.login_id).catch(() => {});
        return;
      }
      setLogin(l);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  const send = async (e: Event) => {
    e.preventDefault();
    if (!login || !code.trim()) return;
    const typed = code.trim();
    // A one-time secret: off the screen and out of the state at once, whatever comes back.
    setCode('');
    setError(null);
    setBusy(true);
    try {
      const done = await app.api.claudeLoginCode(worker.id, login.login_id, typed);
      setStatus(done.status);
      if (done.ok) setLogin(null);
      else setError(done.message || 'The sign-in did not go through.');
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  const cancel = async () => {
    const l = login;
    setLogin(null);
    setCode('');
    setError(null);
    if (l) await app.api.claudeLoginCancel(worker.id, l.login_id).catch(() => {});
  };

  const again = async () => {
    await cancel();
    await start();
  };

  const href = login ? signInHref(login.url) : null;
  return (
    <div class="worker-claude">
      <div class="worker-claude-line">
        <span class={`worker-claude-status ${status && !status.logged_in ? 'is-signed-out' : ''}`}>
          {status ? claudeLine(status) : loadError ? 'Claude: status not available' : 'Claude: …'}
        </span>
        {status?.error && <span class="muted worker-claude-error">{status.error}</span>}
        {loadError && <span class="muted worker-claude-error">{loadError}</span>}
        {!login && (
          <button type="button" class="btn btn-small btn-claude-sign-in" disabled={busy} onClick={() => void start()}>
            {status?.logged_in ? 'Sign in again' : 'Sign in'}
          </button>
        )}
      </div>
      {login && href && (
        <form class="worker-claude-form" onSubmit={send}>
          <a class="btn btn-small claude-sign-in-page" href={href} target="_blank" rel="noopener noreferrer">
            Open the sign-in page
          </a>
          <input
            class="input claude-sign-in-code"
            name="claude_code"
            placeholder="Paste the code"
            aria-label="Paste the code"
            autocomplete="off"
            spellcheck={false}
            value={code}
            onInput={(e) => setCode((e.currentTarget as HTMLInputElement).value)}
          />
          <button type="submit" class="btn btn-small btn-primary btn-claude-send" disabled={busy || !code.trim()}>
            Send
          </button>
          {error && (
            <button type="button" class="btn btn-small btn-claude-again" disabled={busy} onClick={() => void again()}>
              Start again
            </button>
          )}
          <button type="button" class="btn btn-small btn-claude-cancel" disabled={busy} onClick={() => void cancel()}>
            Cancel
          </button>
        </form>
      )}
      {error && <div class="error-text worker-claude-form-error">{error}</div>}
    </div>
  );
}
