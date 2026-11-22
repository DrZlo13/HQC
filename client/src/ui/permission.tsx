// The permission request (T26, SPEC §10.1): one card instead of a tool row and a panel —
// what the agent wants to run, how long it waits, why Claude Code asked, buttons by
// `options[]`, Enter and Esc while the card is on screen, and a text answer instead of the
// call. Answered or cancelled, it folds into a row "$ ls / · allowed". On a phone the open
// request is a bottom sheet in place of the composer.

import type { ComponentChildren } from 'preact';
import { useEffect, useLayoutEffect, useRef, useState } from 'preact/hooks';
import type { ApprovalOption, EventOf, RequestSubagent, RuleSuggestion } from '../proto';
import { ruleError } from '../rules';
import type { ChatActions, FolderActions } from './cards';
import { CHANGE_TOOLS, argsText, callLine, callLineText, callPaths, cutJson, fileChange, isRecord, outsidePath, relativePaths, toolLabel } from './feed';
import { CodeBlock } from './Markdown';
import { useAction, useNow } from './hooks';
import { IconCheck, IconCross, IconInfo, IconKey, IconShield } from './icons';
import { SecretForm } from './secrets';
import {
  CANCEL_LABELS,
  CANONICAL_OUTCOMES,
  WORKER_REQUEST_LABELS,
  formatTimer,
  homePath,
  isDeny,
  optionLabel,
} from './labels';

/** What the card says the agent, or (since 0.181) the subagent that asks, wants to do. */
const TITLES: Record<string, string> = {
  Bash: 'wants to run a command',
  Edit: 'wants to edit a file',
  MultiEdit: 'wants to edit a file',
  Write: 'wants to write a file',
  NotebookEdit: 'wants to edit a notebook',
  Read: 'wants to read a file',
  WebFetch: 'wants to open a page',
  WebSearch: 'wants to search the web',
};

/** Who asks, as a card names it: the agent, or the subagent the request names. */
export function asker(subagent: RequestSubagent | undefined, agent: string): string {
  return subagent ? `Subagent “${subagent.description}”` : agent;
}

/** The length at which `callLine` cuts a call's one line: a single argument that would be cut
 *  there is shown as a block. */
const ONE_LINE_LIMIT = 400;

/** Whether the content of a box (`max-height`, scrolls inside) is taller than the box, measured
 *  after a render that changes `dep`, before it is painted, and when the box changes its size
 *  (a phone turned, a window resized). */
function useScrolls(box: { current: HTMLElement | null }, dep: unknown): boolean {
  const [scrolls, setScrolls] = useState(false);
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return setScrolls(false);
    const measure = () => setScrolls(el.scrollHeight > el.clientHeight);
    measure();
    if (typeof ResizeObserver === 'undefined') return;
    const watch = new ResizeObserver(measure);
    watch.observe(el);
    return () => watch.disconnect();
  }, [dep]);
  return scrolls;
}

/** What a request is about: the command or path, the CLI's description, the paths touched, and
 *  the arguments the card shows whole. The arguments come from the request itself (SPEC 0.207),
 *  else from the call right before it. */
