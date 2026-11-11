import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'preact/hooks';
import { ApiError, type SessionPatch, type WorkerLimits } from '../api';
import type { AppState } from '../app';
import { ArtifactStore, formatSize, type LinkCache } from '../artifacts';
import { onBack } from '../nav';
import type { PermissionMode, SessionInfo, SessionState } from '../proto';
import type { OutgoingFile } from '../session';
import { duringText, type Timeline } from '../timeline';
import { ModelField } from './agentSettings';
import { ArtifactViewer, type ViewedArtifact } from './attachments';
import { Limits } from './limits';
import { ActivityLine, FeedBlocks, StreamingMessage, blockSeq, onlyRequests, type ChatActions } from './cards';
import { peerMessages } from './relay';
import { buildFeed } from './feed';
import { useSubscription } from './hooks';
import { copyText } from '../clipboard';
import { ContextMenu } from './ContextMenu';
import { IconArchive, IconAttach, IconBack, IconCode, IconCopy, IconLock, IconMore, IconPanel, IconQuote, IconSend, IconShield, IconStop } from './icons';
import { AUTO_CONFIRM, STATE_LABELS, agentsLabel, formatCost, homePath, modelName, shownModel } from './labels';
import { LiveSubagents } from './LiveSubagents';
import { CopyButton } from './Markdown';
import { PermissionBlock } from './permission';
import { MoveSession } from './ProjectSettings';
import { WaitedRequests } from './requests';
import { SchedulesPanel } from './Schedules';
import { archivedLabel, atWorkAfterTurn } from './sessions';

const STOPPABLE = new Set(['running', 'waiting', 'queued']);
/** Blocks of the feed rendered on open; scrolling to the top adds `SHOWN` more. */
const SHOWN = 60;

/** The signed links of all chats of the page, one map: a chat opened again gets the same links
 *  while they live, and the browser shows its images from its cache. Emptied whenever the
 *  hub's socket opens: a hub restarted without `HQC_ARTIFACT_KEY` signs with a new key. The
 *  metadata stays per chat opened. */
const linkCaches = new WeakMap<AppState, LinkCache>();
function sharedLinks(app: AppState): LinkCache {
  let links = linkCaches.get(app);
  if (!links) {
    const fresh: LinkCache = new Map();
    app.socket.onState((s) => s === 'open' && fresh.clear());
    linkCaches.set(app, (links = fresh));
  }
  return links;
}

/** `focusSeq`: scroll to that event once it is loaded and mark it (a search hit, SPEC §15).
 *  `sessions`: a window 900–1079 px wide, "Sessions" at the left of the header. */
