import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'preact/hooks';
import { ApiError } from '../api';
import type { AppState } from '../app';
import { ArtifactStore, formatSize } from '../artifacts';
import { ChatSession, type OutgoingFile } from '../session';
import { ActivityLine, BusyIndicator, ChatItems, StreamingBubble, onlyRequests, type ChatActions } from './cards';
import { useSubscription } from './hooks';
import { STATE_LABELS } from './labels';

const STOPPABLE = new Set(['running', 'waiting', 'queued']);

export function Chat({ app, sessionId }: { app: AppState; sessionId: string }) {
  const chat = useMemo(() => new ChatSession(app.api, app.socket, sessionId), [app, sessionId]);
  const [notice, setNotice] = useState<string | null>(null);
  const [requestsOnly, setRequestsOnly] = useState(false);
  const artifacts = useMemo(() => new ArtifactStore(app.api), [app]);
  const [files, setFiles] = useState<OutgoingFile[]>([]);
  const [dragging, setDragging] = useState(false);
  const addFiles = (list: FileList | File[] | null | undefined) => {
    const picked = [...(list ?? [])];
    if (!picked.length) return;
    void Promise.all(picked.map(toOutgoing)).then((add) => setFiles((cur) => [...cur, ...add]));
  };
  useEffect(() => {
    void chat.open();
    return () => chat.close();
  }, [chat]);
  useSubscription((fn) => chat.timeline.onChange(fn), [chat]);
  useSubscription((fn) => app.onChange(fn), [app]);

  const info = app.sessions.get(sessionId);
  const state = info?.state ?? 'unknown';
  // The worker's service session (SPEC §10.2): requests and log lines, no input.
  const system = info?.system ?? false;
  const t = chat.timeline;

  // Read cursor: whatever is on screen while the tab is visible counts as read.
  useEffect(() => {
    const mark = () => {
      if (document.visibilityState !== 'visible') return;
      chat.markRead();
      app.markRead(sessionId);
    };
    mark();
    document.addEventListener('visibilitychange', mark);
    return () => document.removeEventListener('visibilitychange', mark);
  }, [chat, t.lastSeq]);

  const actions: ChatActions = {
    timeline: t,
    approve: (r, o) => chat.approve(r, o),
    answer: (r, a) => chat.answer(r, a),
    resend: (op) => chat.resend(op),
    discard: (op) => chat.discard(op),
    artifacts,
    openArtifact: (id, download) => {
      // Open the window synchronously, or mobile browsers block it as a popup.
      const w = window.open('about:blank', '_blank');
      (download ? artifacts.downloadUrl(id) : artifacts.url(id))
        .then((url) => (w ? (w.location.href = url) : window.open(url, '_blank')))
        .catch((e: Error) => {
          w?.close();
          setNotice(`Вложение недоступно: ${e.message}`);
        });
    },
  };

  const act = (fn: () => Promise<void>, failed: string) =>
    fn().catch((e: Error) => setNotice(`${failed}: ${e.message}`));

  const showRetry = !system && (state === 'error' || t.hasFailedDelivery());
  const items = requestsOnly ? onlyRequests(t.items()) : t.items();
  const locked = state === 'locked_vscode';
  // SPEC §9.3: read-only while VS Code drives it; the hub refuses takeover until VS Code has
  // been quiet for 30 s (`409 vscode_active`).
  const takeover = () =>
    app.api.takeover(sessionId).then(
      () => {
        setNotice(null);
        return app.refreshSessions();
      },
      (e: unknown) =>
        setNotice(
          e instanceof ApiError && e.code === 'vscode_active'
            ? 'В VS Code ещё пишут в эту сессию; подхватить можно через 30 с тишины.'
            : `Не подхвачено: ${(e as Error).message}`,
        ),
    );
  const scroller = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  useLayoutEffect(() => {
    const el = scroller.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  });

  const disabled = state === 'archived' || locked;
  return (
    <section
      class={dragging ? 'chat chat-dragging' : 'chat'}
      onDragOver={(e) => {
        if (disabled || !e.dataTransfer?.types.includes('Files')) return;
        e.preventDefault();
        setDragging(true);
      }}
      onDragLeave={(e) => {
        if (e.currentTarget === e.target) setDragging(false);
      }}
      onDrop={(e) => {
        setDragging(false);
        if (disabled || !e.dataTransfer?.files.length) return;
        e.preventDefault();
        addFiles(e.dataTransfer.files);
      }}
    >
      <header class="bar">
        <a class="back" href="#/" aria-label="К списку">
          ‹
        </a>
        <div class="bar-title">
          <div class="title">{info?.title || 'Сессия'}</div>
          {system && <div class="muted small">служебная сессия воркера</div>}
          <div class={`state state-${state}`}>
            <BusyIndicator state={state} />
            {STATE_LABELS[state]}
            {info && info.queue_len > 0 && ` · в очереди ${info.queue_len}`}
            {app.conn !== 'open' && ' · нет связи'}
          </div>
        </div>
        {system && (
          <button
            type="button"
            class={`btn btn-small btn-filter ${requestsOnly ? 'is-on' : ''}`}
            aria-pressed={requestsOnly}
            onClick={() => setRequestsOnly(!requestsOnly)}
          >
            Только запросы
          </button>
        )}
        {!system && STOPPABLE.has(state) && (
          <button type="button" class="btn btn-deny btn-small" onClick={() => act(() => chat.stop(), 'Не остановлено')}>
            Стоп
          </button>
        )}
        {showRetry && (
          <button type="button" class="btn btn-small" onClick={() => act(() => chat.retry(), 'Повтор не удался')}>
            Повторить
          </button>
        )}
      </header>
      <div
        class="timeline"
        ref={scroller}
        onScroll={(e) => {
          const el = e.currentTarget as HTMLDivElement;
          stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
        }}
      >
        <ChatItems items={items} actions={actions} />
        {requestsOnly && items.length === 0 && <div class="empty">Запросов нет</div>}
        {t.streaming && <StreamingBubble text={t.streaming} />}
        {!system && <ActivityLine state={state} timeline={t} />}
      </div>
      {notice && (
        <div class="notice" role="alert" onClick={() => setNotice(null)}>
          {notice}
        </div>
      )}
      {locked && (
        <div class="locked-banner">
          <span>Сессия ведётся в VS Code. Чтобы писать отсюда, подхватите её.</span>
          <button type="button" class="btn btn-primary btn-small btn-takeover" onClick={() => void takeover()}>
            Подхватить
          </button>
        </div>
      )}
      {!system && (
        <Composer
          disabled={disabled}
          placeholder={locked ? 'Ведётся в VS Code' : undefined}
          files={files}
          onFiles={addFiles}
          onRemove={(i) => setFiles((cur) => cur.filter((_, j) => j !== i))}
          onSend={(text) => {
            stick.current = true;
            chat.send(text, files);
            setFiles([]);
          }}
        />
      )}
    </section>
  );
}

