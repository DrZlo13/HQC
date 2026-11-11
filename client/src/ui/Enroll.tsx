import { useEffect, useState } from 'preact/hooks';
import { IconBack } from './icons';
import type { AppState } from '../app';
import { onBack } from '../nav';
import type { EnrollInfo, ReplaceableWorker } from '../proto';

/** `ab cd-1234` -> `ABCD-1234` once all 8 characters are there, else `null`. */
export function normalizeCode(s: string): string | null {
  const c = s.replace(/[^0-9a-z]/gi, '').toUpperCase();
  return c.length === 8 ? `${c.slice(0, 4)}-${c.slice(4)}` : null;
}

type Info = { code: string; info: EnrollInfo } | { code: string; error: string } | null;

function replaceableNote(w: ReplaceableWorker): string {
  const parts = [w.os, w.status === 'revoked' ? 'revoked' : 'offline'];
  if (w.last_seen) parts.push(`last seen ${new Date(w.last_seen).toLocaleString()}`);
  parts.push(`sessions: ${w.sessions_count}`);
  return parts.filter(Boolean).join(' · ');
}

/** Confirms the code `hqc-worker enroll <hub>` printed (SPEC §5.1). Before the button is
 *  enabled the screen shows what the worker said about itself (name, OS, when, country):
 *  a link with a code may come from someone else's machine. If the user has offline or
 *  revoked workers, the screen offers "the same machine, replace <name>": the new worker
 *  then keeps that worker's id, sessions and workspaces. A new worker is the default,
 *  unless the hub marks one of them `same_machine` (T21): that one is preselected. */
export function Enroll({ app, initialCode }: { app: AppState; initialCode: string }) {
  const [code, setCode] = useState(initialCode);
  const [info, setInfo] = useState<Info>(null);
  const [replace, setReplace] = useState('');
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null);
  const normalized = normalizeCode(code);

  useEffect(() => {
    setInfo(null);
    setReplace('');
    if (!normalized) return;
    let live = true;
    app.api.enrollInfo(normalized).then(
      (i) => live && setInfo({ code: normalized, info: i }),
      (e) =>
        live &&
        setInfo({
          code: normalized,
          error: (e as { status?: number }).status === 404 ? 'Code not found or expired.' : (e as Error).message,
        }),
    );
    return () => {
      live = false;
    };
  }, [app, normalized]);

  const loaded = info && 'info' in info && info.code === normalized ? info.info : null;

  // A worker of the same machine (T21) is the likely answer: preselected, still a choice.
  useEffect(() => {
    const same = loaded?.replaceable.find((w) => w.same_machine);
    if (same) setReplace(same.id);
  }, [loaded]);

  const confirm = async (e: Event) => {
    e.preventDefault();
    if (!normalized || !loaded) return;
    setBusy(true);
    setResult(null);
    try {
      const r = await app.api.enrollConfirm(normalized, replace || undefined);
      setResult({
        ok: true,
        text: r.replaced
          ? `Worker “${r.name}” replaced “${r.replaced}”: the sessions and workspaces stay with it.`
          : `Worker “${r.name}” connected.`,
      });
      setCode('');
    } catch (err) {
      setResult({ ok: false, text: (err as Error).message });
    } finally {
      setBusy(false);
    }
  };

  return (
    <section class="page">
      <header class="bar">
        <a class="back icon-btn" href="#/settings" aria-label="Back" onClick={(e) => onBack(e, '#/settings')}>
          <IconBack />
        </a>
        <div class="bar-title">
          <div class="title">Worker enrollment</div>
        </div>
      </header>
      <form class="page-body" onSubmit={confirm}>
        <p>
          Run <code>hqc-worker enroll &lt;hub address&gt;</code> on the machine and enter the code it
          printed. Only confirm a code you see on your own machine: the worker gets access to your
          account's sessions.
        </p>
        <label class="field">
          <span>Code</span>
          <input
            class="input input-code"
            name="code"
            autoComplete="one-time-code"
            autoCapitalize="characters"
            spellcheck={false}
            placeholder="ABCD-1234"
            value={code}
            onInput={(e) => setCode((e.currentTarget as HTMLInputElement).value)}
          />
        </label>
        {loaded && (
          <dl class="enroll-info">
            <dt>Machine</dt>
            <dd data-field="name">{loaded.name}</dd>
            <dt>OS</dt>
            <dd data-field="os">{loaded.os || '—'}</dd>
            <dt>Code created</dt>
            <dd data-field="created_at">{new Date(loaded.created_at).toLocaleString()}</dd>
            {loaded.country && (
              <>
                <dt>Request country</dt>
                <dd data-field="country">{loaded.country}</dd>
              </>
            )}
          </dl>
        )}
        {info && 'error' in info && info.code === normalized && (
          <div class="error-text">{info.error}</div>
        )}
        {loaded && loaded.replaceable.length > 0 && (
          <fieldset class="enroll-replace">
            <legend>Is this a new machine or a re-enrollment?</legend>
            <label class="choice">
              <input
                type="radio"
                name="replace"
                value=""
                checked={replace === ''}
                onChange={() => setReplace('')}
              />
              <span>New worker</span>
            </label>
            {loaded.replaceable.map((w) => (
              <label class="choice" key={w.id}>
                <input
                  type="radio"
                  name="replace"
                  value={w.id}
                  checked={replace === w.id}
                  onChange={() => setReplace(w.id)}
                />
                <span>
                  The same machine, replace “{w.name}”
                  {w.same_machine && <span class="badge"> same machine</span>}
                  <span class="muted small"> {replaceableNote(w)}</span>
                </span>
              </label>
            ))}
            {replace && (
              <p class="muted small">
                The new worker gets the replaced worker's id, sessions, workspaces and service session; the
                old token stops working.
              </p>
            )}
          </fieldset>
        )}
        {loaded && <p class="muted">Is this your machine, and did you just run enroll on it?</p>}
        <button type="submit" class="btn btn-primary" disabled={busy || !loaded}>
          {replace ? 'Confirm replacement' : 'Confirm'}
        </button>
        {result && <div class={result.ok ? 'outcome ok' : 'error-text'}>{result.text}</div>}
      </form>
    </section>
  );
}
