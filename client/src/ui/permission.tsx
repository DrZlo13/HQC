// The permission request (T26, SPEC §10.1): one card instead of a tool row and a panel —
// what the agent wants to run, how long it waits, why Claude Code asked, buttons by
// `options[]`, Enter and Esc while the card is on screen, and a text answer instead of the
// call. Answered or cancelled, it folds into a row «$ ls / · разрешено». On a phone the open
// request is a bottom sheet in place of the composer.

import { useEffect, useRef, useState } from 'preact/hooks';
import type { ApprovalOption, EventOf } from '../proto';
import type { ChatActions } from './cards';
import { callArgs, callLine, callPaths, outsidePath } from './feed';
import { useAction, useNow } from './hooks';
import { IconCheck, IconCross, IconInfo, IconShield } from './icons';
import {
  CANCEL_LABELS,
  CANONICAL_OUTCOMES,
  WORKER_REQUEST_LABELS,
  formatTimer,
  homePath,
  isDeny,
  optionLabel,
} from './labels';

const TITLES: Record<string, string> = {
  Bash: 'Агент хочет выполнить команду',
  Edit: 'Агент хочет изменить файл',
  MultiEdit: 'Агент хочет изменить файл',
  Write: 'Агент хочет записать файл',
  NotebookEdit: 'Агент хочет изменить блокнот',
  Read: 'Агент хочет прочитать файл',
  WebFetch: 'Агент хочет открыть страницу',
  WebSearch: 'Агент хочет поискать в интернете',
};

/** What a request is about: the command or path, the CLI's description, the paths touched. */
export function requestView(event: EventOf<'approval.request'>, call?: EventOf<'tool.call'>) {
  const p = event.payload;
  // A worker's own request (SPEC §10.2): its description is the whole story.
  const worker = event.author.kind === 'system' || p.tool in WORKER_REQUEST_LABELS;
  const lines = p.description
    .split('\n')
    .map((s) => s.trim())
    .filter(Boolean);
  let what = call ? callLine(p.tool, call.payload.args_preview).text : '';
  if (!what && !worker && lines.length > 1) what = lines[lines.length - 1];
  if (!what && !worker && p.tool === 'Bash') what = lines[0] ?? '';
  const description = worker ? '' : lines.filter((l) => l !== what).join(' ');
  const args = call
    ? callArgs(call.payload.args_preview)
    : p.tool === 'Bash'
      ? { command: what }
      : { file_path: what };
  return {
    worker,
    shell: p.tool === 'Bash' && !!what,
    what: worker ? p.description : what,
    description,
    paths: worker ? [] : callPaths(p.tool, args),
  };
}

/** Why Claude Code asked although a rule allows it, and a path outside the workspace. */
function noteText(event: EventOf<'approval.request'>, outside: string | undefined, root: string | undefined) {
  const p = event.payload;
  const where = (cap: string) =>
    outside && (
      <>
        {cap} <code>{outside}</code> вне рабочей папки <code>{homePath(root!)}</code>
      </>
    );
  if (p.policy_hint === 'allow') {
    return (
      <>
        Правило воркера разрешает {p.tool}, но Claude Code спросил сам{outside ? <>: {where('путь')}.</> : '.'}
      </>
    );
  }
  if (p.policy_hint === 'deny') return <>Правило воркера запрещает {p.tool}.</>;
  if (p.policy_hint === 'unknown') return <>На этот вызов есть правило в политике воркера.</>;
  if (outside) return <>{where('Путь')}.</>;
  return null;
}

/** An answered or cancelled request: what became of it. */
export function outcomeText(event: EventOf<'approval.request'>, actions: ChatActions): { ok: boolean; text: string } | null {
  const fate = actions.timeline.requestFate(event.payload.request_id);
  if (fate.cancel) return { ok: false, text: `отменён: ${CANCEL_LABELS[fate.cancel.reason]}` };
  const r = fate.response;
  if (!r || !('option_id' in r)) return null;
  const opt = event.payload.options.find((o) => o.id === r.option_id);
  const deny = opt ? isDeny(opt) : false;
  let text = opt ? (opt.canonical === 'unknown' ? opt.label : CANONICAL_OUTCOMES[opt.canonical]) : r.option_id;
  if (r.message) text = 'отклонено, ответ текстом';
  else if (r.by === 'auto') text = `${text} автоматически`;
  return { ok: !deny, text };
}

