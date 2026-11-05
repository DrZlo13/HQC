// The permission request (T26, SPEC §10.1): one card instead of a tool row and a panel —
// what the agent wants to run, how long it waits, why Claude Code asked, buttons by
// `options[]`, Enter and Esc while the card is on screen, and a text answer instead of the
// call. Answered or cancelled, it folds into a row "$ ls / · allowed". On a phone the open
// request is a bottom sheet in place of the composer.

import { useEffect, useRef, useState } from 'preact/hooks';
import type { ApprovalOption, EventOf, RuleSuggestion } from '../proto';
import { ruleError } from '../rules';
import type { ChatActions, FolderActions } from './cards';
import { callArgs, callLine, callPaths, fileChange, outsidePath } from './feed';
import { CodeBlock } from './Markdown';
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
  Bash: 'The agent wants to run a command',
  Edit: 'The agent wants to edit a file',
  MultiEdit: 'The agent wants to edit a file',
  Write: 'The agent wants to write a file',
  NotebookEdit: 'The agent wants to edit a notebook',
  Read: 'The agent wants to read a file',
  WebFetch: 'The agent wants to open a page',
  WebSearch: 'The agent wants to search the web',
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
        {cap} <code>{outside}</code> is outside the working folder <code>{homePath(root!)}</code>
      </>
    );
  if (p.policy_hint === 'allow') {
    return (
      <>
        A worker rule allows {p.tool}, but Claude Code asked anyway{outside ? <>: {where('the path')}.</> : '.'}
      </>
    );
  }
  if (p.policy_hint === 'deny') return <>A worker rule denies {p.tool}.</>;
  if (p.policy_hint === 'unknown') return <>The worker policy has a rule for this call.</>;
  if (outside) return <>{where('The path')}.</>;
  return null;
}

/** An answered or cancelled request: what became of it. */
export function outcomeText(event: EventOf<'approval.request'>, actions: ChatActions): { ok: boolean; text: string } | null {
  const fate = actions.timeline.requestFate(event.payload.request_id);
  if (fate.cancel) return { ok: false, text: `cancelled: ${CANCEL_LABELS[fate.cancel.reason]}` };
  const r = fate.response;
  if (!r || !('option_id' in r)) return null;
  const opt = event.payload.options.find((o) => o.id === r.option_id);
  const deny = opt ? isDeny(opt) : false;
  let text = opt ? (opt.canonical === 'unknown' ? opt.label : CANONICAL_OUTCOMES[opt.canonical]) : r.option_id;
  if (r.message) text = 'denied, replied with text';
  else if (r.by === 'auto') text = `${text} automatically`;
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
          {again ? 'Hide the request' : 'Show the request again'}
        </button>
      </div>
      {again && <PermissionCard event={event} call={call} actions={actions} />}
    </div>
  );
}

/** The patterns of "Allow in this folder" (T48, `Session.dc.html`; on a phone the full-screen
 *  page of `MSession2.dc.html`): one field per part, each a rule hqc suggests, editable. */