export function requestView(event: EventOf<'approval.request'>, call?: EventOf<'tool.call'>) {
  const p = event.payload;
  // A worker's own request (SPEC §10.2): its description is the whole story.
  const worker = event.author.kind === 'system' || p.tool in WORKER_REQUEST_LABELS;
  const lines = p.description
    .split('\n')
    .map((s) => s.trim())
    .filter(Boolean);
  const preview = p.args_preview ?? call?.payload.args_preview;
  const read = preview !== undefined ? cutJson(preview) : undefined;
  const rec = read && isRecord(read.value) ? read.value : undefined;
  // A Bash call without a command in its arguments is told by the description.
  let what = preview !== undefined && (p.tool !== 'Bash' || typeof rec?.command === 'string') ? callLine(p.tool, preview).text : '';
  if (!what && !worker && lines.length > 1) what = lines[lines.length - 1];
  if (!what && !worker && p.tool === 'Bash') what = lines[0] ?? '';
  // A command is shown whole, as it will run; its description is the call's own.
  const command = !worker && p.tool === 'Bash' && typeof rec?.command === 'string' && rec.command.trim() ? rec.command : undefined;
  // The fields the card shows as indented JSON: every one of a call with several, what a Bash
  // call has besides its command and description.
  const skip = command !== undefined ? ['command', 'description'] : [];
  const fields = rec && !worker && !CHANGE_TOOLS.includes(p.tool) ? Object.keys(rec).filter((k) => !skip.includes(k)) : [];
  // One field is told by `callLine`, unless that would cut it: a text longer than the line, or
  // one with a line break (a script) that the line would run together.
  const longOnly =
    fields.length === 1 &&
    !!rec &&
    (callLineText(rec).length > ONE_LINE_LIMIT || Object.values(rec).some((x) => typeof x === 'string' && x.includes('\n')));
  const showArgs = command !== undefined ? fields.length > 0 : p.tool !== 'Bash' && (fields.length > 1 || longOnly);
  // The preview ends where the cut value ends: the last key of the record is the cut one.
  const cutCommand = command !== undefined && !!read?.cut && Object.keys(rec!).at(-1) === 'command';
  const commandLines = command !== undefined ? command.replace(/\n$/, '').split('\n').length : 0;
  const description = worker
    ? ''
    : command !== undefined
      ? typeof rec?.description === 'string' && rec.description !== command
        ? rec.description.split('\n').map((s) => s.trim()).filter(Boolean).join(' ')
        : ''
      : lines.filter((l) => l !== what && (preview === undefined || !l.startsWith(`${p.tool} {`))).join(' ');
  const text = worker ? p.description : what;
  const args = rec ?? (p.tool === 'Bash' ? { command: what } : { file_path: what });
  return {
    worker,
    shell: p.tool === 'Bash' && !!what,
    what: text,
    // What the frame shows: the whole command, else the one line; with several arguments, none.
    frame: showArgs && command === undefined ? '' : command ?? text,
    cutCommand,
    commandLines,
    preview,
    showArgs,
    skip,
    description,
    paths: worker ? [] : callPaths(p.tool, args),
  };
}

/** A call's arguments as indented JSON in a `pre` (the opened row of the feed, the permission
 *  card), the cut mark under it unless `cutMarked` says the cut was in a field not shown here;
 *  without the fields in `skip`. Not JSON: as it is. */
export function CallArgs({
  preview,
  skip,
  pre,
  cutClass,
  cutMarked = true,
  scrollNote,
}: {
  preview: string;
  skip?: string[];
  pre: string;
  cutClass: string;
  cutMarked?: boolean;
  scrollNote?: string;
}) {
  const a = argsText(preview, skip);
  const box = useRef<HTMLPreElement>(null);
  const scrolls = useScrolls(box, a?.text ?? preview);
  return (
    <>
      <pre class={pre} ref={box}>
        {a?.text ?? preview}
      </pre>
      {scrollNote && scrolls && <div class="perm-lines muted">{scrollNote}</div>}
      {cutMarked && a?.cut && <div class={`${cutClass} muted`}>The preview is cut at 4 KB: the rest of the arguments are not shown.</div>}
    </>
  );
}

/** What an option grants beyond this call: a sentence for its button's tooltip and the same in
 *  elements for the line under the buttons, the rules in `code`. None for an option that says
 *  nothing (an old worker). */