const coarse = typeof matchMedia === 'function' && matchMedia('(pointer: coarse)').matches;

/** Images up to this size get a preview before sending. */
const PREVIEW_MAX = 10 * 1024 * 1024;

/** A picked file with a `data:` preview for images (the CSP has no `blob:`). */
export function toOutgoing(file: File): Promise<OutgoingFile> {
  const f: OutgoingFile = { file, name: file.name || 'file' };
  if (!file.type.startsWith('image/') || file.size > PREVIEW_MAX || typeof FileReader === 'undefined') {
    return Promise.resolve(f);
  }
  return new Promise((resolve) => {
    const r = new FileReader();
    r.onload = () => resolve({ ...f, preview: typeof r.result === 'string' ? r.result : undefined });
    r.onerror = () => resolve(f);
    r.readAsDataURL(file);
  });
}

export function Composer({
  onSend,
  disabled,
  placeholder,
  files = [],
  onFiles = () => {},
  onRemove = () => {},
}: {
  onSend: (text: string) => void;
  disabled: boolean;
  placeholder?: string;
  files?: OutgoingFile[];
  onFiles?: (files: FileList | File[]) => void;
  onRemove?: (index: number) => void;
}) {
  const [text, setText] = useState('');
  const area = useRef<HTMLTextAreaElement>(null);
  const picker = useRef<HTMLInputElement>(null);
  const canSend = !disabled && (text.trim() !== '' || files.length > 0);
  const submit = () => {
    if (!canSend) return;
    onSend(text.trim());
    setText('');
    area.current?.focus();
  };
  useLayoutEffect(() => {
    const el = area.current;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = `${Math.min(el.scrollHeight, 200)}px`;
  }, [text]);
  return (
    <form
      class="composer"
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      {files.length > 0 && (
        <div class="composer-files">
          {files.map((f, i) => (
            <span key={i} class="composer-file">
              {f.preview ? (
                <img class="attachment-thumb" src={f.preview} alt={f.name} title={f.name} />
              ) : (
                <span class="attachment-chip">
                  {f.name} <span class="muted">{formatSize(f.file.size)}</span>
                </span>
              )}
              <button type="button" class="composer-file-remove" aria-label={`Убрать ${f.name}`} onClick={() => onRemove(i)}>
                ×
              </button>
            </span>
          ))}
        </div>
      )}
      <input
        ref={picker}
        type="file"
        multiple
        hidden
        onChange={(e) => {
          const el = e.currentTarget as HTMLInputElement;
          if (el.files) onFiles(el.files);
          el.value = '';
        }}
      />
      <button
        type="button"
        class="btn btn-attach"
        aria-label="Приложить файлы"
        title="Приложить файлы"
        disabled={disabled}
        onClick={() => picker.current?.click()}
      >
        <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M21 11.5l-8.6 8.6a5.5 5.5 0 0 1-7.8-7.8l8.6-8.6a3.7 3.7 0 0 1 5.2 5.2l-8.6 8.6a1.8 1.8 0 0 1-2.6-2.6l7.9-7.9" />
        </svg>
      </button>
      <textarea
        ref={area}
        class="input"
        rows={1}
        placeholder={placeholder ?? (disabled ? 'Сессия в архиве' : 'Сообщение')}
        disabled={disabled}
        value={text}
        onInput={(e) => setText((e.currentTarget as HTMLTextAreaElement).value)}
        onPaste={(e) => {
          const pasted = [...(e.clipboardData?.files ?? [])];
          if (pasted.length === 0) return;
          e.preventDefault();
          onFiles(pasted);
        }}
        onKeyDown={(e) => {
          // On a phone Enter is a newline; on desktop Enter sends, Shift+Enter is a newline.
          if (e.key === 'Enter' && !e.shiftKey && !coarse && !e.isComposing) {
            e.preventDefault();
            submit();
          }
        }}
      />
      <button type="submit" class="btn btn-primary" disabled={!canSend}>
        Отправить
      </button>
    </form>
  );
}