export function PermissionBlock({
  event,
  call,
  actions,
  sheet,
}: {
  event: EventOf<'approval.request'>;
  call?: EventOf<'tool.call'>;
  actions: ChatActions;
  sheet?: boolean;
}) {
  const [again, setAgain] = useState(false);
  const outcome = outcomeText(event, actions);
  if (!outcome) return <PermissionCard event={event} call={call} actions={actions} sheet={sheet} />;
  const v = requestView(event, call);
  return (
    <div class="perm-done">
      <div class="perm-done-row">
        <span class={`mark ${outcome.ok ? 'mark-ok' : 'mark-no'}`} aria-hidden="true">
          {outcome.ok ? <IconCheck /> : <IconCross />}
        </span>
        <span class="perm-done-what mono">
          {v.worker ? WORKER_REQUEST_LABELS[event.payload.tool] ?? event.payload.tool : `${v.shell ? '$ ' : ''}${v.what || event.payload.tool}`}
        </span>
        <span class="perm-done-outcome outcome">· {outcome.text}</span>
        <span class="spacer" />
        <button type="button" class="btn-link" aria-expanded={again} onClick={() => setAgain(!again)}>
          {again ? 'Скрыть запрос' : 'Показать запрос снова'}
        </button>
      </div>
      {again && <PermissionCard event={event} call={call} actions={actions} />}
    </div>
  );
}

function isTyping(el: Element | null): boolean {
  if (!el) return false;
  const tag = el.tagName;
  return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || (el as HTMLElement).isContentEditable;
}

/** On screen: intersects the viewport. Without layout (no size at all) it counts as shown. */
function onScreen(el: HTMLElement): boolean {
  const r = el.getBoundingClientRect();
  if (r.width === 0 && r.height === 0) return true;
  const h = el.ownerDocument.defaultView?.innerHeight ?? 0;
  return r.bottom > 0 && r.top < h;
}

