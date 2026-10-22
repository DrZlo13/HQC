import { useEffect, useState } from 'preact/hooks';
import type { AppState } from '../app';
import type { EnrollInfo } from '../proto';

/** `ab cd-1234` -> `ABCD-1234` once all 8 characters are there, else `null`. */
export function normalizeCode(s: string): string | null {
  const c = s.replace(/[^0-9a-z]/gi, '').toUpperCase();
  return c.length === 8 ? `${c.slice(0, 4)}-${c.slice(4)}` : null;
}

type Info = { code: string; info: EnrollInfo } | { code: string; error: string } | null;

/** Confirms the code `hqc-worker enroll <hub>` printed (SPEC §5.1). Before the button is
 *  enabled the screen shows what the worker said about itself (name, OS, when, country):
 *  a link with a code may come from someone else's machine. */
export function Enroll({ app, initialCode }: { app: AppState; initialCode: string }) {
  const [code, setCode] = useState(initialCode);
  const [info, setInfo] = useState<Info>(null);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null);
  const normalized = normalizeCode(code);

  useEffect(() => {
    setInfo(null);
    if (!normalized) return;
    let live = true;
    app.api.enrollInfo(normalized).then(
      (i) => live && setInfo({ code: normalized, info: i }),
      (e) =>
        live &&
        setInfo({
          code: normalized,
          error: (e as { status?: number }).status === 404 ? 'Код не найден или истёк.' : (e as Error).message,
        }),
    );
    return () => {
      live = false;
    };
  }, [app, normalized]);

  const loaded = info && 'info' in info && info.code === normalized ? info.info : null;

  const confirm = async (e: Event) => {
    e.preventDefault();
    if (!normalized || !loaded) return;
    setBusy(true);
    setResult(null);
    try {
      const r = (await app.api.enrollConfirm(normalized)) as { name?: string } | undefined;
      setResult({ ok: true, text: `Воркер${r?.name ? ` «${r.name}»` : ''} подключён.` });
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
        <a class="back" href="#/settings" aria-label="Назад">
          ‹
        </a>
        <div class="bar-title">
          <div class="title">Регистрация воркера</div>
        </div>
      </header>
      <form class="page-body" onSubmit={confirm}>
        <p>
          Запустите на машине <code>hqc-worker enroll &lt;адрес хаба&gt;</code> и введите код, который
          он напечатал. Подтверждайте только код, который видите на своей машине: воркер получит
          доступ к сессиям вашего аккаунта.
        </p>
        <label class="field">
          <span>Код</span>
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
            <dt>Машина</dt>
            <dd data-field="name">{loaded.name}</dd>
            <dt>ОС</dt>
            <dd data-field="os">{loaded.os || '—'}</dd>
            <dt>Код создан</dt>
            <dd data-field="created_at">{new Date(loaded.created_at).toLocaleString()}</dd>
            {loaded.country && (
              <>
                <dt>Страна запроса</dt>
                <dd data-field="country">{loaded.country}</dd>
              </>
            )}
          </dl>
        )}
        {info && 'error' in info && info.code === normalized && (
          <div class="error-text">{info.error}</div>
        )}
        {loaded && <p class="muted">Это ваша машина и вы только что запустили на ней enroll?</p>}
        <button type="submit" class="btn btn-primary" disabled={busy || !loaded}>
          Подтвердить
        </button>
        {result && <div class={result.ok ? 'outcome ok' : 'error-text'}>{result.text}</div>}
      </form>
    </section>
  );
}