function FolderForm({
  sheet,
  folder,
  suggestions,
  patterns,
  errors,
  busy,
  error,
  canSave,
  onChange,
  onSave,
  onCancel,
}: {
  sheet?: boolean;
  folder: FolderActions;
  suggestions: RuleSuggestion[];
  patterns: string[];
  errors: (string | null)[];
  busy: boolean;
  error: string | null;
  canSave: boolean;
  onChange: (i: number, value: string) => void;
  onSave: () => void;
  onCancel: () => void;
}) {
  const n = suggestions.length;
  const note =
    n > 1 ? (
      <>
        The command has {n} parts. HQC suggests a rule for each; it applies only when every part is allowed. Rules work in
        all sessions of <code>{folder.path}</code> right away.
      </>
    ) : (
      <>
        The rule works in all sessions of <code>{folder.path}</code> right away. Denies in <code>worker.toml</code> still
        win.
      </>
    );
  const fields = (
    <div class="perm-folder-fields">
      {suggestions.map((s, i) => (
        <label key={i} class="perm-folder-field">
          <span class="perm-folder-part mono" title={s.part}>
            {s.part}
          </span>
          <input
            class={`input mono ${errors[i] ? 'is-invalid' : ''}`}
            aria-label={`Rule for part ${i + 1}`}
            placeholder={s.rule ? '' : 'No pattern: write one or leave empty'}
            value={patterns[i]}
            disabled={busy}
            onInput={(e) => onChange(i, (e.currentTarget as HTMLInputElement).value)}
          />
          {errors[i] && <span class="error-text small">{errors[i]}</span>}
        </label>
      ))}
    </div>
  );
  const save = (
    <button type="button" class="btn btn-primary btn-save-folder" disabled={busy || !canSave} onClick={onSave}>
      Save and allow
    </button>
  );
  if (sheet) {
    return (
      <section class="perm-folder-page" aria-label="Allow in this folder">
        <header class="perm-folder-bar">
          <button type="button" class="btn btn-ghost" onClick={onCancel}>
            Back
          </button>
          <div class="perm-folder-title">Allow in this folder</div>
          <span class="perm-folder-pad" />
        </header>
        <div class="perm-folder-body">
          <div class="perm-note">
            <IconInfo />
            <span>{note}</span>
          </div>
          {fields}
          <a href={folder.rulesHref}>Open folder rules →</a>
          {error && <div class="error-text">{error}</div>}
        </div>
        <div class="perm-folder-foot">{save}</div>
      </section>
    );
  }
  return (
    <div class="perm-folder">
      <div class="perm-note">
        <IconInfo />
        <span>{note}</span>
      </div>
      {fields}
      <div class="perm-folder-actions">
        {save}
        <button type="button" class="btn btn-ghost" onClick={onCancel}>
          Cancel
        </button>
        <span class="spacer" />
        <a href={folder.rulesHref}>Open folder rules →</a>
      </div>
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
  // "Allow in this folder" (T48): the worker's suggestion per part, editable.
  const [folderOpen, setFolderOpen] = useState(false);
  const [patterns, setPatterns] = useState<string[]>(() => (p.rule_suggestions ?? []).map((s) => s.rule ?? ''));
  const card = useRef<HTMLElement>(null);
  const field = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    if (replying) field.current?.focus();
  }, [replying]);
  const v = requestView(event, call);
  // What an edit or a write changes, from the call's arguments (cut at 4 KB, SPEC §4).
  const change = call && !v.worker ? fileChange(p.tool, call.payload.args_preview) : undefined;
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
    ? 'The worker asks for permission'
    : sheet && p.tool === 'Bash'
      ? 'Run the command?'
      : TITLES[p.tool] ?? `The agent wants to use ${p.tool}`;
  const subtitle = v.worker
    ? WORKER_REQUEST_LABELS[p.tool] ?? p.tool
    : [p.tool, v.description].filter(Boolean).join(' · ');
  const outcome = outcomeText(event, actions);
  const others = p.options.filter((o) => o !== once && o !== deny);
  const sendReply = () => {
    if (deny && reply.trim()) answer(deny, reply.trim());
  };
  const folder = actions.folder;
  const suggestions = p.rule_suggestions ?? [];
  const canFolder = !!folder && !!once && suggestions.length > 0 && !v.worker;
  const rules = patterns.map((r) => r.trim()).filter(Boolean);
  const patternErrors = patterns.map((r) => (r.trim() ? ruleError(r.trim()) : null));
  // Saved first: a rule the hub did not store allows nothing (T48).
  const saveAndAllow = () =>
    run(async () => {
      if (!folder || !once) return;
      await folder.save(rules);
      await actions.approve(p.request_id, once.id);
    });
  const folderForm = canFolder && folderOpen && live && (
    <FolderForm
      sheet={sheet}
      folder={folder!}
      suggestions={suggestions}
      patterns={patterns}
      errors={patternErrors}
      busy={busy}
      error={sheet ? error : null}
      canSave={rules.length > 0 && patternErrors.every((e) => !e)}
      onChange={(i, value) => setPatterns(patterns.map((x, j) => (j === i ? value : x)))}
      onSave={saveAndAllow}
      onCancel={() => setFolderOpen(false)}
    />
  );

  return (
    <section
      ref={card}
      class={`perm ${sheet ? 'perm-sheet' : 'perm-card'} ${live ? 'is-open' : 'is-closed'}`}
      aria-label="Permission request"
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
          <span class="perm-timer" title="How long it has been waiting">
            {sheet ? '' : 'waiting '}
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
      {change && (
        <div class="perm-change">
          {change.parts.map((c, i) => (
            <CodeBlock key={i} code={c.code} lang={c.diff ? 'diff' : ''} title={c.title} />
          ))}
          {change.cut && <div class="perm-cut muted">The preview is cut at 4 KB: the rest of the change is not shown.</div>}
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
              {!sheet && hotkeys && <kbd class="kbd">⏎</kbd>}
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
            {canFolder && (
              <button
                type="button"
                class={`btn btn-secondary btn-folder ${folderOpen ? 'is-on' : ''}`}
                aria-expanded={folderOpen}
                title="Save rules for this folder and allow the call"
                disabled={busy}
                onClick={() => setFolderOpen(!folderOpen)}
              >
                {!sheet && <IconShield />}
                {sheet ? 'In this folder…' : 'Allow in this folder'}
              </button>
            )}
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
                {!sheet && hotkeys && <kbd class="kbd">Esc</kbd>}
              </button>
            )}
          </div>
        </div>
      )}
      {!replying && folderForm}
      {live && deny && !v.worker && !replying && (
        <button type="button" class="btn-link perm-reply-open" onClick={() => setReplying(true)}>
          Reply with text instead
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
            The reply denies {v.shell ? <code>{v.what}</code> : 'the request'} and goes to the agent
          </div>
          <textarea
            class="input"
            rows={2}
            placeholder="What to do instead?"
            aria-label="Reply to the agent"
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
              Deny and send
            </button>
            <button type="button" class="btn btn-ghost" onClick={() => setReplying(false)}>
              Back to the request
            </button>
          </div>
        </form>
      )}
      {open && expired && <div class="perm-status muted">The time to answer has run out</div>}
      {outcome && <div class="perm-status outcome">Answer: {outcome.text}</div>}
      {error && <div class="error-text">{error}</div>}
    </section>
  );
}
