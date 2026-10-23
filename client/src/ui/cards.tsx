// Rendering of chat items: one component per event type of SPEC §4.
// Unknown and invalid events render as a collapsed card and never throw.

import { useState } from 'preact/hooks';
import type { ClarifyAnswer, ClarifyQuestion, EventOf, HqcEvent, SessionState } from '../proto';
import type { InvalidEvent, Item, PendingMessage, Timeline } from '../timeline';
import {
  CANCEL_LABELS,
  POLICY_HINT_LABELS,
  activityLabel,
  STATE_LABELS,
  SYSTEM_CODE_LABELS,
  formatDuration,
  formatTime,
  isDeny,
  oneLine,
  optionLabel,
} from './labels';
import { CopyButton, Markdown } from './Markdown';
import type { ArtifactStore } from '../artifacts';
import { AttachmentView, PendingAttachments } from './attachments';

/** What cards need from the chat: lookups and actions. */
export interface ChatActions {
  timeline: Timeline;
  approve(requestId: string, optionId: string): Promise<void>;
  answer(requestId: string, answers: Record<string, ClarifyAnswer>): Promise<void>;
  resend(clientOpId: string): void;
  discard(clientOpId: string): void;
  /** Opens the artifact in a new tab; `download`: as an attachment. */
  openArtifact(id: string, download?: boolean): void;
  /** Metadata and signed links of artifacts; without it attachments are plain cards. */
  artifacts?: ArtifactStore;
  /** Wall clock, injectable for tests. */
  now?: () => number;
}

export function ChatItems({ items, actions }: { items: Item[]; actions: ChatActions }) {
  return (
    <>
      {items.map((it) => {
        switch (it.kind) {
          case 'event':
            return <EventView key={it.key} event={it.event} actions={actions} />;
          case 'invalid':
            return <InvalidCard key={it.key} invalid={it.invalid} />;
          case 'pending':
            return <PendingBubble key={it.key} pending={it.pending} actions={actions} />;
        }
      })}
    </>
  );
}

/** A turn is on: shown in the session header next to the state. */
export function BusyIndicator({ state }: { state: SessionState }) {
  if (state !== 'running') return null;
  return <span class="busy" role="img" aria-label="ход идёт" />;
}

/** The last line of the chat while a turn is on or input waits (SPEC §17.1). */
export function ActivityLine({ state, timeline }: { state: SessionState; timeline: Timeline }) {
  const label = activityLabel(state, state === 'running' ? timeline.runningTool() : undefined);
  if (!label) return null;
  return (
    <div class={`activity activity-${state}`} role="status" aria-live="polite">
      {state === 'running' && <span class="busy" aria-hidden="true" />}
      {label}
    </div>
  );
}