function grantText(o: ApprovalOption): { title: string; line: ComponentChildren } | undefined {
  const g = o.grant;
  if (!g || g.place === 'unknown') return undefined;
  const list = g.rules.map((r, i) => (
    <>
      {i > 0 && ', '}
      <code>{r}</code>
    </>
  ));
  if (g.place === 'session') {
    const rest = g.rules.length ? (g.exact ? ' and this exact call' : '') : 'only this exact call';
    return {
      title: `Allows ${g.rules.length ? g.rules.join(', ') : ''}${rest} until the agent's process ends.`,
      line: (
        <>
          {list}
          {rest}, until the agent's process ends
        </>
      ),
    };
  }
  const written = g.rules.length ? g.rules.join(', ') : 'rules';
  // Where the CLI writes it depends on the git repository of the session's cwd, which the card
  // cannot know (SPEC §8.1).
  const where = "at the root of the session's git repository (outside one, of its folder)";
  return {
    title: `Writes ${written} to .claude/settings.local.json ${where}: every Claude Code session there.`,
    line: (
      <>
        writes {g.rules.length ? list : 'rules'} to <code>.claude/settings.local.json</code> {where}: every Claude Code session
        there
      </>
    ),
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
  // The hub's answer to an `hqc_secret` the owner filled in (SPEC 0.147).
  if (r.option_id === SECRET_GIVEN) return { ok: true, text: 'given' };
  const opt = event.payload.options.find((o) => o.id === r.option_id);
  const deny = opt ? isDeny(opt) : false;
  let text = opt ? (opt.canonical === 'unknown' ? opt.label : CANONICAL_OUTCOMES[opt.canonical]) : r.option_id;
  if (r.message) text = 'denied, replied with text';
  else if (r.by === 'auto') text = `${text} automatically`;
  return { ok: !deny, text };
}

/** `option_id` of the `approval.response` the hub writes when a secret was given. */
export const SECRET_GIVEN = 'give';

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
  if (event.payload.tool === 'hqc_secret' && event.payload.secret) return <SecretCard event={event} actions={actions} sheet={sheet} />;
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
          {/* As the call's row in the feed: `$ command`, else the tool and its main argument. */}
          {v.worker
            ? WORKER_REQUEST_LABELS[event.payload.tool] ?? event.payload.tool
            : v.shell
              ? `$ ${relativePaths(v.what, actions.workspaceRoot)}`
              : [toolName(event.payload.tool, event.payload.display_name), relativePaths(v.what, actions.workspaceRoot)]
                  .filter(Boolean)
                  .join(' ')}
        </span>
        <span class="perm-done-outcome outcome">· {outcome.text}</span>
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

/** An open menu (the session column's, the project bar's, the chat header's pills and
 *  "More"), the workers' list above the column's bottom line, or the session column slid out
 *  over the chat (a narrow window: its Esc puts the column away). */
const OPEN_POPUP = '[role="menu"], .wline-pop, .sidebar-slide';

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
  const name = toolName(p.tool, p.display_name);
  // What an edit or a write changes, from the call's arguments (cut at 4 KB, SPEC §4): the
  // request's own, else the call's.
  const change = v.preview !== undefined && !v.worker ? fileChange(p.tool, v.preview) : undefined;
  const frameBox = useRef<HTMLPreElement>(null);
  const frameScrolls = useScrolls(frameBox, v.frame);
  const outside = outsidePath(v.paths, actions.workspaceRoot);
  const note = v.worker ? null : noteText(event, outside, actions.workspaceRoot);
  const once = p.options.find((o) => o.canonical === 'once');
  const deny = p.options.find((o) => o.canonical === 'deny') ?? p.options.find((o) => o.canonical === 'deny_always');
  const answer = (o: ApprovalOption, message?: string) =>
    run(() => (message === undefined ? actions.approve(p.request_id, o.id) : actions.approve(p.request_id, o.id, message)));

  // Enter allows once, Esc denies: only for the request the chat gives the keys to, while
  // its card is on screen and the focus is not in a field.
  // Never a publication (`hqc_publish`): a public link is made by a click on its button only.
  const hotkeys = live && !busy && actions.hotkeyRequest === p.request_id && p.tool !== 'hqc_publish';
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
      // A menu or the workers' list is open: Esc and Enter are its (Esc closes it), wherever
      // the focus is — inside it, or on the page when a click did not move the focus. The key
      // came from a menu another handler has just closed: its too.
      if (doc.querySelector(OPEN_POPUP) || (e.target instanceof Element && e.target.closest(OPEN_POPUP))) return;
      const o = e.key === 'Enter' ? once : e.key === 'Escape' ? deny : undefined;
      if (!o) return;
      e.preventDefault();
      answer(o);
    };
    doc.addEventListener('keydown', onKey);
    return () => doc.removeEventListener('keydown', onKey);
  }, [hotkeys, p.request_id]);

  // The hub's cards of an agent's `hqc_move` and `hqc_publish` (SPEC §7, §10.1) and of a move
  // after the transcript (SPEC §9.2) have a worker request's form.
  const publish = v.worker && p.tool === 'hqc_publish' && !actions.system;
  const title = v.worker
    ? p.tool === 'hqc_move' && !actions.system
      ? 'The agent asks to move this session'
      : p.tool === 'session.follow' && !actions.system
        ? 'Move this session after its transcript?'
        : publish
          ? 'The agent asks to publish'
          : 'The worker asks for permission'
    : sheet && p.tool === 'Bash'
      ? p.subagent
        ? `${asker(p.subagent, '')}: run the command?`
        : 'Run the command?'
      : `${asker(p.subagent, 'The agent')} ${TITLES[p.tool] ?? `wants to use ${name}`}`;
  const subtitle = v.worker
    ? WORKER_REQUEST_LABELS[p.tool] ?? p.tool
    : [name, v.description].filter(Boolean).join(' · ');
  const outcome = outcomeText(event, actions);
  const others = p.options.filter((o) => o !== once && o !== deny);
  const grants = p.options.flatMap((o) => {
    const g = grantText(o);
    return g ? [[o, g] as const] : [];
  });
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
          {/* Why the runtime asks. */}
          {p.reason && !v.worker && <div class="perm-sub perm-reason">{p.reason}</div>}
        </div>
        {live && (
          <span class="perm-timer" title="How long it has been waiting">
            {sheet ? '' : 'waiting '}
            {formatTimer(now - Date.parse(event.ts))}
          </span>
        )}
      </div>
      {v.frame && (
        <div class="perm-what">
          <pre ref={frameBox}>
            {v.shell && <span class="perm-prompt">$ </span>}
            {v.frame}
          </pre>
        </div>
      )}
      {frameScrolls && (
        <div class="perm-lines muted">
          {v.commandLines > 1
            ? `${v.commandLines} lines: the box scrolls`
            : v.commandLines
              ? 'The command scrolls in the box'
              : 'The call scrolls in the box'}
        </div>
      )}
      {v.cutCommand && <div class="perm-cut muted">The command is cut at 4 KB: the rest is not shown.</div>}
      {v.showArgs && v.preview !== undefined && (
        <div class="perm-args">
          <CallArgs preview={v.preview} skip={v.skip} pre="" cutClass="perm-cut" cutMarked={!v.cutCommand} scrollNote="The arguments scroll in the box" />
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
              title={grantText(once)?.title ?? once.label}
              disabled={busy}
              onClick={() => answer(once)}
            >
              {publish ? 'Publish' : optionLabel(once)}
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
                title={grantText(o)?.title ?? o.label}
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
      {live && !replying && grants.length > 0 && (
        <ul class="perm-grants muted">
          {grants.map(([o, g]) => (
            <li key={o.id}>
              {optionLabel(o)} — {g.line}
            </li>
          ))}
        </ul>
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

/** The hub's card of an agent's `hqc_secret` (SPEC 0.147, §7): what it asks for and why, the
 *  value's fields, Give and Decline. Answered, it folds into a row like a permission. */
function SecretCard({ event, actions, sheet }: { event: EventOf<'approval.request'>; actions: ChatActions; sheet?: boolean }) {
  const p = event.payload;
  const asked = p.secret!;
  const outcome = outcomeText(event, actions);
  const { busy, error, run } = useAction();
  const deny = p.options.find((o) => o.canonical === 'deny');
  const what = asked.kind === 'ssh_key' ? `ssh key ${asked.name}` : `secret ${asked.name}`;
  if (outcome) {
    return (
      <div class="perm-done secret-done" data-request={p.request_id}>
        <div class="perm-done-row">
          <span class={`mark ${outcome.ok ? 'mark-ok' : 'mark-no'}`} aria-hidden="true">
            {outcome.ok ? <IconCheck /> : <IconCross />}
          </span>
          <span class="perm-done-what mono">{what}</span>
          <span class="perm-done-outcome outcome">· {outcome.text}</span>
        </div>
      </div>
    );
  }
  const give = actions.giveSecret;
  return (
    <section
      class={`perm ${sheet ? 'perm-sheet' : 'perm-card'} is-open secret-card`}
      aria-label="Secret request"
      data-request={p.request_id}
    >
      <div class="perm-head">
        <span class="perm-icon">
          <IconKey size={15} />
        </span>
        <div class="perm-titles">
          <div class="perm-title">
            The agent asks for {asked.kind === 'ssh_key' ? 'an ssh key' : 'a secret'}: <span class="mono">{asked.name}</span>
          </div>
          {p.description && <div class="perm-sub secret-purpose">{p.description}</div>}
        </div>
      </div>
      {give ? (
        <SecretForm
          asked={asked}
          giveLabel="Give"
          cancelLabel="Decline"
          escapeCancels={false}
          onGive={(f) => give(p.request_id, f)}
          onCancel={() => deny && run(() => actions.approve(p.request_id, deny.id))}
        />
      ) : (
        <div class="perm-actions">
          <span class="muted small">Open the session to give it.</span>
          {deny && (
            <button type="button" class="btn btn-ghost btn-deny" disabled={busy} onClick={() => run(() => actions.approve(p.request_id, deny.id))}>
              Decline
            </button>
          )}
        </div>
      )}
      {busy && <div class="muted small">Declining…</div>}
      {error && <div class="error-text">{error}</div>}
    </section>
  );
}

/** The tool as the runtime names it (`display_name`), an MCP server's tool as `<tool> (<server>)`:
 *  the CLI's `display_name` of one drops its server, so `mcp__prod-db__query` and
 *  `mcp__staging-db__query` would both be "Query". */
function toolName(tool: string, displayName?: string): string {
  return tool.startsWith('mcp__') ? toolLabel(tool) : (displayName ?? tool);
}