export function Chat({
  app,
  sessionId,
  phone = false,
  focusSeq,
  sessions,
}: {
  app: AppState;
  sessionId: string;
  phone?: boolean;
  focusSeq?: number;
  sessions?: SessionsToggle;
}) {
  const chat = useMemo(() => app.chat(sessionId), [app, sessionId]);
  const [notice, setNotice] = useState<string | null>(null);
  const [requestsOnly, setRequestsOnly] = useState(false);
  const [schedules, setSchedules] = useState(false);
  const [moving, setMoving] = useState(false);
  const artifacts = useMemo(() => new ArtifactStore(app.api, undefined, undefined, undefined, sharedLinks(app)), [app]);
  // The image or Markdown text shown in the viewer over the chat.
  const [viewing, setViewing] = useState<ViewedArtifact | null>(null);
  const peerMessage = useMemo(() => peerMessages(app.api), [app]);
  // A new session's window that just became this chat hands over, once, the files attached
  // there and not sent, and the caret in the input.
  const [handed] = useState(() => app.takeHandover(sessionId));
  const [files, setFiles] = useState<OutgoingFile[]>(handed?.files ?? []);
  // The caret goes to the input once, as the chat opens: on a computer, as in the VS Code tab,
  // unless a request waits for an answer; on a phone only from a new session's window (the
  // keyboard would cover the chat). An input shown later (after a sheet, a lock) leaves the
  // focus where it is.
  const caret = useRef(!!handed || !phone);
  const [dragging, setDragging] = useState(false);
  const addFiles = (list: FileList | File[] | null | undefined) => {
    const picked = [...(list ?? [])];
    if (!picked.length) return;
    // One at a time: each is read into memory, and a HEIC drawn on a canvas.
    void (async () => {
      const add: OutgoingFile[] = [];
      for (const p of picked) add.push(await toOutgoing(p));
      setFiles((cur) => [...cur, ...add]);
    })();
  };
  // The feed shows once the newest page is in, in the first frame for a chat kept in memory or
  // loaded ahead; the older history loads behind it.
  const [loaded, setLoaded] = useState(chat.loaded);
  // The feed renders from the block that starts at `from.current` (a `seq`): the last `SHOWN`
  // blocks on open, more as the user scrolls up. Set once, so blocks that come in at either
  // end do not shift what is on screen.
  const from = useRef<number | undefined>(undefined);
  const [, grow] = useState(0);
  useEffect(() => {
    setLoaded(chat.loaded);
    from.current = undefined;
    void chat.open().then(() => setLoaded(true));
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
  // The rules of the session's folder (T48).
  const workspace = info && !system ? app.workspaces.get(info.project_id)?.find((w) => w.id === info.workspace_id) : undefined;
  const rulesHref = workspace && `#/p/${workspace.project_id}/rules/${workspace.id}?session=${encodeURIComponent(sessionId)}`;

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
      app.markRead(sessionId, t.lastSeq);
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
    system,
    folder: rulesHref && workspace
      ? {
          path: homePath(workspace.path),
          rulesHref,
          save: async (rules) => {
            await app.api.addWorkspaceRules(workspace.id, rules);
            void app.loadWorkspaces(workspace.project_id).catch(() => {});
          },
        }
      : undefined,
    hotkeyRequest: firstApproval?.payload.request_id,
    sheetRequest,
    findSession: (ref) => app.findSession(ref),
    peerMessage,
    fork: !system && info?.workspace_id
      ? (e) =>
          void app.forkSession(sessionId, e.event_id, e.payload.text).then(
            (s) => (location.hash = `#/s/${s.id}`),
            (err: Error) => setNotice(`Not forked: ${err.message}`),
          )
      : undefined,
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
    viewArtifact: setViewing,
  };

  const act = (fn: () => Promise<void>, failed: string) =>
    fn().catch((e: Error) => setNotice(`${failed}: ${e.message}`));

  const showRetry = !system && (state === 'error' || t.hasFailedDelivery());
  const items = !loaded ? [] : requestsOnly ? onlyRequests(t.items()) : t.items();
  const blocks = buildFeed(items, t);
  // While the agent's text is being written, the inputs that came meanwhile and the messages
  // not stored yet stand under it (SPEC §6).
  const under = t.streaming ? blocks.splice(underText(blocks, t)) : [];
  if (from.current === undefined && blocks.length) from.current = blockSeq(blocks[Math.max(0, blocks.length - SHOWN)]) ?? 0;
  const start = firstShown(blocks, from.current ?? 0);
  const locked = state === 'locked_vscode';
  // SPEC §9.3: read-only while VS Code has it. Open there right now (`held_by`): it has to be
  // closed there first, the hub refuses takeover (`409 vscode_open`). Written from there
  // recently but not held (or a worker without the registry): "Take over here", which the hub
  // may refuse until 30 s of silence (`409 vscode_active`).
  const held = locked && info?.held_by !== undefined;
  // Where it is open, said once: in the header's pill.
  const holder = info?.held_by && info.held_by.entrypoint !== 'claude-vscode' ? 'Claude Code' : 'VS Code';
  const takeover = () =>
    app.api.takeover(sessionId).then(
      () => {
        setNotice(null);
        return app.reloadSessions();
      },
      (e: unknown) => {
        if (e instanceof ApiError && e.code === 'vscode_open') {
          setNotice(e.message);
          return app.reloadSessions();
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
  // The last scroll position: only a move up lets go of the bottom. A scroll event can come
  // after the feed grew (an image loaded) and before it is followed: far from the bottom then,
  // but the user did not leave it.
  const lastTop = useRef(0);
  // Blocks added above keep what is on screen in place: the distance to the bottom stays.
  const keep = useRef<number | undefined>(undefined);
  const showMore = (seq: number | undefined) => {
    const el = scroller.current;
    if (!el || seq === undefined) return;
    keep.current = el.scrollHeight - el.scrollTop;
    from.current = seq;
    grow((n) => n + 1);
  };
  useLayoutEffect(() => {
    const el = scroller.current;
    if (el && keep.current !== undefined) {
      el.scrollTop = el.scrollHeight - keep.current;
      keep.current = undefined;
    }
    if (el && stick.current) el.scrollTop = el.scrollHeight;
    // A feed shorter than its window cannot be scrolled to the top, so the scroll below never
    // asks for the older blocks: render them until it fills the window or none is left.
    if (el && start > 0 && el.scrollHeight <= el.clientHeight) showMore(blockSeq(blocks[Math.max(0, start - SHOWN)]));
  });
  // Images and code blocks grow after the render, and the window over the feed shrinks when
  // the composer grows with its text or the keyboard opens: stay at the bottom while stuck to it.
  const feed = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = scroller.current;
    if (!el || !feed.current || typeof ResizeObserver === 'undefined') return;
    const o = new ResizeObserver(() => {
      if (stick.current) el.scrollTop = el.scrollHeight;
    });
    o.observe(feed.current);
    o.observe(el);
    return () => o.disconnect();
  }, []);
  // "Quote": text selected in the feed goes into the composer. On a computer it is an item of
  // the client's menu on a right click of the selection (with Copy); on a phone, a button under
  // the selection's end. A press on the button may clear the selection (a phone's tap): the text
  // is kept and the button stays until the click.
  const [quote, setQuote] = useState<FeedSelection | null>(null);
  const [quoteMenu, setQuoteMenu] = useState<{ x: number; y: number; text: string } | null>(null);
  const quoting = useRef(false);
  const insertQuote = useRef<((text: string) => void) | null>(null);
  useEffect(() => {
    // The button is a phone's: a computer quotes from the menu and needs no state per selection.
    if (!phone) return;
    const update = () => {
      if (!quoting.current) setQuote(feedSelection(feed.current));
    };
    const away = (e: Event) => {
      if (!(e.target as Element | null)?.closest?.('.btn-quote')) quoting.current = false;
    };
    document.addEventListener('selectionchange', update);
    document.addEventListener('pointerdown', away, true);
    return () => {
      document.removeEventListener('selectionchange', update);
      document.removeEventListener('pointerdown', away, true);
    };
  }, [phone]);
  // The jump to a search hit: the event is there once `lastSeq` reaches it and the older
  // history (`olderFrom`) is loaded down to it. The block that holds it is the one that starts
  // at it, else the last one that starts at or before it (a hidden event's neighbour; an answer
  // may stand above an earlier input, SPEC §6); the feed renders from there if it did not.
  const focused = useRef<number | undefined>(undefined);
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el || focusSeq === undefined || focused.current === focusSeq || t.lastSeq < focusSeq || t.olderFrom > focusSeq) return;
    if (focusSeq < (from.current ?? 0)) {
      let at = 0;
      blocks.forEach((b, i) => {
        if ((blockSeq(b) ?? Infinity) <= focusSeq) at = i;
      });
      from.current = blockSeq(blocks[at]) ?? 0;
      grow((n) => n + 1);
      return;
    }
    let target = el.querySelector<HTMLElement>(`.feed-block[data-seq="${focusSeq}"]`) ?? undefined;
    if (!target) {
      for (const b of el.querySelectorAll<HTMLElement>('.feed-block[data-seq]')) {
        if (Number(b.dataset.seq) <= focusSeq) target = b;
      }
    }
    if (!target) return;
    focused.current = focusSeq;
    stick.current = false;
    el.querySelector('.feed-block.is-found')?.classList.remove('is-found');
    target.classList.add('is-found');
    target.scrollIntoView?.({ block: 'center' });
  });

  const stop = () => act(() => chat.stop(), 'Not stopped');
  // T38: Ask at once, Auto after a one-line confirmation.
  const setMode = (mode: PermissionMode) => {
    if (mode === 'auto' && !confirm(AUTO_CONFIRM)) return;
    void act(() => app.setPermissionMode(sessionId, mode), 'Not switched');
  };
  const setAgent = (patch: AgentPatch) => void act(() => app.setAgentSettings(sessionId, patch), 'Not changed');
  const usage = t.usageTotals();
  const model = shownModel(info?.model, usage.model);
  const cost = usage.cost;
  // Running, as in the list, while a background subagent or task works after the turn.
  const shown = info && atWorkAfterTurn(info) ? 'running' : state;
  const pill = <StatePill state={shown} timeline={t} where={locked ? `In ${holder}${worker ? ` on ${worker}` : ''}` : undefined} />;
  const agents = agentsLabel(info?.subagents);
  const conn = app.offline && <span class="conn-lost">offline</span>;

  // T50: archived — read-only, no input; restore and delete are on the archive's rows.
  const archived = info?.archived ?? false;
  const disabled = archived || locked;
  const sheet = sheetRequest && firstApproval?.type === 'approval.request' ? firstApproval : undefined;
  const inputShown = !system && !disabled && !sheet;
  // From a new session's window at once; otherwise with the newest page in, so that an open
  // request keeps Enter and Esc for its card (they are a field's while it has the focus). A chat
  // kept in memory may not have the request that came while it was closed: the session's
  // state, live from the hub, says it waits.
  const focusInput = caret.current && (!!handed || (loaded && !firstApproval && state !== 'waiting'));
  useEffect(() => {
    if (loaded) caret.current = false;
  }, [loaded]);
  // A phone's "‹": the session's project.
  const up = info ? `#/p/${info.project_id}` : '#/';
  return (
    <section
      class={`chat ${dragging ? 'chat-dragging' : ''} ${phone ? 'chat-phone' : ''} ${archived ? 'chat-archived' : ''}`}
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
          <a
            class="icon-btn icon-btn-round back"
            href={up}
            aria-label="Back to sessions"
            onClick={(e) => onBack(e, up)}
          >
            <IconBack />
          </a>
        )}
        {sessions && <SessionsButton sessions={sessions} />}
        <div class="chat-head-main">
          <h1 class="chat-title">{info?.title || 'Session'}</h1>
          <div class="chat-meta">
            {pill}
            {agents && (
              <span class="chat-agents" title="Subagents the agent runs now">
                {agents}
              </span>
            )}
            {!system && info && <PermissionMark info={info} />}
            {system && <span class="chat-system-note">the worker's service session</span>}
            {info?.spawned_by && (
              <span class="chat-spawned">
                started by{' '}
                <a class="mono" href={`#/s/${info.spawned_by.session_id}`}>
                  {info.spawned_by.address}
                </a>
              </span>
            )}
            {phone ? (
              // Held elsewhere, the machine is in the pill already.
              <span class="chat-where mono">{[!locked && worker, path && homePath(path).split('/').pop()].filter(Boolean).join(' · ')}</span>
            ) : (
              <>
                {worker && !system && !locked && <span>{worker}</span>}
                {path && (
                  <>
                    {/* Held elsewhere, the worker is in the pill: nothing to part the path from. */}
                    {!locked && <span class="sep">/</span>}
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
            onClick={() => {
              // The other view has blocks of its own: its window starts again at its newest ones.
              from.current = undefined;
              stick.current = true;
              setRequestsOnly(!requestsOnly);
            }}
          >
            Requests only
          </button>
        )}
        {rulesHref &&
          (phone ? (
            <a class="icon-btn icon-btn-round btn-folder-rules" href={rulesHref} aria-label="Folder rules" title="Folder rules">
              <IconShield />
            </a>
          ) : (
            <a class="btn btn-small btn-ghost btn-folder-rules" href={rulesHref} title={`Rules for ${homePath(workspace!.path)}`}>
              <IconShield />
              Folder rules
            </a>
          ))}
        {showRetry && !phone && (
          <button type="button" class="btn btn-small btn-primary btn-retry" onClick={() => act(() => chat.retry(), 'Retry failed')}>
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
            loadLimits={() => app.api.workers().then((ws) => ws.find((w) => w.id === info.worker_id)?.limits ?? undefined)}
            onRetry={showRetry && phone ? () => void act(() => chat.retry(), 'Retry failed') : undefined}
            onSchedules={() => setSchedules(true)}
            onMove={() => setMoving(true)}
            onPermissionMode={setMode}
            onAgent={setAgent}
          />
        )}
      </header>
      {archived && info && (
        <div class="archived-band stripes">
          <IconArchive />
          <span>
            {archivedLabel(info, Date.now())} · read-only. Restore or delete it from {phone ? 'the archive' : 'the list on the left'}.
          </span>
        </div>
      )}
      {schedules && <SchedulesPanel api={app.api} sessionId={sessionId} onClose={() => setSchedules(false)} />}
      {moving && info && (
        <div class="move-panel" role="region" aria-label="Move to project">
          <MoveSession app={app} session={info} onDone={() => setMoving(false)} />
        </div>
      )}
      <div
        class="timeline"
        ref={scroller}
        onScroll={(e) => {
          const el = e.currentTarget as HTMLDivElement;
          const near = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
          if (near || el.scrollTop < lastTop.current) stick.current = near;
          lastTop.current = el.scrollTop;
          if (el.scrollTop < 400 && start > 0) showMore(blockSeq(blocks[Math.max(0, start - SHOWN)]));
          if (quote && phone) setQuote(feedSelection(feed.current));
        }}
      >
        <div
          class="feed"
          ref={feed}
          onContextMenu={(e) => {
            // A right click on text selected in the feed: the client's menu; elsewhere the
            // browser's.
            if (phone || system || disabled || sheet) return;
            const sel = feedSelection(feed.current);
            if (!sel) return;
            e.preventDefault();
            setQuoteMenu({ x: e.clientX, y: e.clientY, text: sel.text });
          }}
        >
          {!loaded && <div class="empty">Loading…</div>}
          <FeedBlocks blocks={start ? blocks.slice(start) : blocks} actions={actions} />
          {requestsOnly && items.length === 0 && <div class="empty">No requests</div>}
          {t.streaming && <StreamingMessage text={t.streaming} />}
          <FeedBlocks blocks={under} actions={actions} />
          {!system && <ActivityLine state={state} timeline={t} />}
          {!system && <WaitedRequests app={app} sessionId={sessionId} />}
        </div>
      </div>
      {notice && (
        <div class="notice" role="alert" onClick={() => setNotice(null)}>
          {notice}
        </div>
      )}
      {(info?.subagents ?? 0) > 0 && <LiveSubagents agents={t.liveAgents} receivedAt={t.liveAgentsAt} />}
      {locked && (
        <div class="composer-wrap">
          {/* In place of the composer it says only what to do; where it is open, the pill. */}
          <div class={`lockbar ${held ? 'lockbar-held' : ''}`}>
            <IconLock />
            <span class="lockbar-text">
              {held ? `Close the session in ${holder} to write from here.` : `${holder} was the last to run this session.`}
            </span>
            {!held && (
              <button type="button" class="btn btn-small btn-primary btn-takeover" onClick={() => void takeover()}>
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
      {quoteMenu && (
        <ContextMenu
          x={quoteMenu.x}
          y={quoteMenu.y}
          label="Selected text"
          items={[
            {
              label: 'Quote',
              icon: <IconQuote />,
              cls: 'menu-quote',
              onSelect: () => {
                insertQuote.current?.(quoteMenu.text);
                document.getSelection()?.removeAllRanges();
              },
            },
            { label: 'Copy', icon: <IconCopy size={16} />, cls: 'btn-copy-selection', onSelect: () => void copyText(quoteMenu.text).catch(() => {}) },
          ]}
          onClose={() => setQuoteMenu(null)}
        />
      )}
      {quote && phone && !system && !disabled && !sheet && (
        <button
          type="button"
          class="btn btn-small btn-quote"
          // Below the end, clear of a phone's selection handle; on screen when the end is not.
          style={{
            left: `${Math.min(Math.max(quote.x, 48), innerWidth - 48)}px`,
            top: `${Math.min(quote.y + 28, innerHeight - 56)}px`,
          }}
          onPointerDown={(e) => {
            quoting.current = true;
            // A mouse press keeps the selection and the focus where they are.
            e.preventDefault();
          }}
          onClick={() => {
            quoting.current = false;
            insertQuote.current?.(quote.text);
            document.getSelection()?.removeAllRanges();
            setQuote(null);
          }}
        >
          Quote
        </button>
      )}
      {inputShown && (
        <Composer
          initial={app.draft(sessionId)}
          onDraft={(text) => app.setDraft(sessionId, text)}
          autoFocus={focusInput}
          insertQuote={insertQuote}
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
      {viewing && (
        <ArtifactViewer
          {...viewing}
          store={artifacts}
          phone={phone}
          onClose={() => setViewing(null)}
          onDownload={() => actions.openArtifact(viewing.id, true)}
        />
      )}
    </section>
  );
}

/** The session column of a window 900–1079 px wide (App): out over the chat or not, what
 *  slides it out or back, and how many of the project's sessions wait for the user. */
export interface SessionsToggle {
  open: boolean;
  waiting: number;
  toggle: () => void;
}

/** "Sessions" at the left of a header in a window 900–1079 px wide: slides the session column
 *  out over the chat, pressed while it is out; the amber count is the rail badge's. */
export function SessionsButton({ sessions }: { sessions: SessionsToggle }) {
  return (
    <button
      type="button"
      class={`btn btn-small btn-sessions ${sessions.open ? 'is-on' : ''}`}
      aria-expanded={sessions.open}
      onClick={sessions.toggle}
    >
      <IconPanel />
      Sessions
      {sessions.waiting > 0 && <span class="sessions-waiting">{sessions.waiting}</span>}
    </button>
  );
}

/** Text selected inside the feed, and the point under its end where "Quote" goes. */
export interface FeedSelection {
  text: string;
  x: number;
  y: number;
}

/** The selection, if it is text inside `feed`; the edges of the text are trimmed. */
export function feedSelection(feed: HTMLElement | null): FeedSelection | null {
  const sel = feed?.ownerDocument.getSelection();
  if (!feed || !sel || sel.isCollapsed || sel.rangeCount === 0) return null;
  const range = sel.getRangeAt(0);
  if (!feed.contains(range.commonAncestorContainer)) return null;
  const text = sel.toString().trim();
  if (!text) return null;
  const rects = range.getClientRects();
  const end = rects.length ? rects[rects.length - 1] : range.getBoundingClientRect();
  return { text, x: end.right, y: end.bottom };
}

/** The draft with `quote` after it as `> ` lines (a blank line between them when the draft
 *  has text) and a blank line after, where the reply goes. */
export function withQuote(draft: string, quote: string): string {
  const lines = quote
    .split(/\r?\n/)
    .map((l) => (l.trim() ? `> ${l}` : '>'))
    .join('\n');
  const head = draft.trimEnd();
  return `${head ? `${head}\n\n` : ''}${lines}\n\n`;
}

/** The `tool.call` an approval card shows (feed.ts), for the sheet. */
function approvalCall(items: ReturnType<Timeline['items']>, t: Timeline, requestId: string) {
  const b = buildFeed(items, t).find((x) => x.kind === 'approval' && x.event.payload.request_id === requestId);
  return b?.kind === 'approval' ? b.call : undefined;
}

/** The state in the header: a pill, pulsing while the session works (a turn, or a background
 *  subagent after it). `where`: a session VS Code has ("In VS Code on <machine>"), marked `</>`. */
function StatePill({ state, timeline, where }: { state: SessionState; timeline: Timeline; where?: string }) {
  let label = where ?? STATE_LABELS[state];
  if (state === 'waiting') {
    const r = timeline.openRequests()[0];
    label = r?.type === 'approval.request' ? 'Waiting for permission' : 'Waiting for reply';
  }
  return (
    <span class={`state pill state-${state}`}>
      {where ? <IconCode size={12} /> : <span class={`pill-dot ${state === 'running' ? 'pulse' : ''}`} aria-hidden="true" />}
      {label}
    </span>
  );
}

/** The session's permission mode (T38): what the worker reports the agent runs in, else
 *  what hqc asked for. "Auto" stands out: nothing but the worker's deny list stops a call.
 *  A turn capped by a message from a session in Ask (T39) shows both: Ask for this turn,
 *  named by that session, and the session's own Auto after it. */
export function PermissionMark({ info }: { info: SessionInfo }) {
  const runs = info.effective_permission_mode;
  const asked = info.permission_mode;
  const by = info.permission_capped_by;
  if (by) {
    return (
      <span
        class="perm-ask perm-capped"
        title={`This turn runs in Ask: it was started by a message along a chain with ${by}, which runs in Ask, and a message cannot raise permissions. The session's own mode, Auto, comes back when the turn ends.`}
      >
        Ask (asked by {by})<span class="perm-next"> · Auto after this turn</span>
      </span>
    );
  }
  if (runs === 'auto' || (!runs && asked === 'auto')) {
    return (
      <span class="perm-auto" title="Claude Code decides tool calls itself; only the worker's deny list stops one">
        Auto
      </span>
    );
  }
  if (asked === 'auto') {
    return (
      <span class="perm-ask" title="The worker runs this session in Ask: until the agent next runs, or for good if the worker does not allow Auto">
        Ask (Auto asked)
      </span>
    );
  }
  if (!runs && !asked) return null;
  return (
    <span class="perm-ask" title="Tool calls outside the worker's rules are asked about">
      Ask
    </span>
  );
}

/** The mode the menu switches from: hqc's choice, else what the agent runs in. */
export function currentMode(info: SessionInfo): PermissionMode {
  return info.permission_mode ?? info.effective_permission_mode ?? 'ask';
}

type AgentPatch = Pick<SessionPatch, 'model'>;

/** "More": schedules, the move to another project, the permission mode and the model; on
 *  a phone also the path, the model of the last turn and the cost. Archiving is on the
 *  list's rows, restore and delete on the archive's (T50). */
function MoreMenu({
  info,
  phone,
  path,
  model,
  cost,
  onRetry,
  onSchedules,
  onMove,
  onPermissionMode,
  onAgent,
  loadLimits,
}: {
  info: SessionInfo;
  phone: boolean;
  path?: string;
  model?: string;
  cost?: number;
  /** On a phone "Retry" lives here, not in the crowded header. */
  onRetry?: () => void;
  onSchedules: () => void;
  /** "Move to project…": the session continues in another folder and project. */
  onMove: () => void;
  onPermissionMode: (mode: PermissionMode) => void;
  /** A change PATCHes at once; `null` is the runtime's default. */
  onAgent: (patch: AgentPatch) => void;
  /** The usage limits of the session's worker account, read when the menu opens. */
  loadLimits: () => Promise<WorkerLimits | undefined>;
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
          {item('btn-schedules', 'Schedules', onSchedules, 'Messages scheduled into this session')}
          {!info.archived &&
            item('btn-move', 'Move to project…', onMove, 'Continue this session in another folder and project on its worker')}
          {!info.archived &&
            (currentMode(info) === 'auto'
              ? item('btn-mode-ask', 'Switch to Ask', () => onPermissionMode('ask'), 'Ask before tool calls outside the worker\'s rules')
              : item(
                  'btn-mode-auto',
                  'Switch to Auto',
                  () => onPermissionMode('auto'),
                  "Claude Code decides tool calls itself; the worker's deny list is the only limit",
                ))}
          {!info.archived && (
            <div class="menu-settings">
              <ModelField value={info.model} onChange={(m) => onAgent({ model: m ?? null })} />
              <Limits load={loadLimits} />
            </div>
          )}
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

/** A file the hub takes at most (SPEC §12, `MAX_ARTIFACT_BYTES`): a larger one is not read
 *  into memory. */
const HELD_MAX = 100 * 1000 * 1000;
/** iOS draws a canvas of at most this many pixels; a larger photo is scaled down to it. */
const CANVAS_MAX = 16_000_000;
const HEIC_NAME = /\.hei[cf]$/i;

/** A picked file, read into memory at once, with a `data:` preview for images (the CSP has
 *  no `blob:`). On an iPhone a HEIC file whose preview was read in full still went out cut
 *  off when the upload read it again ("incomplete multipart stream"), so the upload sends
 *  what was read here.
 *  A HEIC or HEIF image becomes a JPEG where the browser decodes it (Safari, any browser on an
 *  iPhone): the agent does not read HEIC. */
export async function toOutgoing(file: File): Promise<OutgoingFile> {
  let f: OutgoingFile = { file, name: file.name || 'file' };
  if (file.size <= HELD_MAX) {
    try {
      f.file = new Blob([await file.arrayBuffer()], { type: file.type });
    } catch {
      // Not readable: the upload fails and says so.
      return f;
    }
    if (/^image\/hei[cf]/i.test(file.type) || HEIC_NAME.test(f.name)) f = (await toJpeg(f)) ?? f;
  }
  if (!f.file.type.startsWith('image/') || f.file.size > PREVIEW_MAX || typeof FileReader === 'undefined') return f;
  return { ...f, preview: await dataUrl(f.file) };
}

/** `f` drawn as a JPEG, or null where the browser does not decode it. */
async function toJpeg(f: OutgoingFile): Promise<OutgoingFile | null> {
  try {
    const src = await dataUrl(f.file);
    if (!src) return null;
    const img = new Image();
    img.src = src;
    await img.decode();
    const scale = Math.min(1, Math.sqrt(CANVAS_MAX / (img.naturalWidth * img.naturalHeight)));
    const c = document.createElement('canvas');
    c.width = Math.floor(img.naturalWidth * scale);
    c.height = Math.floor(img.naturalHeight * scale);
    c.getContext('2d')!.drawImage(img, 0, 0, c.width, c.height);
    const jpeg = await new Promise<Blob | null>((resolve) => c.toBlob(resolve, 'image/jpeg', 0.9));
    // iOS frees a canvas's memory only when it is collected.
    c.width = c.height = 0;
    return jpeg ? { file: jpeg, name: `${f.name.replace(HEIC_NAME, '')}.jpg` } : null;
  } catch {
    return null;
  }
}

function dataUrl(b: Blob): Promise<string | undefined> {
  if (typeof FileReader === 'undefined') return Promise.resolve(undefined);
  return new Promise((resolve) => {
    const r = new FileReader();
    r.onload = () => resolve(typeof r.result === 'string' ? r.result : undefined);
    r.onerror = () => resolve(undefined);
    r.readAsDataURL(b);
  });
}

export function Composer({
  onSend,
  placeholder,
  phone = false,
  commands,
  files = [],
  onFiles = () => {},
  onRemove = () => {},
  insertQuote,
  initial = '',
  onDraft,
  autoFocus = false,
  wait,
}: {
  onSend: (text: string) => void;
  placeholder?: string;
  phone?: boolean;
  /** The runtime's slash commands: `/` at the start of the text opens a list of them. */
  commands?: string[];
  files?: OutgoingFile[];
  onFiles?: (files: FileList | File[]) => void;
  onRemove?: (index: number) => void;
  /** Set here to a function that puts a quote of the feed into the message (`withQuote`),
   *  focused, the caret at its end. */
  insertQuote?: { current: ((text: string) => void) | null };
  /** The text not sent last time (a draft), and where every change of it goes. */
  initial?: string;
  onDraft?: (text: string) => void;
  /** The caret in the field when it is shown with this set, or when this turns on. */
  autoFocus?: boolean;
  /** Sending waits for this (a new session being created): send is disabled with it as its
   *  title, Enter sends nothing, the text stays. */
  wait?: string;
}) {
  const [text, setText] = useState(initial);
  const typed = useRef(false);
  useEffect(() => {
    // Not the draft it opened with: that one is kept already.
    if (typed.current) onDraft?.(text);
    typed.current = true;
  }, [text]);
  const area = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    if (!autoFocus) return;
    // Not away from where the user went while the chat loaded: another field (the project
    // search), a menu, the session column slid out over the chat.
    const doc = area.current?.ownerDocument;
    const at = doc?.activeElement as HTMLElement | null | undefined;
    const field = at && at !== area.current && (at.tagName === 'INPUT' || at.tagName === 'TEXTAREA' || at.tagName === 'SELECT' || at.isContentEditable);
    if (field || doc?.querySelector('[role="menu"], .sidebar-slide')) return;
    area.current?.focus();
  }, [autoFocus]);
  const caretToEnd = useRef(false);
  useEffect(() => {
    if (!insertQuote) return;
    insertQuote.current = (q) => {
      caretToEnd.current = true;
      setText((cur) => withQuote(cur, q));
      area.current?.focus();
    };
    return () => {
      insertQuote.current = null;
    };
  }, [insertQuote]);
  const picker = useRef<HTMLInputElement>(null);
  const canSend = !wait && (text.trim() !== '' || files.length > 0);
  // The slash command popup (T27): the matches, the highlighted one, and the text Esc closed
  // it at (typing on reopens it).
  const [pick, setPick] = useState(0);
  const [closedAt, setClosedAt] = useState<string | null>(null);
  const matches = closedAt === text ? [] : matchCommands(commands, text);
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
    // The form keeps its height while the field is measured at one line: a composer shorter
    // for that moment let the feed above clamp its scroll, and the feed's bottom went under the
    // composer on every key.
    const form = el.form;
    if (form) form.style.minHeight = `${form.offsetHeight}px`;
    // One line keeps the stylesheet's height, which follows the look (applied after this first
    // runs); more lines set it.
    el.style.height = '';
    if (el.scrollHeight > el.clientHeight) el.style.height = `${Math.min(el.scrollHeight, 200)}px`;
    if (form) form.style.minHeight = '';
    if (caretToEnd.current) {
      caretToEnd.current = false;
      el.setSelectionRange(el.value.length, el.value.length);
      el.scrollTop = el.scrollHeight;
    }
  }, [text]);
  const attach = (
    <button
      type="button"
      class="icon-btn btn-attach"
      aria-label="Attach files"
      title="Attach files"
      onClick={() => picker.current?.click()}
    >
      <IconAttach />
    </button>
  );
  const send = (
    <button type="submit" class="btn-send" aria-label="Send" title={wait ?? 'Send'} disabled={!canSend}>
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
      rows={1}
      placeholder={placeholder ?? (phone ? 'Message' : 'Message · Enter to send, Shift+Enter for a new line')}
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
        // A press on a button of the composer (send, attach, remove a file) leaves the focus in
        // the field: on a phone the field losing it drops the keyboard and the composer grows by
        // the home indicator's room at once, and the tap's click then lands under the button.
        onMouseDown={(e) => {
          if ((e.target as Element).closest('button')) e.preventDefault();
        }}
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
        {/* One row: attach, the field (growing with its lines), send. */}
        <div class="composer-row">
          {attach}
          {textarea}
          {send}
        </div>
      </form>
    </div>
  );
}

/** Index of the first block to render: the first that starts at or after `from`. Blocks
 *  without a `seq` (messages not stored yet) come last and always render. */
function firstShown(blocks: ReturnType<typeof buildFeed>, from: number): number {
  const i = blocks.findIndex((b) => (blockSeq(b) ?? Infinity) >= from);
  return i < 0 ? blocks.length : i;
}

/** Index of the first of the last blocks that are inputs come while the text being written now
 *  was (`during_text`, stored after the last event that ended a text) or messages not stored
 *  yet. */
function underText(blocks: ReturnType<typeof buildFeed>, t: Timeline): number {
  const evs = t.events();
  let ended = 0;
  for (let i = evs.length - 1; i >= 0; i--) {
    const e = evs[i];
    if (e.type === 'agent.message' || (e.type === 'status' && e.payload.state !== 'running')) {
      ended = e.seq!;
      break;
    }
  }
  let i = blocks.length;
  while (i > 0) {
    const b = blocks[i - 1];
    if (b.kind !== 'item') break;
    const it = b.item;
    if (!(it.kind === 'pending' || (it.kind === 'event' && duringText(it) && it.event.seq! > ended))) break;
    i--;
  }
  return i;
}