function PermissionCard({
  event,
  call,
  actions,
  sheet,
}: {
  event: EventOf<'approval.request'>;
  call?: EventOf<'tool.call'>;
  actions: ChatActions;
  sheet?: boolean;
}) {
  const p = event.payload;
  const fate = actions.timeline.requestFate(p.request_id);
  const open = !fate.response && !fate.cancel;
  const { busy, error, run } = useAction();
  const now = useNow(1000, open, actions.now);
  const expired = p.expires_at !== undefined && Date.parse(p.expires_at) <= now;
  const live = open && !expired;
  const [replying, setReplying] = useState(false);
  const [reply, setReply] = useState('');
  const card = useRef<HTMLElement>(null);
  const field = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    if (replying) field.current?.focus();
  }, [replying]);
  const v = requestView(event, call);
  const outside = outsidePath(v.paths, actions.workspaceRoot);
  const note = v.worker ? null : noteText(event, outside, actions.workspaceRoot);
  const once = p.options.find((o) => o.canonical === 'once');
  const deny = p.options.find((o) => o.canonical === 'deny') ?? p.options.find((o) => o.canonical === 'deny_always');
  const answer = (o: ApprovalOption, message?: string) =>
    run(() => (message === undefined ? actions.approve(p.request_id, o.id) : actions.approve(p.request_id, o.id, message)));

  // Enter allows once, Esc denies: only for the request the chat gives the keys to, while
  // its card is on screen and the focus is not in a field.
  const hotkeys = live && !busy && actions.hotkeyRequest === p.request_id;
  useEffect(() => {
    if (!hotkeys) return;
    const doc = card.current?.ownerDocument;
    if (!doc) return;
    // A request arrived while the focus sits in an empty field (the composer after sending):
    // let the keys reach the card. A field with text keeps the focus, so typing is not lost.
    const active = doc.activeElement as HTMLInputElement | HTMLTextAreaElement | null;
    if (isTyping(active) && !card.current?.contains(active) && !(active?.value ?? '').trim()) {
      (active as HTMLElement).blur();
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.isComposing || e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
      if (isTyping(doc.activeElement) || !card.current || !onScreen(card.current)) return;
      const o = e.key === 'Enter' ? once : e.key === 'Escape' ? deny : undefined;
      if (!o) return;
      e.preventDefault();
      answer(o);
    };
    doc.addEventListener('keydown', onKey);
    return () => doc.removeEventListener('keydown', onKey);
  }, [hotkeys, p.request_id]);

  const title = v.worker
    ? 'Воркер спрашивает разрешения'
    : sheet && p.tool === 'Bash'
      ? 'Выполнить команду?'
      : TITLES[p.tool] ?? `Агент хочет использовать ${p.tool}`;
  const subtitle = v.worker
    ? WORKER_REQUEST_LABELS[p.tool] ?? p.tool
    : [p.tool, v.description].filter(Boolean).join(' · ');
  const outcome = outcomeText(event, actions);
  const others = p.options.filter((o) => o !== once && o !== deny);
  const sendReply = () => {
    if (deny && reply.trim()) answer(deny, reply.trim());
  };

  return (
    <section
      ref={card}
      class={`perm ${sheet ? 'perm-sheet' : 'perm-card'} ${live ? 'is-open' : 'is-closed'}`}
      aria-label="Запрос разрешения"
      data-request={p.request_id}
    >
      <div class="perm-head">
        <span class="perm-icon">
          <IconShield />
        </span>
        <div class="perm-titles">
          <div class="perm-title">{title}</div>
          <div class="perm-sub">{subtitle}</div>
        </div>
        {live && (
          <span class="perm-timer" title="Сколько ждёт ответа">
            {sheet ? '' : 'ждёт '}
            {formatTimer(now - Date.parse(event.ts))}
          </span>
        )}
      </div>
      {v.what && (
        <div class="perm-what">
          <pre>
            {v.shell && <span class="perm-prompt">$ </span>}
            {v.what}
          </pre>
        </div>
      )}
      {note && (
        <div class="perm-note">
          <IconInfo />
          <span>{note}</span>
        </div>
      )}
      {live && !replying && (
        <div class="perm-actions">
          {once && (
            <button
              type="button"
              class="btn btn-primary btn-allow"
              data-option={once.id}
              data-canonical={once.canonical}
              title={once.label}
              disabled={busy}
              onClick={() => answer(once)}
            >
              {optionLabel(once)}
              {!sheet && <kbd class="kbd">⏎</kbd>}
            </button>
          )}
          <div class="perm-more">
            {others.map((o) => (
              <button
                type="button"
                key={o.id}
                class={`btn btn-secondary ${isDeny(o) ? 'btn-deny-more' : ''}`}
                data-option={o.id}
                data-canonical={o.canonical}
                title={o.label}
                disabled={busy}
                onClick={() => answer(o)}
              >
                {optionLabel(o)}
              </button>
            ))}
            {deny && (
              <button
                type="button"
                class="btn btn-ghost btn-deny"
                data-option={deny.id}
                data-canonical={deny.canonical}
                title={deny.label}
                disabled={busy}
                onClick={() => answer(deny)}
              >
                {optionLabel(deny)}
                {!sheet && <kbd class="kbd">Esc</kbd>}
              </button>
            )}
          </div>
        </div>
      )}
      {live && deny && !v.worker && !replying && (
        <button type="button" class="btn-link perm-reply-open" onClick={() => setReplying(true)}>
          Ответить текстом вместо этого
        </button>
      )}
      {live && replying && (
        <form
          class="perm-reply"
          onSubmit={(e) => {
            e.preventDefault();
            sendReply();
          }}
        >
          <div class="perm-reply-hint">
            Ответ отклонит {v.shell ? <code>{v.what}</code> : 'запрос'} и уйдёт агенту
          </div>
          <textarea
            class="input"
            rows={2}
            placeholder="Что сделать вместо этого?"
            aria-label="Ответ агенту"
            value={reply}
            disabled={busy}
            ref={field}
            onInput={(e) => setReply((e.currentTarget as HTMLTextAreaElement).value)}
            onKeyDown={(e) => {
              if (e.key === 'Escape') {
                e.preventDefault();
                setReplying(false);
              } else if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
                e.preventDefault();
                sendReply();
              }
            }}
          />
          <div class="perm-reply-actions">
            <button type="submit" class="btn btn-primary" disabled={busy || !reply.trim()}>
              Отклонить и отправить
            </button>
            <button type="button" class="btn btn-ghost" onClick={() => setReplying(false)}>
              Вернуться к запросу
            </button>
          </div>
        </form>
      )}
      {open && expired && <div class="perm-status muted">Срок ответа истёк</div>}
      {outcome && <div class="perm-status outcome">Ответ: {outcome.text}</div>}
      {error && <div class="error-text">{error}</div>}
    </section>
  );
}
