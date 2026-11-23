import type { ComponentChildren } from 'preact';
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'preact/hooks';
import { ApiError, type SecretFields, type SecretGiven, type SessionPatch, type WorkerLimits } from '../api';
import type { AppState } from '../app';
import { ArtifactStore, formatSize, type LinkCache } from '../artifacts';
import { HEIC_NAME, dataUrl, heicToJpeg } from '../heic';
import { onBack } from '../nav';
import { PERMISSION_MODES, type EventOf, type ModelProvider, type PermissionMode, type SessionInfo, type SessionState, type SlashCommand } from '../proto';
import type { OutgoingFile } from '../session';
import { duringText, type Timeline } from '../timeline';
import { ModelName, isTypedModel, type ModelChoice, modelMenuItems } from './agentSettings';
import { ArtifactViewer, type ViewedArtifact } from './attachments';
import { LimitBar, Limits } from './limits';
import { ActivityLine, FeedBlocks, activityShown, StreamingMessage, blockSeq, onlyRequests, type ChatActions } from './cards';
import { peerMessages } from './relay';
import { buildFeed } from './feed';
import { useSubscription } from './hooks';
import { ContextMenu, type MenuEntry } from './ContextMenu';
import { ComposerSecret } from './secrets';
import {
  IconArchive,
  IconAttach,
  IconBolt,
  IconChevronDown,
  IconKey,
  IconBack,
  IconCode,
  IconLock,
  IconMore,
  IconPanel,
  IconPause,
  IconPlay,
  IconRename,
  IconSend,
  IconStop,
} from './icons';
import { MODE_LABELS, ROOM_MODE_LABELS, STATE_LABELS, agentsLabel, formatCost, homePath, modeConfirm, modelName, shownModel } from './labels';
import { LiveSubagents } from './LiveSubagents';
import { Loading } from './Loading';
import { CopyButton } from './Markdown';
import { SelectionContext, useChatSelection } from './selection';
import { PermissionBlock, SECRET_GIVEN } from './permission';
import { MoveSession } from './ProjectSettings';
import { WaitedRequests } from './requests';
import { SchedulesPanel } from './Schedules';
import { archivedLabel, atWorkAfterTurn, canPause, pausingAfterTurn, restartableSession } from './sessions';

const STOPPABLE = new Set(['running', 'waiting', 'queued']);
/** Blocks of the feed rendered on open; scrolling to the top adds `SHOWN` more. */
const SHOWN = 60;
/** How far back from the newest event the header's `usage` is looked for in the older history. */
const USAGE_DEPTH = 1000;
/** How long the user's own scroll counts as still going after its last input, or after the last
 *  scroll event that came while it went: a trackpad's momentum sends wheel events to its end, a
 *  finger's fling and a bounce at an edge move the feed with scroll events only. */
const SCROLL_SETTLE_MS = 400;
/** Keys that scroll the feed when the focus is not in a field. */
const SCROLL_KEYS = new Set(['ArrowUp', 'ArrowDown', 'PageUp', 'PageDown', 'Home', 'End', ' ']);
/** Of them, the ones that scroll up (and Shift with the space). */
const UP_KEYS = new Set(['ArrowUp', 'PageUp', 'Home']);
/** WebKit as in Safari, the macOS app and every browser on an iPhone applies the steps of the
 *  user's scroll (a wheel gesture, a finger and its fling) to the position the feed had and
 *  drops one the page set in between; Chromium applies them to the new one. */
const losesScrollInGesture = () => {
  const ua = navigator.userAgent;
  return /AppleWebKit/.test(ua) && !/Chrome\/|Chromium\/|Edg\//.test(ua);
};

