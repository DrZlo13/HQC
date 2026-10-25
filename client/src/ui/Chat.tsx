import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'preact/hooks';
import { ApiError } from '../api';
import type { AppState } from '../app';
import { ArtifactStore, formatSize } from '../artifacts';
import type { SessionInfo, SessionState } from '../proto';
import { ChatSession, type OutgoingFile } from '../session';
import type { Timeline } from '../timeline';
import { ActivityLine, ChatItems, StreamingMessage, onlyRequests, type ChatActions } from './cards';
import { buildFeed } from './feed';
import { useSubscription } from './hooks';
import { IconAttach, IconBack, IconCode, IconMore, IconSend, IconStop } from './icons';
import { STATE_LABELS, formatCost, homePath, modelName } from './labels';
import { CopyButton } from './Markdown';
import { PermissionBlock } from './permission';

const STOPPABLE = new Set(['running', 'waiting', 'queued']);

/** `focusSeq`: scroll to that event once it is loaded and mark it (a search hit, SPEC §15). */
export function Chat({
  app,
  sessionId,
  phone = false,
  focusSeq,
}: {
  app: AppState;
  sessionId: string;
  phone?: boolean;
  focusSeq?: number;
}) {
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

  // The folder of the session, for the header and the requests' "outside the working folder".
  useEffect(() => {
    if (info && !system && !app.workspaces.has(info.project_id)) app.loadWorkspaces(info.project_id).catch(() => {});
  }, [app, info?.project_id, system]);
  const path = info && !system ? app.sessionPath(info) : undefined;

  // The runtime's slash commands come with the session object (T27), which the summary does
  // not refresh: fetch it when an idle session has no list yet (after its first process).
  const fetchCommands = !system && info !== undefined && info.commands === undefined && state === 'idle';
  useEffect(() => {
    if (fetchCommands) void app.refreshSession(sessionId);
  }, [app, sessionId, fetchCommands]);
  const worker = app.workerName(info?.worker_id);

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

  const open = t.openRequests();
  const firstApproval = open.find((r) => r.type === 'approval.request');
  const sheetRequest = phone && !requestsOnly ? firstApproval?.payload.request_id : undefined;
  const actions: ChatActions = {
    timeline: t,
    approve: (r, o, m) => chat.approve(r, o, m),
    answer: (r, a) => chat.answer(r, a),
    resend: (op) => chat.resend(op),
    discard: (op) => chat.discard(op),
    artifacts,
    workspaceRoot: path,
    hotkeyRequest: firstApproval?.payload.request_id,
    sheetRequest,
    openArtifact: (id, download) => {
      // Open the window synchronously, or mobile browsers block it as a popup.
      const w = window.open('about:blank', '_blank');
      (download ? artifacts.downloadUrl(id) : artifacts.url(id))
        .then((url) => (w ? (w.location.href = url) : window.open(url, '_blank')))
        .catch((e: Error) => {
          w?.close();
          setNotice(`Attachment unavailable: ${e.message}`);
        });
    },
  };

  const act = (fn: () => Promise<void>, failed: string) =>
    fn().catch((e: Error) => setNotice(`${failed}: ${e.message}`));

  const showRetry = !system && (state === 'error' || t.hasFailedDelivery());
  const items = requestsOnly ? onlyRequests(t.items()) : t.items();
  const locked = state === 'locked_vscode';
  // SPEC §9.3: read-only while VS Code has it. Open there right now (`held_by`): it has to be
  // closed there first, the hub refuses takeover (`409 vscode_open`). Written from there
  // recently but not held (or a worker without the registry): "Take over here", which the hub
  // may refuse until 30 s of silence (`409 vscode_active`).
  const held = locked && info?.held_by !== undefined;
  const takeover = () =>
    app.api.takeover(sessionId).then(
      () => {
        setNotice(null);
        return app.refreshSessions();
      },
      (e: unknown) => {
        if (e instanceof ApiError && e.code === 'vscode_open') {
          setNotice(e.message);
          return app.refreshSessions();
        }
        setNotice(
          e instanceof ApiError && e.code === 'vscode_active'
            ? 'VS Code is still writing to this session; you can take it over after 30 s of quiet.'
            : `Not taken over: ${(e as Error).message}`,
        );
      },
    );
  const scroller = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  useLayoutEffect(() => {
    const el = scroller.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  });
  // The jump to a search hit: the history loads in full on open (ChatSession.open), so the
  // event is there once `lastSeq` reaches it; the block that holds it is the last one that
  // starts at or before it (a hidden event's neighbour).
  const focused = useRef<number | undefined>(undefined);
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el || focusSeq === undefined || focused.current === focusSeq || t.lastSeq < focusSeq) return;
    let target: HTMLElement | undefined;
    for (const b of el.querySelectorAll<HTMLElement>('.feed-block[data-seq]')) {
      if (Number(b.dataset.seq) <= focusSeq) target = b;
    }
    if (!target) return;
    focused.current = focusSeq;
    stick.current = false;
    el.querySelector('.feed-block.is-found')?.classList.remove('is-found');
    target.classList.add('is-found');
    target.scrollIntoView?.({ block: 'center' });
  });

  const archive = () => act(() => app.api.archive(sessionId).then(() => app.refreshSessions()), 'Not archived');
  const unarchive = () => act(() => app.api.unarchive(sessionId).then(() => app.refreshSessions()), 'Not restored');
  const remove = () => {
    if (!info || !confirm('Delete the session with its history? This cannot be undone.')) return;
    const projectId = info.project_id;
    void act(
      () =>
        app.api.deleteSession(sessionId).then(() => {
          app.removeSession(sessionId);
          location.hash = `#/p/${projectId}`;
        }),
      'Not deleted',
    );
  };
  const stop = () => act(() => chat.stop(), 'Not stopped');
  const { model, cost } = t.usageTotals();
  const pill = <StatePill state={state} timeline={t} />;
  const conn = app.conn !== 'open' && <span class="conn-lost">offline</span>;

  const disabled = state === 'archived' || locked;
  const sheet = sheetRequest && firstApproval?.type === 'approval.request' ? firstApproval : undefined;
  return (
    <section
      class={`chat ${dragging ? 'chat-dragging' : ''} ${phone ? 'chat-phone' : ''}`}
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
      <header class="chat-head">
        {phone && (
          <a class="icon-btn icon-btn-round back" href={info ? `#/p/${info.project_id}` : '#/'} aria-label="Back to sessions">
            <IconBack />
          </a>
        )}
        <div class="chat-head-main">
          <h1 class="chat-title">{info?.title || 'Session'}</h1>
          <div class="chat-meta">
            {pill}
            {system && <span class="chat-system-note">the worker's service session</span>}
            {phone ? (
              <span class="chat-where mono">{[worker, path && homePath(path).split('/').pop()].filter(Boolean).join(' · ')}</span>
            ) : (
              <>
                {worker && !system && <span>{worker}</span>}
                {path && (
                  <>
                    <span class="sep">/</span>
                    <span class="chat-path">
                      <span class="mono">{homePath(path)}</span>
                      <CopyButton text={path} label="Copy path" />
                    </span>
                  </>
                )}
                {model && (
                  <>
                    <span class="sep">/</span>
                    <span class="chat-model" title={model}>
                      {modelName(model)}
                    </span>
                  </>
                )}
                {cost !== undefined && (
                  <>
                    <span class="sep">/</span>
                    <span class="chat-cost">{formatCost(cost)} this session</span>
                  </>
                )}
                {locked && (
                  <>
                    <span class="sep">/</span>
                    <span>VS Code</span>
                  </>
                )}
              </>
            )}
            {conn}
          </div>
        </div>
        {system && (
          <button
            type="button"
            class={`btn btn-small btn-outline btn-filter ${requestsOnly ? 'is-on' : ''}`}
            aria-pressed={requestsOnly}
            onClick={() => setRequestsOnly(!requestsOnly)}
          >
            Requests only
          </button>
        )}
        {showRetry && !phone && (
          <button type="button" class="btn btn-small btn-outline btn-retry" onClick={() => act(() => chat.retry(), 'Retry failed')}>
            Retry
          </button>
        )}
        {!system && STOPPABLE.has(state) &&
          (phone ? (
            <button type="button" class="icon-btn icon-btn-round btn-stop" aria-label="Stop the agent" onClick={stop}>
              <IconStop size={14} />
            </button>
          ) : (
            <button type="button" class="btn btn-small btn-outline btn-stop" onClick={stop}>
              <IconStop />
              Stop
            </button>
          ))}
        {!system && info && (
          <MoreMenu
            info={info}
            phone={phone}
            path={path}
            model={model}
            cost={cost}
            onRetry={showRetry && phone ? () => void act(() => chat.retry(), 'Retry failed') : undefined}
            onArchive={archive}
            onUnarchive={unarchive}
            onDelete={remove}
          />
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
        <div class="feed">
          <ChatItems items={items} actions={actions} />
          {requestsOnly && items.length === 0 && <div class="empty">No requests</div>}
          {t.streaming && <StreamingMessage text={t.streaming} />}
          {!system && <ActivityLine state={state} timeline={t} />}
        </div>
      </div>
      {notice && (
        <div class="notice" role="alert" onClick={() => setNotice(null)}>
          {notice}
        </div>
      )}
      {locked && (
        <div class="composer-wrap">
          <div class={`handover ${held ? 'handover-held' : ''}`}>
            <span class="handover-icon">
              <IconCode />
            </span>
            <div class="handover-text">
              {held ? (
                <>
                  <div class="handover-title">
                    The session is open in {info?.held_by?.entrypoint === 'claude-vscode' ? 'VS Code' : 'Claude Code'}
                    {worker ? ` on ${worker}` : ''}.
                  </div>
                  <div class="handover-sub">Close it there to write from here.</div>
                </>
              ) : (
                <div class="handover-title">VS Code was the last to run this session{worker ? ` on ${worker}` : ''}.</div>
              )}
            </div>
            {!held && (
              <button type="button" class="btn btn-primary btn-takeover" onClick={() => void takeover()}>
                Take over here
              </button>
            )}
          </div>
        </div>
      )}
      {sheet && (
        <div class="sheet-wrap">
          <PermissionBlock event={sheet} call={approvalCall(items, t, sheet.payload.request_id)} actions={actions} sheet />
        </div>
      )}
      {!system && !locked && !sheet && (
        <Composer
          disabled={disabled}
          phone={phone}
          commands={info?.commands}
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

/** The `tool.call` an approval card shows (feed.ts), for the sheet. */
function approvalCall(items: ReturnType<Timeline['items']>, t: Timeline, requestId: string) {
  const b = buildFeed(items, t).find((x) => x.kind === 'approval' && x.event.payload.request_id === requestId);
  return b?.kind === 'approval' ? b.call : undefined;
}

/** The state in the header: a pill, pulsing while a turn is on. */
function StatePill({ state, timeline }: { state: SessionState; timeline: Timeline }) {
  let label = STATE_LABELS[state];
  if (state === 'waiting') {
    const r = timeline.openRequests()[0];
    label = r?.type === 'approval.request' ? 'Waiting for permission' : 'Waiting for reply';
  }
  return (
    <span class={`state pill state-${state}`}>
      <span class={`pill-dot ${state === 'running' ? 'pulse' : ''}`} aria-hidden="true" />
      {label}
    </span>
  );
}

/** "More": archive, restore and delete; on a phone also the path, model and cost. */
function MoreMenu({
  info,
  phone,
  path,
  model,
  cost,
  onRetry,
  onArchive,
  onUnarchive,
  onDelete,
}: {
  info: SessionInfo;
  phone: boolean;
  path?: string;
  model?: string;
  cost?: number;
  /** On a phone "Retry" lives here, not in the crowded header. */
  onRetry?: () => void;
  onArchive: () => void;
  onUnarchive: () => void;
  onDelete: () => void;
}) {
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const doc = box.current?.ownerDocument;
    if (!doc) return;
    const away = (e: Event) => {
      if (box.current && !box.current.contains(e.target as Node)) setOpen(false);
    };
    const esc = (e: KeyboardEvent) => e.key === 'Escape' && setOpen(false);
    doc.addEventListener('pointerdown', away);
    doc.addEventListener('keydown', esc);
    return () => {
      doc.removeEventListener('pointerdown', away);
      doc.removeEventListener('keydown', esc);
    };
  }, [open]);
  const item = (cls: string, label: string, fn: () => void, title?: string) => (
    <button
      type="button"
      role="menuitem"
      class={`menu-item ${cls}`}
      title={title}
      onClick={() => {
        setOpen(false);
        fn();
      }}
    >
      {label}
    </button>
  );
  const archived = info.state === 'archived';
  return (
    <div class="menu-box" ref={box}>
      <button
        type="button"
        class={`icon-btn ${phone ? 'icon-btn-round' : ''} btn-more`}
        aria-label="More"
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen(!open)}
      >
        <IconMore />
      </button>
      {open && (
        <div class="menu" role="menu">
          {phone && (path || model || cost !== undefined) && (
            <div class="menu-info">
              {path && (
                <div class="menu-info-row">
                  <span class="mono">{homePath(path)}</span>
                  <CopyButton text={path} label="Copy path" />
                </div>
              )}
              {model && <div class="menu-info-row">{modelName(model)}</div>}
              {cost !== undefined && <div class="menu-info-row">{formatCost(cost)} this session</div>}
            </div>
          )}
          {onRetry && item('btn-retry', 'Retry', onRetry)}
          {!archived && item('btn-archive', 'Archive', onArchive, 'Move the session to the archive; the agent stops')}
          {archived && item('btn-unarchive', 'Restore', onUnarchive)}
          {archived && item('btn-delete menu-danger', 'Delete', onDelete)}
        </div>
      )}
    </div>
  );
}

/** The slash command being typed: the text is `/` and a name so far, nothing after it. */
export function commandQuery(text: string): string | undefined {
  return /^\/(\S*)$/.exec(text)?.[1];
}

/** The commands the popup offers for `text`: those starting with what is typed (ignoring
 *  case), in the runtime's order; none when the only one is typed in full already. */
export function matchCommands(commands: string[] | undefined, text: string): string[] {
  const q = commandQuery(text);
  if (q === undefined || !commands?.length) return [];
  const lq = q.toLowerCase();
  const out = commands.filter((c) => c.toLowerCase().startsWith(lq));
  return out.length === 1 && out[0] === q ? [] : out;
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
  phone = false,
  commands,
  files = [],
  onFiles = () => {},
  onRemove = () => {},
}: {
  onSend: (text: string) => void;
  disabled: boolean;
  placeholder?: string;
  phone?: boolean;
  /** The runtime's slash commands: `/` at the start of the text opens a list of them. */
  commands?: string[];
  files?: OutgoingFile[];
  onFiles?: (files: FileList | File[]) => void;
  onRemove?: (index: number) => void;
}) {
  const [text, setText] = useState('');
  const area = useRef<HTMLTextAreaElement>(null);
  const picker = useRef<HTMLInputElement>(null);
  const canSend = !disabled && (text.trim() !== '' || files.length > 0);
  // The slash command popup (T27): the matches, the highlighted one, and the text Esc closed
  // it at (typing on reopens it).
  const [pick, setPick] = useState(0);
  const [closedAt, setClosedAt] = useState<string | null>(null);
  const matches = disabled || closedAt === text ? [] : matchCommands(commands, text);
  const popup = matches.length > 0;
  const active = Math.min(pick, matches.length - 1);
  const list = useRef<HTMLUListElement>(null);
  useLayoutEffect(() => {
    list.current?.children[active]?.scrollIntoView?.({ block: 'nearest' });
  }, [active, popup]);
  const edit = (t: string) => {
    // In the same update as the text: a key right after typing moves from the first match.
    setPick(0);
    setText(t);
  };
  const insert = (c: string) => {
    edit(`/${c} `);
    setClosedAt(null);
    area.current?.focus();
  };
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
  const attach = (
    <button
      type="button"
      class="icon-btn btn-attach"
      aria-label="Attach files"
      title="Attach files"
      disabled={disabled}
      onClick={() => picker.current?.click()}
    >
      <IconAttach />
    </button>
  );
  const send = (
    <button type="submit" class="btn-send" aria-label="Send" title="Send" disabled={!canSend}>
      <IconSend />
    </button>
  );
  const textarea = (
    <textarea
      ref={area}
      class="composer-input"
      aria-label="Message"
      aria-autocomplete={commands?.length ? 'list' : undefined}
      aria-expanded={commands?.length ? popup : undefined}
      aria-controls={popup ? 'composer-commands' : undefined}
      aria-activedescendant={popup ? `composer-command-${active}` : undefined}
      rows={phone ? 1 : 2}
      placeholder={placeholder ?? (disabled ? 'Session archived' : 'Message')}
      disabled={disabled}
      value={text}
      onInput={(e) => edit((e.currentTarget as HTMLTextAreaElement).value)}
      onPaste={(e) => {
        const pasted = [...(e.clipboardData?.files ?? [])];
        if (pasted.length === 0) return;
        e.preventDefault();
        onFiles(pasted);
      }}
      onKeyDown={(e) => {
        if (popup) {
          const n = matches.length;
          if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
            e.preventDefault();
            setPick((active + (e.key === 'ArrowDown' ? 1 : n - 1)) % n);
            return;
          }
          if ((e.key === 'Tab' || e.key === 'Enter') && !e.shiftKey && !e.isComposing) {
            e.preventDefault();
            insert(matches[active]);
            return;
          }
          if (e.key === 'Escape') {
            e.preventDefault();
            e.stopPropagation();
            setClosedAt(text);
            return;
          }
        }
        // On a phone Enter is a newline; on desktop Enter sends, Shift+Enter is a newline.
        if (e.key === 'Enter' && !e.shiftKey && !coarse && !e.isComposing) {
          e.preventDefault();
          submit();
        }
      }}
    />
  );
  return (
    <div class="composer-wrap">
      <form
        class={`composer ${phone ? 'composer-phone' : ''}`}
        onSubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        {popup && (
          <ul class="cmd-popup" id="composer-commands" role="listbox" aria-label="Commands" ref={list}>
            {matches.map((c, i) => (
              <li
                key={c}
                id={`composer-command-${i}`}
                role="option"
                aria-selected={i === active}
                class={`cmd-item ${i === active ? 'is-active' : ''}`}
                // Keep the focus (and a phone's keyboard) in the field.
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => insert(c)}
              >
                <span class="cmd-slash">/</span>
                {c}
              </li>
            ))}
          </ul>
        )}
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
                <button type="button" class="composer-file-remove" aria-label={`Remove ${f.name}`} onClick={() => onRemove(i)}>
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
        {phone ? (
          <div class="composer-row">
            {attach}
            {textarea}
            {send}
          </div>
        ) : (
          <>
            {textarea}
            <div class="composer-row">
              {attach}
              <span class="composer-hint">{disabled ? '' : 'Enter to send, Shift+Enter for a new line'}</span>
              {send}
            </div>
          </>
        )}
      </form>
    </div>
  );
}
