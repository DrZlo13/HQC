import { useState } from 'preact/hooks';
import type { AppState } from '../app';

/** Confirms the code `hqc-worker enroll <hub>` printed (SPEC §5.1). */
export function Enroll({ app, initialCode }: { app: AppState; initialCode: string }) {
  const [code, setCode] = useState(initialCode);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null);

  const confirm = async (e: Event) => {
    e.preventDefault();
    const c = code.trim().toUpperCase();
    if (!c) return;
    setBusy(true);
    setResult(null);
    try {
      const r = (await app.api.enrollConfirm(c)) as { name?: string } | undefined;
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
        <button type="submit" class="btn btn-primary" disabled={busy || !code.trim()}>
          Подтвердить
        </button>
        {result && <div class={result.ok ? 'outcome ok' : 'error-text'}>{result.text}</div>}
      </form>
    </section>
  );
}