/** The signed links of all chats of the page, one map: a chat opened again gets the same links
 *  while they live, and the browser shows its images from its cache. Emptied whenever the
 *  hub's socket opens: a hub restarted without `HQC_ARTIFACT_KEY` signs with a new key. The
 *  metadata stays per chat opened (a page unpublished elsewhere shows so on the next open). */
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
  const artifacts = useMemo(() => new ArtifactStore(app.api, undefined, undefined, undefined, sharedLinks(app), app.artifactCache), [app]);
  // The image or Markdown text shown in the viewer over the chat.
  const [viewing, setViewing] = useState<ViewedArtifact | null>(null);
  const peerMessage = useMemo(() => peerMessages(app.api), [app]);
  // A new session's window that just became this chat hands over, once, the files attached
  // there and not sent, and the caret in the input.
  const [handed] = useState(() => app.takeHandover(sessionId));
  const [files, setFiles] = useState<OutgoingFile[]>(handed?.files ?? []);
  // Files picked and not read yet: a HEIC libheif decodes takes a second or more, and a message
  // sent meanwhile would go without it.
  const [reading, setReading] = useState(0);
  // The caret goes to the input once, as the chat opens: on a computer, as in the VS Code tab,
  // unless a request waits for an answer; on a phone only from a new session's window (the
  // keyboard would cover the chat). An input shown later (after a sheet, a lock) leaves the
  // focus where it is.
  const caret = useRef(!!handed || !phone);
  const [dragging, setDragging] = useState(false);
  const addFiles = (list: FileList | File[] | null | undefined) => {
    const picked = [...(list ?? [])];
    if (!picked.length) return;
    setReading((n) => n + picked.length);
    // One at a time: each is read into memory, and a HEIC decoded into a JPEG; each shows
    // as soon as it is read.
    void (async () => {
      for (const p of picked) {
        const f = await toOutgoing(p);
        setFiles((cur) => [...cur, f]);
        setReading((n) => n - 1);
      }
    })();
  };
  // The feed shows once the newest page is in (or the copy of it the device keeps), in the first
  // frame for a chat kept in memory or loaded ahead; the older history loads when asked for.
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
  const project = info && app.project(info.project_id);
  const state = info?.state ?? 'unknown';
  // The worker's service session (SPEC §10.2): requests and log lines, no input.
  const system = info?.system ?? false;
  const t = chat.timeline;
  // The state comes over the socket, late after an answer of this client: the request is
  // closed already, and the agent is not waiting for it.
  const answeredState = state === 'waiting' && t.answeredLocally() && t.openRequests().length === 0 ? 'running' : state;
  // A room's agents: their chats stay open while the room's is, for the running command each one's
  // activity line names.
  const room = info?.room_mode;
  // It has no pills of its own: its mode stands in the meta row after the folder (on a phone,
  // after the state), and its agents' sessions go under the line.
  const members = room ? app.roomMembers(sessionId) : [];
  const memberIds = members.map((m) => m.id).join(' ');
  const memberChats = useMemo(() => (memberIds ? memberIds.split(' ').map((id) => app.chat(id)) : []), [app, memberIds]);
  useEffect(() => {
    for (const c of memberChats) void c.open();
    return () => memberChats.forEach((c) => c.close());
  }, [memberChats]);
  useSubscription((fn) => {
    const offs = memberChats.map((c) => c.timeline.onChange(fn));
    return () => offs.forEach((off) => off());
  }, [memberChats]);

  // The folder of the session, for the header and the requests' "outside the working folder".
  useEffect(() => {
    if (info && !system && !app.workspaces.has(info.project_id)) app.loadWorkspaces(info.project_id).catch(() => {});
  }, [app, info?.project_id, system]);
  const path = info && !system ? app.sessionPath(info) : undefined;
  // The rules of the session's folder (T48).
  const workspace = info && !system ? app.workspaces.get(info.project_id)?.find((w) => w.id === info.workspace_id) : undefined;
  const rulesHref = workspace && `#/p/${workspace.project_id}/rules/${workspace.id}?session=${encodeURIComponent(sessionId)}`;

  // The runtime's slash commands come with the session object (T27), which the summary does
  // not refresh, and with what the runtime says of each only in the hub's answer about this
  // one session, not in its lists: fetch it when an idle session has no list yet (after its
  // first process) or came from a list.
  const fetchCommands = !system && info !== undefined && info.command_info === undefined && state === 'idle';
  useEffect(() => {
    if (fetchCommands) void app.refreshSession(sessionId);
  }, [app, sessionId, fetchCommands]);
  const worker = app.workerName(info?.worker_id);
  // The usage limits of the session's worker account (SPEC §6), for the bar above the input
  // and the model pill's menu.
  const loadLimits = async () => (await app.api.workers()).find((w) => w.id === info?.worker_id)?.limits ?? undefined;
  // From which share of a window the bar shows: the hub's setting, 70 % from a hub before it and
  // until /api/me is in.
  const limitPercent = app.me?.limit_notice_percent ?? 70;

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

  // A message taken back (SPEC 0.219) returns to the input: its text after what is typed there
  // (the draft when the input is not shown, under a request's sheet), its files read again from
  // the device when it holds them, else from the hub, and not kept on the device. Sending waits
  // while they are read.
  const putBack = (e: EventOf<'user.message'>) => {
    const text = e.payload.text;
    if (text) {
      if (insertText.current) insertText.current(text);
      else app.setDraft(sessionId, withText(app.draft(sessionId), text));
    }
    const ids = e.payload.artifact_ids;
    if (!ids.length) return;
    setReading((n) => n + ids.length);
    void (async () => {
      let lost = 0;
      for (const id of ids) {
        try {
          const [meta, blob] = await Promise.all([artifacts.meta(id), artifacts.blob(id, false)]);
          const f = await toOutgoing(new File([blob], meta.name, { type: meta.mime }));
          setFiles((cur) => [...cur, f]);
        } catch {
          lost++;
        }
        setReading((n) => n - 1);
      }
      if (lost) setNotice(`${lost} attached file${lost > 1 ? 's' : ''} could not be put back in the input`);
    })();
  };
  const open = t.openRequests();
  const firstApproval = open.find((r) => r.type === 'approval.request');
  const sheetRequest = phone && !requestsOnly ? firstApproval?.payload.request_id : undefined;
  const actions: ChatActions = {
    timeline: t,
    // Answered here: the Requests list drops it now, as when answered there (`App.closing`).
    approve: async (r, o, m) => {
      await chat.approve(r, o, m);
      app.dropRequest({ session_id: sessionId, request_id: r });
    },
    answer: async (r, a, x) => {
      await chat.answer(r, a, x);
      app.dropRequest({ session_id: sessionId, request_id: r });
    },
    // Only where the input is: the hub refuses one of an archived session or one VS Code holds.
    send:
      !system && !info?.archived && state !== 'locked_vscode'
        ? (text) => {
            chat.send(text);
          }
        : undefined,
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
    roomAgent: room
      ? (id) => {
          const i = members.findIndex((m) => m.id === id);
          return i < 0 ? undefined : { index: i, name: members[i].title || undefined, model: members[i].model };
        }
      : undefined,
    peerMessage,
    giveSecret: !system && app.takesSecrets(info?.worker_id)
      ? async (requestId, f) => {
          await app.api.giveSecret(sessionId, f, requestId);
          t.markAnswered(requestId, { request_id: requestId, option_id: SECRET_GIVEN });
          app.dropRequest({ session_id: sessionId, request_id: requestId });
        }
      : undefined,
    fork: !system && info?.workspace_id
      ? (e) =>
          void app.forkSession(sessionId, e.event_id, e.payload.text).then(
            (s) => {
              if (s) location.hash = `#/s/${s.id}`;
            },
            (err: Error) => setNotice(`Not forked: ${err.message}`),
          )
      : undefined,
    forking: (eventId) => app.forking(eventId),
    // Quote only where the input is (as `send`, and not under a request's sheet, which takes its place).
    quote:
      !system && !info?.archived && state !== 'locked_vscode' && !sheetRequest
        ? (text) => insertQuote.current?.(text)
        : undefined,
    webSearch: app.webSearch,
    // Where the input is (as `send`; a room's messages are not queued for an agent of the room's own).
    withdraw:
      !system && !room && !info?.archived && state !== 'locked_vscode'
        ? async (e) => {
            try {
              await chat.withdraw(e);
            } catch (err) {
              // Taken back already (another device, or an earlier try whose answer was lost):
              // its text comes back as on success.
              if (err instanceof ApiError && err.code === 'withdrawn') {
                putBack(e);
                return;
              }
              setNotice(
                err instanceof ApiError && err.code === 'already_taken'
                  ? 'The agent already took this message'
                  : `Not taken back: ${(err as Error).message}`,
              );
              return;
            }
            putBack(e);
          }
        : undefined,
    // Only while a turn runs: with none there is nothing to take the message into.
    sendNow:
      !system && !room && !info?.archived && state !== 'locked_vscode' && answeredState === 'running'
        ? async (e) => {
            try {
              if (!(await chat.sendNow(e.event_id))) setNotice('Not sent now: the agent took it already, or it goes next');
            } catch (err) {
              setNotice(
                err instanceof ApiError && err.code === 'not_now'
                  ? 'The agent cannot take this message into its turn now'
                  : `Not sent now: ${(err as Error).message}`,
              );
            }
          }
        : undefined,
    // As the composer's list: not in the service session, which has no composer.
    quickMessages: project && !system
      ? {
          has: (text) => (project.quick_messages ?? []).includes(text),
          add: (text) => app.addQuickMessage(project.id, text).catch((err: Error) => setNotice(`Not added to quick messages: ${err.message}`)),
        }
      : undefined,
    // Where the header has the model pill (SPEC 0.168; a room has none), and not while VS Code
    // holds the session: its message cannot be sent again from here.
    modelSwitch:
      !system && !room && info && !info.archived && state !== 'locked_vscode'
        ? {
            model: info.model,
            provider: info.provider?.id,
            providers: app.providersFor(info.worker_id),
            to: (c) => setAgent(agentPatch(info, c)),
          }
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
  // not stored yet stand under it (SPEC §6), and so does the thinking being written.
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
  // The feed's scroll range (`scrollHeight - clientHeight`) at that position: a move up that is
  // not the user's own scroll counts only if the range stayed. A feed that got shorter (the
  // names of images giving way to the images not loaded yet) moves the position up by itself,
  // and WebKit delivers that scroll after the images loaded and the feed grew again, far from
  // the bottom.
  const lastRange = useRef(0);
  // The user's own scroll: a wheel or trackpad, a finger on the feed (`touching`) or a pointer
  // pressed on the feed's own element, its scrollbar (`barHeld`), while they are down, a key,
  // and the momentum and bounce after them, for as long as scroll events follow within
  // SCROLL_SETTLE_MS. A move up in it lets go of the bottom whatever the range did when the
  // user scrolls up (`upward`, the direction of its last input): an answer being written grows the
  // feed under it. A scroll down keeps the range rule, so a move the browser makes as images load
  // does not let go of the bottom under it.
  const userAt = useRef(-Infinity);
  const touching = useRef(false);
  const barHeld = useRef<number | undefined>(undefined);
  const upward = useRef(false);
  const userScrolling = () =>
    touching.current || barHeld.current !== undefined || performance.now() - userAt.current < SCROLL_SETTLE_MS;
  // While it goes on, the chat does not follow the bottom: Chromium drops the rest of a smooth
  // wheel scroll the page set a position in, so an answer being written took each scroll up back
  // down before its scroll event came. In WebKit the chat sets no position at all: WebKit
  // applies the scroll's next step to the position it had, so a position set in between is lost.
  // The older blocks rendered above a feed scrolled near its top then landed with their height
  // lost, thousands of pixels above where the user was. What the scroll held up (`more`; a
  // render or resize that would have followed the bottom, `heldBack`) is done once it settles,
  // on the render the settle asks for.
  const holding = () => losesScrollInGesture() && userScrolling();
  const more = useRef(false);
  const heldBack = useRef(false);
  // The scroll to the top asked for an older page that has not been shown yet: its blocks show as
  // `showMore`'s do once it is in, if the feed is still near its top.
  const asked = useRef(false);
  const settle = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  /** `goesUp`: the input's direction; a scroll event without an input keeps the last one. */
  const userScrolled = (goesUp?: boolean) => {
    if (goesUp !== undefined) upward.current = goesUp;
    userAt.current = performance.now();
    clearTimeout(settle.current);
    // The timer is the end of the scroll (a later input or scroll event would have reset it),
    // even if it fires a fraction of a millisecond before `performance.now()` says so.
    settle.current = setTimeout(() => {
      // Still down: the release starts the timer again.
      if (touching.current || barHeld.current !== undefined) return;
      userAt.current = -Infinity;
      // The next scroll gives its own direction with its first input.
      upward.current = false;
      // A render with nothing held would pull a feed scrolled a little up, still counted at
      // its bottom, back down.
      if (!heldBack.current && !more.current) return;
      heldBack.current = false;
      grow((n) => n + 1);
    }, SCROLL_SETTLE_MS);
  };
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
    if (!el) return;
    // Set by a `showMore` of this same task, before any input could come in between.
    if (keep.current !== undefined) {
      el.scrollTop = el.scrollHeight - keep.current;
      keep.current = undefined;
    }
    if (holding()) {
      heldBack.current = true;
      return;
    }
    if (stick.current) {
      if (userScrolling()) heldBack.current = true;
      else el.scrollTop = el.scrollHeight;
    }
    const wanted = more.current;
    more.current = false;
    // A feed shorter than its window cannot be scrolled to the top, so the scroll below never
    // asks for the older blocks: render them, and load the older history behind them, until it
    // fills the window or none is left.
    if (start > 0 && (el.scrollHeight <= el.clientHeight || ((wanted || asked.current) && el.scrollTop < 400))) {
      showMore(blockSeq(blocks[Math.max(0, start - SHOWN)]));
    } else if (loaded && start === 0 && t.olderFrom > 0 && el.scrollHeight <= el.clientHeight) {
      // `loaded`: the newest page is in the timeline a render before it is drawn, and the feed,
      // still empty then, is no shorter than its window.
      void chat.loadOlder(undefined, true);
    }
    // The page the scroll to the top asked for may add no block (its events all joined the top
    // group of calls, so `start` stays 0); the user then sits at the top with no further scroll
    // event to ask again: ask for the next page.
    if (asked.current && start === 0 && !chat.loadingOlder && t.olderFrom > 0 && el.scrollTop < 400) {
      void chat.loadOlder(undefined, true);
    } else if (start > 0 || !chat.loadingOlder) asked.current = false;
  });
  // Behind the newest page, the header's cost and model come from a `usage` event and the cards
  // of the session's open requests from their events: older pages load, one after another, until
  // they are in. A turn ends with a `usage`, so it is looked for only `USAGE_DEPTH` events back
  // (a session with none, as a worker's service session, would load its whole history).
  const needsUsage =
    loaded &&
    t.olderFrom > 0 &&
    !system &&
    !room &&
    t.olderFrom > t.lastSeq - USAGE_DEPTH &&
    !t.events().some((e) => e.type === 'usage');
  // Whatever its kind (`publish`, `move`, `worker_approval`…), the request's own event stands
  // in the session, and its card comes from there: the pages go down to the oldest of them.
  const requestSeqs = loaded
    ? app.requests
        .filter((r) => r.session_id === sessionId && (r.event.seq ?? 0) < t.olderFrom)
        .map((r) => r.event.seq ?? 0)
    : [];
  const requestsDownTo = requestSeqs.length ? Math.min(...requestSeqs) : undefined;
  useEffect(() => {
    // The page that was missing may have landed since the render.
    if ((needsUsage || requestsDownTo !== undefined) && t.olderFrom > 0) void chat.loadOlder(requestsDownTo, true);
  });
  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const wheel = (e: WheelEvent) => userScrolled(e.deltaY ? e.deltaY < 0 : undefined);
    // A finger: its touch events, not its pointer events, as a browser cancels the pointer once
    // the touch turns into a scroll. The end is taken on the window, where it comes wherever the
    // finger was lifted; one that went to a node removed meanwhile is made up by the next touch.
    let fingerY: number | undefined;
    const touchStart = (e: TouchEvent) => {
      touching.current = true;
      fingerY = e.touches[0]?.clientY;
      userScrolled();
    };
    // The finger moving down scrolls the feed up.
    const touchMove = (e: TouchEvent) => {
      const y = e.touches[0]?.clientY;
      if (y !== undefined && fingerY !== undefined && y !== fingerY) userScrolled(y > fingerY);
      fingerY = y;
    };
    const touchEnd = (e: TouchEvent) => {
      if (!touching.current || e.touches.length) return;
      touching.current = false;
      userScrolled();
    };
    // The main button pressed on the feed's own element: its scrollbar (and its padding and the
    // margins beside the column, which scroll nothing). Its direction is not known: taken as up.
    const pointerDown = (e: PointerEvent) => {
      if (e.target !== el || e.pointerType === 'touch' || e.button !== 0 || e.ctrlKey) return;
      barHeld.current = e.pointerId;
      userScrolled(true);
    };
    const pointerUp = (e: PointerEvent) => {
      if (e.pointerId !== barHeld.current) return;
      barHeld.current = undefined;
      userScrolled();
    };
    const key = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null;
      if (!SCROLL_KEYS.has(e.key) || t?.closest('input, textarea, select, [contenteditable]')) return;
      userScrolled(UP_KEYS.has(e.key) || (e.key === ' ' && e.shiftKey));
    };
    // Passive: a wheel or touch listener that may cancel would take the scroll off the
    // browser's scrolling thread.
    el.addEventListener('wheel', wheel, { passive: true });
    el.addEventListener('touchstart', touchStart, { passive: true });
    el.addEventListener('touchmove', touchMove, { passive: true });
    window.addEventListener('touchend', touchEnd, true);
    window.addEventListener('touchcancel', touchEnd, true);
    el.addEventListener('pointerdown', pointerDown);
    window.addEventListener('pointerup', pointerUp);
    window.addEventListener('pointercancel', pointerUp);
    window.addEventListener('keydown', key);
    return () => {
      clearTimeout(settle.current);
      el.removeEventListener('wheel', wheel);
      el.removeEventListener('touchstart', touchStart);
      el.removeEventListener('touchmove', touchMove);
      window.removeEventListener('touchend', touchEnd, true);
      window.removeEventListener('touchcancel', touchEnd, true);
      el.removeEventListener('pointerdown', pointerDown);
      window.removeEventListener('pointerup', pointerUp);
      window.removeEventListener('pointercancel', pointerUp);
      window.removeEventListener('keydown', key);
    };
  }, []);
  // Images and code blocks grow after the render, and the window over the feed shrinks when
  // the composer grows with its text or the keyboard opens: stay at the bottom while stuck to it.
  const feed = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = scroller.current;
    if (!el || !feed.current || typeof ResizeObserver === 'undefined') return;
    const o = new ResizeObserver(() => {
      if (!stick.current) return;
      if (userScrolling()) heldBack.current = true;
      else el.scrollTop = el.scrollHeight;
    });
    o.observe(feed.current);
    o.observe(el);
    return () => o.disconnect();
  }, []);
  // The buttons of a message (Copy, Quote, Search) take the text selected in it, in the feed and in a subagent's history alike (selection.ts).
  const section = useRef<HTMLElement>(null);
  const selection = useChatSelection(section);
  const insertQuote = useRef<((text: string) => void) | null>(null);
  const insertText = useRef<((text: string) => void) | null>(null);
  // The jump to a search hit: the event is there once `lastSeq` reaches it and the older
  // history (`olderFrom`) is loaded down to it. The block that holds it is the one that starts
  // at it, else the last one that starts at or before it (a hidden event's neighbour; an answer
  // may stand above an earlier input, SPEC §6); the feed renders from there if it did not.
  const focused = useRef<number | undefined>(undefined);
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el || focusSeq === undefined || focused.current === focusSeq || t.lastSeq < focusSeq) return;
    if (t.olderFrom > focusSeq) {
      void chat.loadOlder(focusSeq, true);
      return;
    }
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
  // A goal is cleared by the slash command, which Claude Code takes between turns (SPEC 0.218):
  // while a turn runs, it is stopped first. Stop leaves the goal set.
  const clearGoal = () =>
    act(async () => {
      if (STOPPABLE.has(state)) await chat.stop();
      chat.send('/goal clear', []);
    }, 'Goal not cleared');
  const toBackground = (callId: string) => void act(() => chat.background(callId), 'Not moved to the background');
  const stopTask = (taskId: string) => act(() => chat.stopTask(taskId), 'Not stopped');
  // The rows first in the live block (§17.1): the main agent's, or a room's agents at work; only
  // those that are drawn, so the block does not show empty for a row that would be nothing.
  const lead = room
    ? members.flatMap((m, i) =>
        activityShown(m.state, memberChats[i].timeline)
          ? [<ActivityLine key={m.id} state={m.state} timeline={memberChats[i].timeline} name={m.title || m.slug} />]
          : [],
      )
    : !system && activityShown(answeredState, t)
      ? [
          <ActivityLine
            key="main"
            state={answeredState}
            timeline={t}
            root={path}
            foreground={info?.foreground_calls}
            activity={info?.activity}
            onBackground={toBackground}
          />,
        ]
      : [];
  // Pause beside Stop (SPEC §17.3): a running turn is stopped now and the agent's process ends
  // with it; Resume in its place starts it again. Restart in "More" kills the process and
  // starts it on the same history.
  const pause = () => act(() => app.api.pauseSession(sessionId), 'Not paused');
  const resume = () => act(() => app.api.resumeSession(sessionId), 'Not resumed');
  const restart = () => act(() => app.api.restartSession(sessionId), 'Not restarted');
  const rename = (title: string) => act(() => app.renameSession(sessionId, title), 'Not renamed');
  // T38: Ask at once, Auto and Allow all after a one-line confirmation.
  const setMode = (mode: PermissionMode) => {
    const line = modeConfirm(mode);
    if (line && !confirm(line)) return;
    void act(() => app.setPermissionMode(sessionId, mode), 'Not switched');
  };
  // Resolves to whether the hub took the change.
  const setAgent = async (patch: AgentPatch): Promise<boolean> => {
    // Another provider restarts the agent (SPEC 0.144): asked once, as a switch to Auto is.
    if (patch.provider_id !== undefined && patch.provider_id !== (info?.provider?.id ?? null)) {
      const to = patch.provider_id ? app.providers.find((p) => p.id === patch.provider_id)?.label : "Claude Code's own endpoint";
      if (!confirm(`The agent restarts on ${to ?? 'the provider'}; its conversation is read again from the start of the context.`)) return false;
    }
    try {
      await app.setAgentSettings(sessionId, patch);
      return true;
    } catch (e) {
      setNotice(`Not changed: ${(e as Error).message}`);
      return false;
    }
  };
  // SPEC 0.148: the user's Chrome for the agent, on a worker that gives it; either way the
  // agent restarts, so it is asked once.
  const setBrowser = (on: boolean) => {
    const line = on
      ? `Give the agent your browser?\n\nThe agent restarts with your Chrome on ${worker ?? 'its worker'}: it opens its own tabs in your profile, signed in as you, and sees the tabs you drag into its tab group. It can click and type there; it cannot upload files. Watch what it does.`
      : 'The agent restarts without the browser.';
    if (!confirm(line)) return;
    void act(() => app.setBrowser(sessionId, on), on ? 'Browser not given' : 'Browser not taken away');
  };
  const usage = t.usageTotals();
  // A provider's model is what the provider names; the CLI prices it at a made-up rate.
  const model = info?.provider ? undefined : shownModel(info?.model, usage.model);
  // A room has no spending of its own: the sum over its agents', each on the Claude Code price
  // list (an agent on a provider has none, as a session on one shows none).
  let cost = info?.provider ? undefined : usage.cost;
  if (room) {
    cost = undefined;
    members.forEach((m, i) => {
      const spent = m.provider ? undefined : memberChats[i].timeline.usageTotals().cost;
      if (spent !== undefined) cost = (cost ?? 0) + spent;
    });
  }
  // Running, as in the list, while a background subagent or task works after the turn; a room,
  // as in the list, with its agents' state (and Stop stops them all).
  const shown = info?.room_mode ? app.shown(info).state : info && atWorkAfterTurn(info) ? 'running' : answeredState;
  const pill = <StatePill state={shown} timeline={t} where={locked ? `In ${holder}${worker ? ` on ${worker}` : ''}` : undefined} pausing={!!info && pausingAfterTurn(info)} />;
  const agents = agentsLabel(info?.subagents);
  const conn = app.offline && <span class="conn-lost">offline</span>;
  // In the meta row on a computer; on a phone after the pills, on their line.
  const spawned = info?.spawned_by && (
    <span class="chat-spawned">
      started by{' '}
      <a class="mono" href={`#/s/${info.spawned_by.session_id}`} title={info.spawned_by.address}>
        {info.spawned_by.address}
      </a>
    </span>
  );

  // T50: archived — read-only, no input; restore and delete are on the archive's rows.
  const archived = info?.archived ?? false;
  const disabled = archived || locked;
  const sheet = sheetRequest && firstApproval?.type === 'approval.request' ? firstApproval : undefined;
  const inputShown = !system && !disabled && !sheet;
  const goal = system ? null : t.goal();
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
  // The permission mode and the model, each a pill with its menu; an archived session's are marks
  // without one. A room has neither of its own (its agents have them): its mode stands in the
  // meta row. On a computer they stand in the row after the state, on a phone on a line of their
  // own under it, "started by" after them.
  const pills = !system && !room && info && (
    <>
      <ModePill
        info={info}
        phone={phone}
        frozen={archived}
        onMode={setMode}
        onBrowser={app.givesBrowser(info.worker_id) ? setBrowser : undefined}
      />
      <ModelPill
        info={info}
        model={model}
        phone={phone}
        frozen={archived}
        providers={app.providersFor(info.worker_id)}
        loadLimits={loadLimits}
        onAgent={setAgent}
      />
    </>
  );
  const title = (
    <ChatTitle
      title={info?.title || 'Session'}
      name={info?.title ?? ''}
      phone={phone}
      // An archived session is read-only; the worker's service session has no title of its own
      // to give.
      onRename={info && !archived && !system ? rename : undefined}
    />
  );
  const meta = (
    <div class="chat-meta">
      {pill}
      {/* Next to the state: the row is one line, and its end is what a narrow window cuts. */}
      {conn}
      {agents && (
        <span class="chat-agents" title="Subagents the agent runs now">
          {agents}
        </span>
      )}
      {!phone && pills}
      {system && <span class="chat-system-note">the worker's service session</span>}
      {!phone && spawned}
      {phone ? (
        // On a phone the row is the state, its subagents and a room's mode; the worker and the
        // folder are in "More".
        room && <span class="chat-room">Room · {ROOM_MODE_LABELS[room]}</span>
      ) : (
        <>
          {/* "on": after "started by <address>" the machine reads as where it runs. */}
          {worker && !system && !locked && <span class="chat-worker">on {worker}</span>}
          {path && (
            <>
              {/* Held elsewhere, the worker is in the pill: nothing to part the path from. */}
              {!locked && <span class="sep">/</span>}
              <span class="chat-path">
                <span class="mono" title={homePath(path)}>
                  {homePath(path)}
                </span>
                <CopyButton text={path} label="Copy path" />
              </span>
            </>
          )}
          {!room && info?.browser && (
            <>
              <span class="sep">/</span>
              <span class="chat-browser" title={`The agent has your Chrome on ${worker ?? 'its worker'}`}>
                Browser
              </span>
            </>
          )}
          {room && (
            <>
              {(worker || path) && <span class="sep">/</span>}
              <span class="chat-room">Room · {ROOM_MODE_LABELS[room]}</span>
            </>
          )}
          {cost !== undefined && (
            <>
              <span class="sep">/</span>
              <span class="chat-cost">
                {formatCost(cost)} this {room ? 'room' : 'session'}
              </span>
            </>
          )}
        </>
      )}
    </div>
  );
  const roomAgents = members.length > 0 && (
    <div class="chat-room-agents">
      {members.map((m) => (
        <a key={m.id} class="chat-room-agent" href={`#/s/${m.id}`}>
          {m.title || m.slug} · {STATE_LABELS[atWorkAfterTurn(m) ? 'running' : m.state]}
        </a>
      ))}
    </div>
  );
  const buttons = (
    <>
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
      {showRetry && !phone && (
        <button type="button" class="btn btn-small btn-primary btn-retry" onClick={() => act(() => chat.retry(), 'Retry failed')}>
          Retry
        </button>
      )}
      {/* Pause only while the agent works (SPEC §17.3), right before Stop; a paused session has
          Resume in its place, and Stop still while the turn it cut short ends. After the turn,
          with only a background subagent or task at work, Pause stands alone: Stop stops a turn
          and spares those (§8.1), while a pause ends them with the agent's process. */}
      {info && canPause(info) &&
        (phone ? (
          <button type="button" class="icon-btn icon-btn-round btn-pause" aria-label="Pause session" onClick={pause}>
            <IconPause size={14} />
          </button>
        ) : (
          <button type="button" class="btn btn-small btn-outline btn-pause" title="The agent stops now; messages wait for Resume" onClick={pause}>
            <IconPause size={12} />
            Pause
          </button>
        ))}
      {!!info?.paused_at && !archived &&
        (phone ? (
          <button type="button" class="icon-btn icon-btn-round btn-resume" aria-label="Resume" onClick={resume}>
            <IconPlay size={14} />
          </button>
        ) : (
          <button type="button" class="btn btn-small btn-outline btn-resume" onClick={resume}>
            <IconPlay size={12} />
            Resume
          </button>
        ))}
      {!system && STOPPABLE.has(room ? shown : state) &&
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
          // Held elsewhere, the machine is in the state pill already.
          worker={locked ? undefined : worker}
          path={path}
          cost={cost}
          onRetry={showRetry && phone ? () => void act(() => chat.retry(), 'Retry failed') : undefined}
          onSchedules={() => setSchedules(true)}
          onMove={() => setMoving(true)}
          // A room has no folder of its own, whatever workspace it was made in.
          rules={!room && rulesHref && workspace ? { href: rulesHref, path: homePath(workspace.path) } : undefined}
          onRestart={restartableSession(info) ? () => void restart() : undefined}
        />
      )}
    </>
  );
  return (
    <SelectionContext.Provider value={selection}>
      <section
        ref={section}
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
          {phone ? (
            <>
              {/* Line 1: "‹", the title up to the first button, and the buttons; the lines under
                  it are as wide as the header past "‹". */}
              <div class="chat-head-top">
                <a class="icon-btn icon-btn-round back" href={up} aria-label="Back to sessions" onClick={(e) => onBack(e, up)}>
                  <IconBack />
                </a>
                {title}
                {buttons}
              </div>
              {meta}
              {/* A room's agents stay right under its state, as on a computer. */}
              {roomAgents}
              {/* Each pill at its full text, then "started by" (alone where there are no pills),
                  whose address gives way first. */}
              {(pills || spawned) && (
                <div class="chat-pills">
                  {pills}
                  {spawned}
                </div>
              )}
            </>
          ) : (
            <>
              {sessions && <SessionsButton sessions={sessions} />}
              <div class="chat-head-main">
                {title}
                {meta}
                {roomAgents}
              </div>
              {buttons}
            </>
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
            // A scroll event while the user's scroll goes on is its momentum or its bounce.
            const user = userScrolling();
            if (user) userScrolled();
            const near = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
            const range = el.scrollHeight - el.clientHeight;
            if (near || (el.scrollTop < lastTop.current && ((user && upward.current) || range === lastRange.current))) stick.current = near;
            lastTop.current = el.scrollTop;
            lastRange.current = range;
            if (el.scrollTop < 400 && start > 0) {
              if (holding()) more.current = true;
              else showMore(blockSeq(blocks[Math.max(0, start - SHOWN)]));
            } else if (loaded && el.scrollTop < 400 && t.olderFrom > 0) {
              asked.current = true;
              void chat.loadOlder();
            }
          }}
        >
          <div class="feed" ref={feed}>
            {/* The empty feed's note in its middle: the first page on its way, or no requests
                once it is in (before, the feed has no items either). */}
            {!loaded && (
              <div class="feed-note">
                <Loading center />
              </div>
            )}
            {chat.loadingOlder && start === 0 && (
              <div class="feed-older">
                <Loading />
              </div>
            )}
            <FeedBlocks blocks={start ? blocks.slice(start) : blocks} actions={actions} />
            {requestsOnly && loaded && items.length === 0 && <div class="feed-note">No requests</div>}
            {t.streaming && <StreamingMessage text={t.streaming} />}
            <FeedBlocks blocks={under} actions={actions} />
            {!system && <WaitedRequests app={app} sessionId={sessionId} />}
          </div>
        </div>
        {notice && (
          <div class="notice" role="alert" onClick={() => setNotice(null)}>
            {notice}
          </div>
        )}
        {/* A session on a provider does not run on the subscription the limits are of; a hub with
            HQC_LIMIT_NOTICE_PERCENT=0 shows no bar. */}
        {!system && info?.worker_id && !info.provider && limitPercent > 0 && (
          <LimitBar load={loadLimits} refresh={`${info.worker_id} ${state}`} percent={limitPercent} />
        )}
        {/* The list also holds the agent's background commands, which only background_tasks counts. */}
        {/* The wakeups and cron jobs of the agent and the session's schedules wait while it is idle. */}
        {(goal !== null ||
          lead.length > 0 ||
          (info?.subagents ?? 0) > 0 ||
          (info?.background_tasks ?? 0) > 0 ||
          t.liveAgents.some((a) => a.kind === 'wakeup' || a.kind === 'cron') ||
          t.liveSchedules.length > 0) && (
          <LiveSubagents
            goal={goal ? { ...goal, running: STOPPABLE.has(state), onClear: inputShown ? clearGoal : undefined } : undefined}
            lead={lead}
            agents={t.liveAgents}
            receivedAt={t.liveAgentsAt}
            schedules={t.liveSchedules}
            schedulesAt={t.liveSchedulesAt}
            onSchedules={() => setSchedules(true)}
            onStop={stopTask}
            asking={new Set(open.flatMap((r) => (r.type === 'approval.request' && r.payload.subagent ? [r.payload.subagent.task_id] : [])))}
            history={{
              load: (taskId, after) => app.api.subagentHistory(sessionId, taskId, after),
              actions,
            }}
          />
        )}
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
        {inputShown && (
          <Composer
            initial={app.draft(sessionId)}
            onDraft={(text) => app.setDraft(sessionId, text)}
            autoFocus={focusInput}
            insertQuote={insertQuote}
            insertText={insertText}
            phone={phone}
            commands={info?.commands}
            commandInfo={info?.command_info}
            files={files}
            reading={reading}
            onFiles={addFiles}
            onRemove={(i) => setFiles((cur) => cur.filter((_, j) => j !== i))}
            onSend={(text) => {
              stick.current = true;
              chat.send(text, files);
              setFiles([]);
            }}
            giveSecret={!system && app.takesSecrets(info?.worker_id) ? (f) => app.api.giveSecret(sessionId, f) : undefined}
            quick={
              project && {
                messages: project.quick_messages ?? [],
                refresh: () => void app.refreshProject(project.id).catch(() => {}),
                send: (text) => {
                  stick.current = true;
                  chat.send(text, []);
                },
                settingsHref: `#/p/${project.id}/settings`,
              }
            }
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
    </SelectionContext.Provider>
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

/** The draft with `text` after it, a blank line between them when the draft has text. */
export function withText(draft: string, text: string): string {
  const head = draft.trimEnd();
  return head ? `${head}\n\n${text}` : text;
}

/** The `tool.call` an approval card shows (feed.ts), for the sheet. */
function approvalCall(items: ReturnType<Timeline['items']>, t: Timeline, requestId: string) {
  const b = buildFeed(items, t).find((x) => x.kind === 'approval' && x.event.payload.request_id === requestId);
  return b?.kind === 'approval' ? b.call : undefined;
}

/** The state in the header: a pill, pulsing while the session works (a turn, or a background
 *  subagent after it). `where`: a session VS Code has ("In VS Code on <machine>"), marked `</>`.
 *  `pausing`: paused while the turn the pause stopped still runs, "Pausing". */
function StatePill({ state, timeline, where, pausing }: { state: SessionState; timeline: Timeline; where?: string; pausing?: boolean }) {
  let label = where ?? STATE_LABELS[state];
  if (state === 'waiting') {
    const r = timeline.openRequests()[0];
    label = r?.type === 'approval.request' ? 'Waiting for permission' : 'Waiting for reply';
  }
  if (pausing) label = 'Pausing';
  return (
    <span class={`state pill state-${state}`}>
      {where ? <IconCode size={12} /> : <span class={`pill-dot ${state === 'running' ? 'pulse' : ''}`} aria-hidden="true" />}
      {label}
    </span>
  );
}

/** Each mode's mark: Auto and Allow all stand out, Ask is plain text. */
const MODE_MARKS: Record<PermissionMode, { cls: string; title: string }> = {
  ask: { cls: 'perm-ask', title: "Tool calls outside the worker's rules are asked about" },
  auto: { cls: 'perm-auto', title: "Claude Code decides tool calls itself; only the worker's deny list stops one" },
  allow_all: {
    cls: 'perm-allow-all',
    title: "Claude Code runs every tool call without asking, with no classifier either; only the worker's deny list, the ask rules of Claude Code's settings and its own safety checks limit it",
  },
};

/** What each mode of the mode pill's switch does (T38). */
const SWITCH_TITLES: Record<PermissionMode, string> = {
  ask: "Ask before tool calls outside the worker's rules",
  auto: "Claude Code decides tool calls itself; the worker's deny list is the only limit",
  allow_all:
    "Claude Code runs every tool call without asking, with no classifier either; the worker's deny list, the ask rules of Claude Code's settings and its own safety checks are the only limits",
};

/** What the mode pill says (T38): its look, tooltip and words, and the words as plain text for
 *  its name. What the worker reports the agent runs in, else what hqc asked for; asked for a
 *  higher mode than the worker runs, both; a turn capped by a message from a session in a lower
 *  mode (T39), the cap's mode for this turn, named by that session, and the session's own mode
 *  after it. With neither known yet the session runs its worker's default (`known` false), which
 *  the client does not know. */
function modeFace(info: SessionInfo): { cls: string; title: string; text: ComponentChildren; label: string; known: boolean } {
  const runs = info.effective_permission_mode;
  const asked = info.permission_mode;
  const by = info.permission_capped_by;
  if (by) {
    const now = runs ?? 'ask';
    // Without a mode of its own the session runs the worker's default, which is not known here.
    const own = asked ? MODE_LABELS[asked] : "the session's own mode";
    return {
      cls: `${MODE_MARKS[now].cls} perm-capped`,
      title: `This turn runs in ${MODE_LABELS[now]}: it was started by a message along a chain with ${by}, which runs in ${MODE_LABELS[now]}, and a message cannot raise permissions. ${asked ? `The session's own mode, ${MODE_LABELS[asked]},` : "The session's own mode"} comes back when the turn ends.`,
      text: (
        <>
          {MODE_LABELS[now]} (asked by {by})<span class="perm-next"> · {own} after this turn</span>
        </>
      ),
      label: `${MODE_LABELS[now]} (asked by ${by}) · ${own} after this turn`,
      known: true,
    };
  }
  if (asked && runs && PERMISSION_MODES.indexOf(asked) > PERMISSION_MODES.indexOf(runs)) {
    // Asked for a higher mode than the worker runs: not restarted yet, a process that cannot
    // switch, or a worker that does not allow it.
    const text = `${MODE_LABELS[runs]} (${MODE_LABELS[asked]} asked)`;
    return {
      cls: MODE_MARKS[runs].cls,
      title: `The worker runs this session in ${MODE_LABELS[runs]}: until the agent next runs, or for good if the worker does not allow ${MODE_LABELS[asked]}`,
      text,
      label: text,
      known: true,
    };
  }
  const shown = runs ?? asked;
  if (!shown) {
    return { cls: 'perm-default', title: "The worker's default mode, which it has not reported yet", text: 'Default', label: 'Default', known: false };
  }
  return { cls: MODE_MARKS[shown].cls, title: MODE_MARKS[shown].title, text: MODE_LABELS[shown], label: MODE_LABELS[shown], known: true };
}

/** Where a pill's menu opens: under its left end. */
function underPill(el: HTMLElement | null): { x: number; y: number } {
  const r = el?.getBoundingClientRect();
  return { x: r?.left ?? 0, y: (r?.bottom ?? 0) + 4 };
}

/** The chat's title, renamed in place: on a computer a pencil after it while the mouse is on
 *  it, on a phone a tap on the title itself, puts a field in its place, in its type. Enter or a
 *  press away saves the name, trimmed (an empty or unchanged one sends nothing); Esc keeps the
 *  old one. Without `onRename` (an archived or the service session) it is the title alone.
 *  `title` is what the header shows ("Session" for an untitled one), `name` the session's own
 *  title, which the field starts from. */
function ChatTitle({ title, name, phone, onRename }: { title: string; name: string; phone: boolean; onRename?: (title: string) => void }) {
  // The text in the field while the title is edited.
  const [draft, setDraft] = useState<string | null>(null);
  // The title when the field opened: a title that changes meanwhile (the CLI or the agent names
  // the session) is not put back by a field left unchanged.
  const opened = useRef('');
  // The edit ended by Enter or Esc: the blur of the field going away does not end it again.
  const ended = useRef(false);
  const field = useRef<HTMLInputElement>(null);
  const editing = draft !== null;
  useLayoutEffect(() => {
    if (!editing) return;
    field.current?.focus();
    field.current?.select();
  }, [editing]);
  const start = () => {
    ended.current = false;
    opened.current = name;
    setDraft(name);
  };
  const end = (value: string | null) => {
    if (ended.current) return;
    ended.current = true;
    setDraft(null);
    const typed = value?.trim();
    if (typed && typed !== opened.current) onRename?.(typed);
  };
  if (editing && onRename) {
    return (
      <h1 class="chat-title is-editing">
        <input
          ref={field}
          class="chat-title-input"
          aria-label="Title"
          value={draft}
          onInput={(e) => setDraft((e.currentTarget as HTMLInputElement).value)}
          onKeyDown={(e) => {
            // Handled here: a request's card in the chat would take Enter and Esc as its answer.
            if (e.key === 'Enter' && !e.isComposing) {
              e.preventDefault();
              end((e.currentTarget as HTMLInputElement).value);
            } else if (e.key === 'Escape') {
              e.preventDefault();
              end(null);
            }
          }}
          onBlur={(e) => end((e.currentTarget as HTMLInputElement).value)}
        />
      </h1>
    );
  }
  return (
    <h1 class={`chat-title ${phone && onRename ? 'is-renamable' : ''}`} onClick={phone && onRename ? start : undefined}>
      {title}
      {!phone && onRename && (
        <button type="button" class="icon-btn btn-rename-title" aria-label="Rename" title="Rename" onClick={start}>
          <IconRename size={13} />
        </button>
      )}
    </h1>
  );
}

/** The session's permission mode (T38) as a round pill beside the state. Pressed, it opens a
 *  switch Ask | Auto | Allow all over its items, the session's own mode pressed (Auto and Allow
 *  all keep their one-line confirmation), and on a worker that gives a browser (`onBrowser`,
 *  SPEC 0.148) the browser under it. `frozen` (archived): the mark alone, without a menu. */
function ModePill({
  info,
  phone,
  frozen,
  onMode,
  onBrowser,
}: {
  info: SessionInfo;
  phone: boolean;
  frozen: boolean;
  onMode: (mode: PermissionMode) => void;
  onBrowser?: (on: boolean) => void;
}) {
  const btn = useRef<HTMLButtonElement>(null);
  const [at, setAt] = useState<{ x: number; y: number } | null>(null);
  const face = modeFace(info);
  if (frozen) {
    return face.known ? (
      <span class={`btn-mode-pill ${face.cls}`} title={face.title}>
        <span class="pill-text">{face.text}</span>
      </span>
    ) : null;
  }
  // The mode a switch goes from: hqc's choice, else what the agent runs in.
  const own = info.permission_mode ?? info.effective_permission_mode;
  const items: MenuEntry[] = !onBrowser
    ? []
    : info.browser
      ? [{ label: 'Browser: on · Turn off', cls: 'btn-browser btn-browser-off', title: 'The agent restarts without your Chrome', onSelect: () => onBrowser(false) }]
      : [{ label: 'Browser: off · Give the browser', cls: 'btn-browser btn-browser-on', title: 'The agent restarts with your Chrome on its worker', onSelect: () => onBrowser(true) }];
  return (
    <>
      <button
        ref={btn}
        type="button"
        class={`btn-mode-pill ${face.cls}`}
        title={face.title}
        aria-label={`Permission mode: ${face.label}`}
        aria-haspopup="menu"
        aria-expanded={!!at}
        onClick={() => setAt(at ? null : underPill(btn.current))}
      >
        <span class="pill-text">{face.text}</span>
        <IconChevronDown size={10} />
      </button>
      {at && (
        <ContextMenu
          x={at.x}
          y={at.y}
          label="Permission mode"
          anchor={btn.current}
          backdrop={phone}
          onClose={() => setAt(null)}
          top={
            <div class="mode-switch" role="group" aria-label="Permission mode">
              {PERMISSION_MODES.map((m) => (
                <button
                  key={m}
                  type="button"
                  class={`btn-mode-${m}`}
                  aria-pressed={m === own}
                  title={SWITCH_TITLES[m]}
                  data-focus={m === own ? '' : undefined}
                  onClick={() => {
                    setAt(null);
                    if (m !== own) onMode(m);
                  }}
                >
                  {MODE_LABELS[m]}
                </button>
              ))}
            </div>
          }
          items={items}
        />
      )}
    </>
  );
}

/** The session's model as a pill beside the mode's: the provider it runs on, else the model
 *  the header names (`model`: the one switched to since the last turn, else the last turn's),
 *  else "Default". Pressed, it opens the model menu (`modelMenuItems`): a pick switches the
 *  session at once, the entry it has (an alias, a version in the submenu) only closes the menu,
 *  while a family checked for one of its versions switches to its alias; "Another model…" asks
 *  for the name and Enter switches to it; the usage limits of the worker's account stand under
 *  the items.
 *  `frozen` (archived): the name alone, without a menu. */
function ModelPill({
  info,
  model,
  phone,
  frozen,
  providers,
  loadLimits,
  onAgent,
}: {
  info: SessionInfo;
  model?: string;
  phone: boolean;
  frozen: boolean;
  /** The providers the session's worker allows (SPEC 0.144). */
  providers: ModelProvider[];
  /** The usage limits of the session's worker account, read when the menu opens. */
  loadLimits: () => Promise<WorkerLimits | undefined>;
  onAgent: (patch: AgentPatch) => Promise<boolean>;
}) {
  const btn = useRef<HTMLButtonElement>(null);
  const [at, setAt] = useState<{ x: number; y: number; typing?: boolean } | null>(null);
  const label = info.provider?.label ?? (model ? modelName(model) : 'Default');
  const title = info.provider?.label ?? model ?? "Claude Code's default model";
  if (frozen) {
    return (
      <span class="btn-model-pill" title={title}>
        <span class="pill-text">{label}</span>
      </span>
    );
  }
  const pick = (c: ModelChoice) => {
    if ('provider' in c ? c.provider === info.provider?.id : !info.provider && c.model === info.model) return;
    void onAgent(agentPatch(info, c));
  };
  let menu: ComponentChildren = null;
  if (at) {
    const common = { x: at.x, y: at.y, label: 'Model', anchor: btn.current, backdrop: phone, onClose: () => setAt(null) };
    menu = at.typing ? (
      <ContextMenu {...common}>
        <ModelName
          // A name typed before stands in the field.
          value={!info.provider && isTypedModel(info.model) ? info.model! : ''}
          onKeyDown={(e) => {
            if (e.key !== 'Enter' || e.isComposing) return;
            e.preventDefault();
            const m = (e.currentTarget as HTMLInputElement).value.trim();
            if (!m) return;
            setAt(null);
            pick({ model: m });
          }}
        />
      </ContextMenu>
    ) : (
      <ContextMenu
        {...common}
        items={modelMenuItems({
          model: info.model,
          provider: info.provider?.id,
          providers,
          onPick: pick,
          onAnother: () => setAt({ ...at, typing: true }),
        })}
        bottom={<Limits load={loadLimits} />}
      />
    );
  }
  return (
    <>
      <button
        ref={btn}
        type="button"
        class="btn-model-pill"
        title={title}
        aria-label={`Model: ${label}`}
        aria-haspopup="menu"
        aria-expanded={!!at}
        onClick={() => setAt(at ? null : underPill(btn.current))}
      >
        <span class="pill-text">{label}</span>
        <IconChevronDown size={10} />
      </button>
      {menu}
    </>
  );
}

type AgentPatch = Pick<SessionPatch, 'model' | 'provider_id'>;

/** The `PATCH` of a model menu's choice: a Claude model leaves the provider the session runs on
 *  in the same `PATCH`. */
function agentPatch(info: SessionInfo, c: ModelChoice): AgentPatch {
  if ('provider' in c) return { provider_id: c.provider };
  return info.provider ? { provider_id: null, model: c.model ?? null } : { model: c.model ?? null };
}

/** "More": on a phone first where the session runs, "<worker> · <folder>" (the folder with its
 *  copy button; no worker while VS Code holds it, the state pill names it), and the cost, then
 *  Retry; Schedules
 *  and Move to project… (neither for a room: it has no worker or folder; no move for an
 *  archived session), Folder rules (`rules`, T48) and Restart session. The title is renamed in
 *  place, the mode and the model are pills of the meta row, Pause stands by Stop; archiving is
 *  on the list's rows, restore and delete on the archive's (T50). Nothing to hold, no button. */
function MoreMenu({
  info,
  phone,
  worker,
  path,
  cost,
  onRetry,
  onSchedules,
  onMove,
  rules,
  onRestart,
}: {
  info: SessionInfo;
  phone: boolean;
  /** On a phone, with the folder, where the session runs: the meta row has no room for them. */
  worker?: string;
  path?: string;
  cost?: number;
  /** On a phone "Retry" lives here, not in the crowded header. */
  onRetry?: () => void;
  onSchedules: () => void;
  /** "Move to project…": the session continues in another folder and project. */
  onMove: () => void;
  /** The rules screen of the session's folder and the folder, for the tooltip. */
  rules?: { href: string; path: string };
  /** "Restart session": the agent's process killed and started on the same history; absent
   *  where it may not be (`restartableSession`). */
  onRestart?: () => void;
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
  const facts = phone && (worker || path || cost !== undefined);
  const schedules = !info.room_mode;
  const move = !info.archived && !info.room_mode;
  if (!facts && !onRetry && !schedules && !move && !rules && !onRestart) return null;
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
          {facts && (
            <div class="menu-info">
              {(worker || path) && (
                <div class="menu-info-row menu-place">
                  <span class="mono">{[worker, path && homePath(path)].filter(Boolean).join(' · ')}</span>
                  {path && <CopyButton text={path} label="Copy path" />}
                </div>
              )}
              {cost !== undefined && <div class="menu-info-row">{formatCost(cost)} this {info.room_mode ? 'room' : 'session'}</div>}
            </div>
          )}
          {onRetry && item('btn-retry', 'Retry', onRetry)}
          {schedules && item('btn-schedules', 'Schedules', onSchedules, 'Messages scheduled into this session')}
          {move && item('btn-move', 'Move to project…', onMove, 'Continue this session in another folder and project on its worker')}
          {rules && (
            <a role="menuitem" class="menu-item btn-folder-rules" href={rules.href} title={`Rules for ${rules.path}`} onClick={() => setOpen(false)}>
              Folder rules
            </a>
          )}
          {onRestart && item('btn-restart', 'Restart session', onRestart, "The agent's process starts again on the same history")}
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

/** A picked file, read into memory at once, with a `data:` preview for images (the CSP has
 *  no `blob:`). On an iPhone a HEIC file whose preview was read in full still went out cut
 *  off when the upload read it again ("incomplete multipart stream"), so the upload sends
 *  what was read here.
 *  A HEIC or HEIF image becomes a JPEG: the agent does not read HEIC. The browser draws it
 *  where it decodes it (Safari, any browser on an iPhone), libheif decodes it elsewhere. */
export async function toOutgoing(file: File): Promise<OutgoingFile> {
  let f: OutgoingFile = { file, name: file.name || 'file' };
  if (file.size <= HELD_MAX) {
    try {
      f.file = new Blob([await file.arrayBuffer()], { type: file.type });
    } catch {
      // Not readable: the upload fails and says so.
      return f;
    }
    if (/^image\/hei[cf]/i.test(file.type) || HEIC_NAME.test(f.name)) {
      const jpeg = (await toJpeg(f.file)) ?? (await heicToJpeg(f.file));
      if (jpeg) f = { file: jpeg, name: `${f.name.replace(HEIC_NAME, '')}.jpg` };
    }
  }
  if (!f.file.type.startsWith('image/') || f.file.size > PREVIEW_MAX || typeof FileReader === 'undefined') return f;
  return { ...f, preview: await dataUrl(f.file) };
}

/** `b` drawn as a JPEG, or null where the browser does not decode it. */
async function toJpeg(b: Blob): Promise<Blob | null> {
  try {
    const src = await dataUrl(b);
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
    return jpeg;
  } catch {
    return null;
  }
}

export function Composer({
  onSend,
  placeholder,
  phone = false,
  commands,
  commandInfo,
  files = [],
  reading = 0,
  onFiles = () => {},
  onRemove = () => {},
  insertQuote,
  insertText,
  initial = '',
  onDraft,
  autoFocus = false,
  wait,
  giveSecret,
  quick,
}: {
  onSend: (text: string) => void;
  placeholder?: string;
  phone?: boolean;
  /** The runtime's slash commands: `/` at the start of the text opens a list of them. */
  commands?: string[];
  /** What the runtime says of each command, shown beside it in the list. */
  commandInfo?: SlashCommand[];
  files?: OutgoingFile[];
  /** Files picked and still being read: nothing is sent until they are in `files`. */
  reading?: number;
  onFiles?: (files: FileList | File[]) => void;
  onRemove?: (index: number) => void;
  /** Set here to a function that puts a quote of the feed into the message (`withQuote`),
   *  focused, the caret at its end. */
  insertQuote?: { current: ((text: string) => void) | null };
  /** Set here to a function that adds a text to the message (`withText`), focused, the caret at
   *  its end: a message the user took back. */
  insertText?: { current: ((text: string) => void) | null };
  /** The text not sent last time (a draft), and where every change of it goes. */
  initial?: string;
  onDraft?: (text: string) => void;
  /** The caret in the field when it is shown with this set, or when this turns on. */
  autoFocus?: boolean;
  /** Sending waits for this (a new session being created): send is disabled with it as its
   *  title, Enter sends nothing, the text stays. */
  wait?: string;
  /** "Secret" beside attach (SPEC 0.147): only where the session's worker takes secrets. The
   *  answer's `note` goes into the text; the value goes nowhere else. */
  giveSecret?: (f: SecretFields) => Promise<SecretGiven>;
  /** The project's quick messages: a button beside the field opens them, one press of a message
   *  sends it (`send`) and leaves the field's text and files as they are. `settingsHref` is where
   *  the list is edited. */
  quick?: { messages: string[]; send: (text: string) => void; refresh: () => void; settingsHref: string };
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
  useEffect(() => {
    if (!insertText) return;
    insertText.current = (t) => {
      caretToEnd.current = true;
      setText((cur) => withText(cur, t));
      area.current?.focus();
    };
    return () => {
      insertText.current = null;
    };
  }, [insertText]);
  const picker = useRef<HTMLInputElement>(null);
  const [secretOpen, setSecretOpen] = useState(false);
  const addNote = (note: string) => {
    caretToEnd.current = true;
    setText((cur) => (cur.trim() ? `${cur.replace(/\s+$/, '')}\n${note}` : note));
    area.current?.focus();
  };
  const canSend = !wait && (text.trim() !== '' || files.length > 0) && reading === 0;
  // The slash command popup (T27): the matches, the highlighted one, and the text Esc closed
  // it at (typing on reopens it).
  const [pick, setPick] = useState(0);
  const [closedAt, setClosedAt] = useState<string | null>(null);
  const matches = closedAt === text ? [] : matchCommands(commands, text);
  const about = useMemo(() => new Map((commandInfo ?? []).map((c) => [c.name, c])), [commandInfo]);
  const popup = matches.length > 0;
  const active = Math.min(pick, matches.length - 1);
  const list = useRef<HTMLUListElement>(null);
  useLayoutEffect(() => {
    list.current?.children[active]?.scrollIntoView?.({ block: 'nearest' });
  }, [active, popup]);
  const [quickOpen, setQuickOpen] = useState(false);
  const quickList = useRef<HTMLUListElement>(null);
  const quickButton = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (!quickOpen) return;
    const doc = area.current?.ownerDocument ?? document;
    const away = (e: PointerEvent) => {
      const t = e.target as Node;
      if (!quickList.current?.contains(t) && !quickButton.current?.contains(t)) setQuickOpen(false);
    };
    const esc = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      e.preventDefault();
      e.stopPropagation();
      // A message of the list had the focus: it goes with the list, back to the opener.
      if (quickList.current?.contains(doc.activeElement)) quickButton.current?.focus();
      setQuickOpen(false);
    };
    doc.addEventListener('pointerdown', away);
    doc.addEventListener('keydown', esc, true);
    return () => {
      doc.removeEventListener('pointerdown', away);
      doc.removeEventListener('keydown', esc, true);
    };
  }, [quickOpen]);
  const edit = (t: string) => {
    // In the same update as the text: a key right after typing moves from the first match.
    setPick(0);
    setText(t);
    // Typing is for the slash popup now: the two are never open together.
    setQuickOpen(false);
  };
  const toggleQuick = () => {
    // Opening hides the slash popup until the text changes.
    setClosedAt(text);
    // The list on show is the one kept here; what another device changed comes with the answer.
    if (!quickOpen) quick?.refresh();
    setQuickOpen(!quickOpen);
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
  const secretButton = giveSecret && (
    <button
      type="button"
      class={`icon-btn btn-secret ${secretOpen ? 'is-on' : ''}`}
      aria-label="Secret for the agent"
      title="Give the agent a password or an ssh key"
      aria-expanded={secretOpen}
      onClick={() => setSecretOpen(!secretOpen)}
    >
      <IconKey />
    </button>
  );
  const quickButtonEl = quick && (
    <button
      ref={quickButton}
      type="button"
      class={`icon-btn btn-quick ${quickOpen ? 'is-on' : ''}`}
      aria-label="Quick messages"
      title="Quick messages"
      aria-expanded={quickOpen}
      aria-controls={quickOpen ? 'composer-quick' : undefined}
      onClick={toggleQuick}
    >
      <IconBolt />
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
      {giveSecret && secretOpen && (
        <ComposerSecret give={giveSecret} onNote={addNote} onClose={() => setSecretOpen(false)} />
      )}
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
                {about.get(c)?.argument_hint && <span class="cmd-args">{about.get(c)?.argument_hint}</span>}
                {about.get(c)?.description && <span class="cmd-desc">{about.get(c)?.description}</span>}
              </li>
            ))}
          </ul>
        )}
        {quick && quickOpen && (
          <ul class="cmd-popup quick-popup" id="composer-quick" aria-label="Quick messages" ref={quickList}>
            {quick.messages.map((m, i) => (
              <li key={i}>
                <button
                  type="button"
                  class="cmd-item quick-item"
                  title={m}
                  onClick={() => {
                    quick.send(m);
                    setQuickOpen(false);
                  }}
                >
                  {m.trim().split('\n')[0]}
                </button>
              </li>
            ))}
            <li class="quick-edit">
              {quick.messages.length === 0 ? (
                <>
                  No quick messages yet — <a href={quick.settingsHref}>add them in project settings</a>
                </>
              ) : (
                <a href={quick.settingsHref}>Edit quick messages</a>
              )}
            </li>
          </ul>
        )}
        {(files.length > 0 || reading > 0) && (
          <div class="composer-files">
            {reading > 0 && (
              <span class="attachment-chip muted">
                reading {reading} {reading === 1 ? 'file' : 'files'}…
              </span>
            )}
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
          {secretButton}
          {quickButtonEl}
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
  // No block starts at `from` any more: an older page joined the block that did to the events
  // before it (a group of calls the page's edge cut), and the block before holds it now.
  if (i > 0 && (blockSeq(blocks[i]) ?? from) > from) return i - 1;
  return i < 0 ? blocks.length : i;
}

/** Index of the first of the last blocks that are inputs come while the text being written now
 *  was (`during_text`, stored after the last event that ended a text or a thinking), inputs
 *  waiting for a turn of their own (`queued`, still `pending`) or messages not stored yet, and
 *  the thinking being written above them. */
function underText(blocks: ReturnType<typeof buildFeed>, t: Timeline): number {
  const evs = t.events();
  let ended = 0;
  for (let i = evs.length - 1; i >= 0; i--) {
    const e = evs[i];
    if (e.type === 'agent.message' || e.type === 'agent.thinking' || (e.type === 'status' && e.payload.state !== 'running')) {
      ended = e.seq!;
      break;
    }
  }
  let i = blocks.length;
  while (i > 0) {
    const b = blocks[i - 1];
    if (b.kind !== 'item') break;
    const it = b.item;
    const queued = it.kind === 'event' && it.queued && it.event.delivery === 'pending';
    if (!(it.kind === 'pending' || queued || (it.kind === 'event' && duringText(it) && it.event.seq! > ended))) break;
    i--;
  }
  // The thinking being written, which the timeline puts right above those inputs. A text shown
  // as being written while a thinking is written came before that thinking: the worker holds a
  // text until the next block of the response is stored. So the thinking goes under it.
  const prev = blocks[i - 1];
  if (prev?.kind === 'item' && prev.item.kind === 'event' && prev.item.event.type === 'agent.delta') i--;
  return i;
}