export function EventView({ event, actions }: { event: HqcEvent; actions: ChatActions }) {
  switch (event.type) {
    case 'user.message':
      return <UserBubble event={event} actions={actions} />;
    case 'agent.delta':
      return <StreamingBubble text={event.payload.text} />;
    case 'agent.message':
      return <AgentBubble event={event} />;
    case 'agent.thinking':
      return (
        <details class="card card-thinking">
          <summary>Размышления · {oneLine(event.payload.text, 60)}</summary>
          <pre class="pre">{event.payload.text}</pre>
        </details>
      );
    case 'tool.call':
      return <ToolCard call={event} result={actions.timeline.resultFor(event.payload.call_id)} actions={actions} />;
    case 'tool.result':
      // Shown inside its call's card; a result without a call gets its own card.
      return actions.timeline.hasCall(event.payload.call_id) ? null : <ToolCard result={event} actions={actions} />;
    case 'approval.request':
      return <ApprovalCard event={event} actions={actions} />;
    case 'approval.response': {
      const req = actions.timeline.request(event.payload.request_id);
      const opt =
        req?.type === 'approval.request'
          ? req.payload.options.find((o) => o.id === event.payload.option_id)
          : undefined;
      const by = event.payload.by === 'auto' ? ' (автоматически)' : '';
      return (
        <Line kind="approval">
          {req?.type === 'approval.request' ? `${req.payload.tool}: ` : ''}
          {opt ? optionLabel(opt) : event.payload.option_id}
          {by}
        </Line>
      );
    }
    case 'approval.cancel':
    case 'clarify.cancel':
      return (
        <Line kind="cancel">
          {event.type === 'approval.cancel' ? 'Запрос разрешения' : 'Вопрос'} отменён:{' '}
          {CANCEL_LABELS[event.payload.reason]}
        </Line>
      );
    case 'clarify.request':
      return <ClarifyCard event={event} actions={actions} />;
    case 'clarify.response':
      return <Line kind="approval">Ответы отправлены</Line>;
    case 'attachment':
      return (
        <AttachmentView
          id={event.payload.artifact_id}
          kind={event.payload.kind}
          name={event.payload.name}
          caption={event.payload.caption}
          actions={actions}
        />
      );
    case 'peer.message':
      return (
        <div class="card card-peer">
          <div class="card-title">
            {event.payload.mode === 'ask' ? 'Вопрос' : 'Сообщение'} от сессии{' '}
            <code>{event.payload.from_session_id.slice(-8)}</code>
          </div>
          <Markdown class="text" text={event.payload.text} />
          <div class="meta">
            <CopyButton text={event.payload.text} />
          </div>
        </div>
      );
    case 'usage': {
      const p = event.payload;
      return (
        <Line kind="usage">
          {p.input_tokens} → {p.output_tokens} токенов
          {p.cache_read ? ` · кэш ${p.cache_read}` : ''}
          {p.cost_usd !== undefined ? ` · $${p.cost_usd.toFixed(4)}` : ''}
          {p.model ? ` · ${p.model}` : ''}
        </Line>
      );
    }
    case 'status':
      // The state is in the header and the activity line; only an error with a reason
      // is worth a line of its own.
      if (event.payload.state !== 'error' || !event.payload.detail) return null;
      return (
        <Line kind="status state-error">
          {STATE_LABELS.error} · {event.payload.detail}
        </Line>
      );
    case 'system': {
      const caption = SYSTEM_CODE_LABELS[event.payload.code];
      return (
        <Line kind={caption ? `system line-system-${event.payload.code}` : 'system'}>
          {caption && <b>{caption}</b>}
          <Markdown text={event.payload.text} />
        </Line>
      );
    }
    case 'unknown':
      return (
        <details class="card card-unknown">
          <summary>Неизвестное событие · {event.wire_type}</summary>
          <pre class="pre">{JSON.stringify(event.payload, null, 2)}</pre>
        </details>
      );
  }
}

function Line({ kind, children }: { kind: string; children: preact.ComponentChildren }) {
  return <div class={`line line-${kind}`}>{children}</div>;
}

function UserBubble({ event, actions }: { event: EventOf<'user.message'>; actions: ChatActions }) {
  const d = event.delivery;
  const ids = event.payload.artifact_ids;
  return (
    <div class="bubble bubble-user">
      {event.payload.text && <Markdown class="text" text={event.payload.text} />}
      {ids.length > 0 && (
        <div class="attachments">
          {ids.map((id) => (
            <AttachmentView key={id} id={id} actions={actions} />
          ))}
        </div>
      )}
      <div class="meta">
        {event.payload.text && <CopyButton text={event.payload.text} />}
        {formatTime(event.ts)}
        {d === 'pending' && <span class="delivery"> · ждёт доставки</span>}
        {d === 'failed' && <span class="delivery delivery-failed"> · не доставлено</span>}
      </div>
    </div>
  );
}

