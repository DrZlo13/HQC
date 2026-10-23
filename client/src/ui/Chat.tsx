import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'preact/hooks';
import type { AppState } from '../app';
import { ChatSession } from '../session';
import { ActivityLine, BusyIndicator, ChatItems, StreamingBubble, onlyRequests, type ChatActions } from './cards';
import { useSubscription } from './hooks';
import { STATE_LABELS } from './labels';

const STOPPABLE = new Set(['running', 'waiting', 'queued']);

export function Chat({ app, sessionId }: { app: AppState; sessionId: string }) {
  const chat = useMemo(() => new ChatSession(app.api, app.socket, sessionId), [app, sessionId]);
  const [notice, setNotice] = useState<string | null>(null);
  const [requestsOnly, setRequestsOnly] = useState(false);
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
    openArtifact: (id) => {
      // Open the window synchronously, or mobile browsers block it as a popup.
      const w = window.open('about:blank', '_blank');
      app.api
        .artifactUrl(id)
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
  const scroller = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  useLayoutEffect(() => {
    const el = scroller.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  });

  return (
    <section class="chat">
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
      {!system && (
        <Composer
          disabled={state === 'archived'}
          onSend={(text) => {
            stick.current = true;
            chat.send(text);
          }}
        />
      )}
    </section>
  );
}

const coarse = typeof matchMedia === 'function' && matchMedia('(pointer: coarse)').matches;

function Composer({ onSend, disabled }: { onSend: (text: string) => void; disabled: boolean }) {
  const [text, setText] = useState('');
  const area = useRef<HTMLTextAreaElement>(null);
  const submit = () => {
    const v = text.trim();
    if (!v) return;
    onSend(v);
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
      <textarea
        ref={area}
        class="input"
        rows={1}
        placeholder={disabled ? 'Сессия в архиве' : 'Сообщение'}
        disabled={disabled}
        value={text}
        onInput={(e) => setText((e.currentTarget as HTMLTextAreaElement).value)}
        onKeyDown={(e) => {
          // On a phone Enter is a newline; on desktop Enter sends, Shift+Enter is a newline.
          if (e.key === 'Enter' && !e.shiftKey && !coarse && !e.isComposing) {
            e.preventDefault();
            submit();
          }
        }}
      />
      <button type="submit" class="btn btn-primary" disabled={disabled || !text.trim()}>
        Отправить
      </button>
    </form>
  );
}