function PendingBubble({ pending, actions }: { pending: PendingMessage; actions: ChatActions }) {
  return (
    <div class={`bubble bubble-user bubble-pending ${pending.status === 'failed' ? 'bubble-failed' : ''}`}>
      {pending.text && <Markdown class="text" text={pending.text} />}
      {pending.attachments && <PendingAttachments files={pending.attachments} />}
      <div class="meta">
        {pending.status === 'sending' ? (
          'отправка…'
        ) : (
          <>
            не отправлено: {pending.error}{' '}
            <button type="button" class="btn btn-small" onClick={() => actions.resend(pending.client_op_id)}>
              Ещё раз
            </button>{' '}
            <button type="button" class="btn btn-small btn-ghost" onClick={() => actions.discard(pending.client_op_id)}>
              Убрать
            </button>
          </>
        )}
      </div>
    </div>
  );
}

function AgentBubble({ event }: { event: EventOf<'agent.message'> }) {
  return (
    <div class={`bubble bubble-agent ${event.payload.final ? 'bubble-final' : 'bubble-interim'}`}>
      <Markdown class="text" text={event.payload.text} />
      <div class="meta">
        <CopyButton text={event.payload.text} />
        {formatTime(event.ts)}
      </div>
    </div>
  );
}

export function StreamingBubble({ text }: { text: string }) {
  return (
    <div class="bubble bubble-agent bubble-streaming" aria-live="polite">
      {/* The cursor is drawn by CSS after the last block. */}
      <Markdown class="text" text={text} />
    </div>
  );
}

function ToolCard({
  call,
  result,
  actions,
}: {
  call?: EventOf<'tool.call'>;
  result?: EventOf<'tool.result'>;
  actions: ChatActions;
}) {
  const r = result?.payload;
  const state = !r ? 'running' : r.error ? 'error' : 'ok';
  const mark = { running: '…', error: '✕', ok: '✓' }[state];
  const name = call?.payload.tool ?? 'Результат инструмента';
  const preview = call ? oneLine(call.payload.args_preview, 60) : oneLine(r?.result_preview ?? '', 60);
  return (
    <details class={`card card-tool tool-${state}`}>
      <summary>
        <span class="tool-mark" aria-label={state}>
          {mark}
        </span>{' '}
        <b>{name}</b> <span class="muted">{preview}</span>
        {r?.duration_ms !== undefined && <span class="muted"> · {formatDuration(r.duration_ms)}</span>}
      </summary>
      {call && (
        <>
          <div class="label">Аргументы</div>
          <pre class="pre">{call.payload.args_preview}</pre>
        </>
      )}
      {r && (
        <>
          <div class="label">{r.error ? 'Ошибка' : 'Результат'}</div>
          {r.error_text && <div class="error-text">{r.error_text}</div>}
          {r.result_preview && <pre class="pre">{r.result_preview}</pre>}
          {r.artifact_id && (
            <button type="button" class="btn btn-small" onClick={() => actions.openArtifact(r.artifact_id!)}>
              Полный результат
            </button>
          )}
        </>
      )}
    </details>
  );
}

function useAction() {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const run = (fn: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    fn()
      .catch((e: Error) => setError(e.message))
      .finally(() => setBusy(false));
  };
  return { busy, error, run };
}

export function ApprovalCard({ event, actions }: { event: EventOf<'approval.request'>; actions: ChatActions }) {
  const p = event.payload;
  const fate = actions.timeline.requestFate(p.request_id);
  const { busy, error, run } = useAction();
  const now = actions.now?.() ?? Date.now();
  const expired = p.expires_at !== undefined && Date.parse(p.expires_at) <= now;
  const chosen =
    fate.response && 'option_id' in fate.response
      ? p.options.find((o) => o.id === (fate.response as { option_id: string }).option_id)
      : undefined;
  const open = !fate.response && !fate.cancel;
  return (
    <div class={`card card-approval ${open ? 'is-open' : 'is-closed'}`}>
      <div class="card-title">
        Разрешение · <b>{p.tool}</b>
        {p.policy_hint && <span class={`policy-hint policy-${p.policy_hint}`}> · {POLICY_HINT_LABELS[p.policy_hint]}</span>}
      </div>
      <pre class="pre">{p.description}</pre>
      {open && !expired && (
        <div class="actions">
          {p.options.map((o) => (
            <button
              type="button"
              key={o.id}
              class={`btn ${isDeny(o) ? 'btn-deny' : o.canonical === 'once' ? 'btn-primary' : ''}`}
              data-option={o.id}
              data-canonical={o.canonical}
              title={o.label}
              disabled={busy}
              onClick={() => run(() => actions.approve(p.request_id, o.id))}
            >
              {optionLabel(o)}
            </button>
          ))}
        </div>
      )}
      {open && expired && <div class="muted">Срок ответа истёк</div>}
      {open && !expired && p.expires_at && <div class="muted small">до {formatTime(p.expires_at)}</div>}
      {fate.response && (
        <div class="outcome">Ответ: {chosen ? optionLabel(chosen) : (fate.response as { option_id?: string }).option_id}</div>
      )}
      {fate.cancel && <div class="outcome">Отменён: {CANCEL_LABELS[fate.cancel.reason]}</div>}
      {error && <div class="error-text">{error}</div>}
    </div>
  );
}

export function ClarifyCard({ event, actions }: { event: EventOf<'clarify.request'>; actions: ChatActions }) {
  const p = event.payload;
  const fate = actions.timeline.requestFate(p.request_id);
  const open = !fate.response && !fate.cancel;
  const [answers, setAnswers] = useState<Record<string, ClarifyAnswer>>({});
  const { busy, error, run } = useAction();
  const given = fate.response && 'answers' in fate.response ? fate.response.answers : undefined;

  const set = (q: ClarifyQuestion, v: string, checked: boolean) => {
    setAnswers((a) => {
      if (!q.multi) return { ...a, [q.id]: v };
      const cur = Array.isArray(a[q.id]) ? (a[q.id] as string[]) : [];
      return { ...a, [q.id]: checked ? [...cur, v] : cur.filter((x) => x !== v) };
    });
  };

  return (
    <form
      class={`card card-clarify ${open ? 'is-open' : 'is-closed'}`}
      onSubmit={(e) => {
        e.preventDefault();
        if (open) run(() => actions.answer(p.request_id, answers));
      }}
    >
      <div class="card-title">Вопрос агента</div>
      {p.questions.map((q) => (
        <fieldset key={q.id} class="question" disabled={!open || busy}>
          <legend>{q.text}</legend>
          {q.options?.map((o) => {
            const cur = (given ?? answers)[q.id];
            const input = {
              name: `${p.request_id}-${q.id}`,
              value: o,
              checked: Array.isArray(cur) ? cur.includes(o) : cur === o,
              onChange: (e: Event) => set(q, o, (e.currentTarget as HTMLInputElement).checked),
            };
            return (
              <label key={o} class="choice">
                {q.multi ? <input type="checkbox" {...input} /> : <input type="radio" {...input} />} {o}
              </label>
            );
          })}
          {!q.options?.length && (
            <input
              class="input"
              type="text"
              value={String((given ?? answers)[q.id] ?? '')}
              onInput={(e) => set(q, (e.currentTarget as HTMLInputElement).value, true)}
            />
          )}
        </fieldset>
      ))}
      {open && (
        <div class="actions">
          <button type="submit" class="btn btn-primary" disabled={busy}>
            Ответить
          </button>
        </div>
      )}
      {fate.cancel && <div class="outcome">Отменён: {CANCEL_LABELS[fate.cancel.reason]}</div>}
      {error && <div class="error-text">{error}</div>}
    </form>
  );
}

function InvalidCard({ invalid }: { invalid: InvalidEvent }) {
  const type = (invalid.raw as { type?: unknown })?.type;
  return (
    <details class="card card-unknown card-invalid">
      <summary>Некорректное событие · {typeof type === 'string' ? type : '?'}</summary>
      <div class="error-text">{invalid.error}</div>
      <pre class="pre">{JSON.stringify(invalid.raw, null, 2)}</pre>
    </details>
  );
}
