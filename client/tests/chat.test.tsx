// @vitest-environment happy-dom
// T26: the chat screen — the header (state pill, worker, path, model and cost of the
// session, "Stop", "More"), the composer in one row, the VS Code strip in its place, the
// request's hotkeys through the chat, and the phone's bottom sheet with the text answer.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { Api, ApiError, type SessionPatch } from '../src/api';
import { AppState } from '../src/app';
import {
  parseEvent,
  type HeldBy,
  type HqcEvent,
  type OpenRequest,
  type PermissionMode,
  type EventOf,
  type RoomMode,
  type SessionInfo,
  type SessionState,
  type SlashCommand,
  type SpawnedBy,
  type SubagentHistory,
} from '../src/proto';
import { ALLOW_ALL_CONFIRM, AUTO_CONFIRM, modelName, shownModel } from '../src/ui/labels';
import { limitNotice } from '../src/ui/limits';
import type { HubSocket, SessionSubscriber } from '../src/ws';
import { Chat, withQuote } from '../src/ui/Chat';
import { firstSentences } from '../src/ui/selection';
import { LiveSubagents } from '../src/ui/LiveSubagents';
import { WEB_SEARCHES, storedWebSearch } from '../src/websearch';

const WIRE = join(import.meta.dirname, '../../crates/hqc-proto/examples/wire');
const ev = (name: string, seq: number, patch: (e: any) => void = () => {}): HqcEvent => {
  const e = JSON.parse(readFileSync(join(WIRE, `${name}.json`), 'utf8'));
  e.seq = seq;
  e.session_id = 's1';
  patch(e);
  return parseEvent(e);
};

let root: HTMLElement;
afterEach(() => {
  if (root) render(null, root);
  document.body.innerHTML = '';
  // Drafts stay on the device (`hqc.drafts`), from one test to the next too.
  localStorage.clear();
});

/** The Copy button, not the other buttons of the strip, which have the class too. */
const COPY = '.btn-copy:not(.btn-msg-search):not(.btn-msg-quote):not(.btn-to-quick):not(.btn-fork)';
/** A Search button that is shown (not the hidden slot a message keeps for it). */
const SEARCH = '.btn-msg-search:not(.btn-msg-search-idle)';

const HISTORY = [
  ev('event.user.message', 1, (e) => (e.payload.artifact_ids = [])),
  ev('event.agent.message', 2),
  ev('event.usage', 3, (e) => (e.payload = { input_tokens: 2, output_tokens: 555, cost_usd: 0.05, model: 'claude-opus-5-5' })),
  ev('event.user.message', 4, (e) => ((e.event_id = '0199f3a2-7c10-7000-8000-0000000001c4'), (e.payload.artifact_ids = []))),
  ev('event.tool.call', 5, (e) => (e.payload.args_preview = '{"command":"ls /"}')),
  ev('event.approval.request', 6, (e) => {
    e.ts = new Date(Date.now() - 42_000).toISOString();
    e.payload.description = 'List root directory\nls /';
    delete e.payload.expires_at;
  }),
  ev('event.usage', 7, (e) => (e.payload = { input_tokens: 1, output_tokens: 2, cost_usd: 0.02 })),
];

/** The open chat's subscription: what the hub sends it over the socket. */
let subscriber: SessionSubscriber | undefined;

function setup(
  state: SessionState,
  opts: {
    held_by?: HeldBy;
    history?: HqcEvent[];
    commands?: string[];
    fetched?: string[];
    fetched_info?: SlashCommand[];
    spawned_by?: SpawnedBy;
    permission_mode?: PermissionMode;
    effective_permission_mode?: PermissionMode;
    permission_capped_by?: string;
    subagents?: number;
    background_tasks?: number;
    foreground_calls?: string[];
    model?: string;
    paused_at?: string;
    room_mode?: RoomMode;
  } = {},
) {
  const history = opts.history ?? HISTORY;
  const api = {
    // GET /api/sessions/{id}: the session object with the commands the hub has by now, and
    // what the runtime says of each, which a list does not carry.
    session: vi.fn(async (): Promise<SessionInfo> => ({ ...info, commands: opts.fetched ?? opts.commands, command_info: opts.fetched_info })),
    events: vi.fn(async (_id: string, after: number) => {
      const events = history.filter((e) => e.seq! > after);
      return { events, invalid: [], maxSeq: Math.max(after, ...events.map((e) => e.seq!)), count: events.length };
    }),
    eventsBefore: vi.fn(async (_id: string, before = Number.MAX_SAFE_INTEGER, limit = 200) => {
      const found = history.filter((e) => e.seq! < before).slice(-limit);
      const seqs = found.map((e) => e.seq!);
      return { events: found, invalid: [], maxSeq: Math.max(0, ...seqs), minSeq: seqs[0], count: found.length };
    }),
    readCursor: vi.fn(async () => {}),
    // GET /api/schedules?session_id=
    schedules: vi.fn(async () => []),
    approve: vi.fn(async () => {}),
    takeover: vi.fn(async () => ({})),
    // PATCH /api/sessions/{id}: the hub stores the mode, the worker confirms it at once; the
    // model is stored as sent (`null`: the runtime's default).
    patchSession: vi.fn(async (_id: string, patch: SessionPatch) => ({
      ...info,
      permission_mode: patch.permission_mode,
      effective_permission_mode: patch.permission_mode,
      model: patch.model === undefined ? info.model : (patch.model ?? undefined),
      // SPEC 0.144: the provider as the hub names it.
      provider:
        patch.provider_id === undefined
          ? info.provider
          : patch.provider_id
            ? { id: patch.provider_id, label: app.providers.find((p) => p.id === patch.provider_id)?.label ?? '' }
            : undefined,
      // SPEC 0.148: the browser as stored.
      browser: patch.browser === undefined ? info.browser : patch.browser || undefined,
    })),
    // POST /api/sessions/{id}/fork: the new session.
    forkSession: vi.fn(async () => ({ ...info, id: 's2', title: 'test 12 (fork)', last_seq: 4, last_read_seq: 4 })),
    // POST /api/sessions/{id}/background.
    background: vi.fn(async () => {}),
    // POST /api/sessions/{id}/tasks/{task_id}/stop.
    stopTask: vi.fn(async () => {}),
    // POST /api/sessions/{id}/stop, /pause, /resume, /restart.
    stop: vi.fn(async () => {}),
    pauseSession: vi.fn(async () => {}),
    resumeSession: vi.fn(async () => {}),
    restartSession: vi.fn(async () => {}),
    // GET /api/sessions/{id}/subagents/{task_id}/history: nothing done yet unless a test says.
    subagentHistory: vi.fn(
      async (_id: string, _task: string, _after?: string): Promise<SubagentHistory> => ({ events: [], truncated: false }),
    ),
    sessions: vi.fn(async () => []),
    projects: vi.fn(async () => []),
    workspaces: vi.fn(async () => [
      {
        id: 'w1',
        project_id: 'p1',
        worker_id: 'wk1',
        path: '/Users/me/work/hqc',
        label: '',
        kind: 'dir',
        mirror_enabled: true,
        created_at: '2026-10-20T10:00:00Z',
      },
    ]),
  };
  const socket = {
    onState: () => () => {},
    subscribe: (_id: string, sub: SessionSubscriber) => {
      subscriber = sub;
      return () => {};
    },
  } as unknown as HubSocket;
  const app = new AppState(api as unknown as Api, socket);
  app.conn = 'open';
  const info: SessionInfo = {
    id: 's1',
    project_id: 'p1',
    workspace_id: 'w1',
    worker_id: 'wk1',
    cwd: '.',
    title: 'test 12',
    slug: 'test-12',
    state,
    queue_len: 0,
    unread: 0,
    last_seq: 7,
    last_read_seq: 7,
    archived: false,
    updated_at: '2026-10-24T19:47:00Z',
    system: false,
    held_by: opts.held_by,
    commands: opts.commands,
    spawned_by: opts.spawned_by,
    permission_mode: opts.permission_mode,
    effective_permission_mode: opts.effective_permission_mode,
    permission_capped_by: opts.permission_capped_by,
    subagents: opts.subagents,
    background_tasks: opts.background_tasks,
    foreground_calls: opts.foreground_calls,
    model: opts.model,
    paused_at: opts.paused_at,
    room_mode: opts.room_mode,
  };
  app.sessions.set('s1', info);
  app.sessions.set('sys', { ...info, id: 'sys', title: 'drzlo-mac', system: true, workspace_id: undefined, held_by: undefined, state: 'idle' });
  return { api, app };
}

async function mount(app: AppState, phone = false) {
  root = document.createElement('div');
  document.body.appendChild(root);
  // History over REST and the workspaces load inside act, so the effects of the renders
  // they cause run before the test looks.
  await act(async () => {
    render(<Chat app={app} sessionId="s1" phone={phone} />, root);
    await new Promise((r) => setTimeout(r, 50));
  });
}

const $ = (sel: string) => root.querySelector<HTMLElement>(sel);
const button = (text: string) => [...root.querySelectorAll<HTMLButtonElement>('button')].find((b) => b.textContent?.trim() === text);

// T38: the session's permission mode in the header, a pill whose menu switches it.
describe('permission mode', () => {
  const mark = () => ({ auto: $('.perm-auto')?.textContent, ask: $('.perm-ask')?.textContent });
  /** The mode pill pressed: the switch's buttons over the menu's items. */
  const menu = async () => {
    await act(async () => $('.btn-mode-pill')!.click());
    return [...root.querySelectorAll<HTMLButtonElement>('.ctx-menu .mode-switch button')];
  };
  const pressed = () => [...root.querySelectorAll('.ctx-menu .mode-switch [aria-pressed="true"]')].map((b) => b.textContent);

  it('marks Auto, shows Ask, and what the worker runs over what was asked', async () => {
    let { app } = setup('idle', { permission_mode: 'auto', effective_permission_mode: 'auto' });
    await mount(app);
    expect(mark()).toEqual({ auto: 'Auto', ask: undefined });
    expect($('.perm-auto')!.getAttribute('title')).toContain('deny list');
    render(null, root);

    // Asked for auto, the worker runs ask (allow_auto = false, or not started again yet).
    ({ app } = setup('idle', { permission_mode: 'auto', effective_permission_mode: 'ask' }));
    await mount(app);
    expect(mark()).toEqual({ auto: undefined, ask: 'Ask (Auto asked)' });
    render(null, root);

    ({ app } = setup('idle', { effective_permission_mode: 'ask' }));
    await mount(app);
    expect(mark()).toEqual({ auto: undefined, ask: 'Ask' });
    render(null, root);

    // Nothing known yet: no mark.
    ({ app } = setup('idle'));
    await mount(app);
    expect(mark()).toEqual({ auto: undefined, ask: undefined });
  });

  it('switches to Auto after a one-line confirmation and back to Ask without one', async () => {
    const { app, api } = setup('idle', { effective_permission_mode: 'ask' });
    await mount(app);
    const confirm = vi.fn(() => false);
    vi.stubGlobal('confirm', confirm);
    try {
      let items = await menu();
      expect(items.map((b) => b.textContent)).toEqual(['Ask', 'Auto', 'Allow all']);
      expect(pressed()).toEqual(['Ask']);
      await act(async () => items.find((b) => b.textContent === 'Auto')!.click());
      expect(confirm).toHaveBeenCalledWith(AUTO_CONFIRM);
      expect(AUTO_CONFIRM).toContain("deny list is the only limit");
      expect(AUTO_CONFIRM.includes('\n')).toBe(false);
      expect(api.patchSession).not.toHaveBeenCalled();
      expect($('.ctx-menu')).toBeNull();

      confirm.mockReturnValue(true);
      items = await menu();
      await act(async () => {
        items.find((b) => b.textContent === 'Auto')!.click();
        await new Promise((r) => setTimeout(r, 10));
      });
      expect(api.patchSession).toHaveBeenCalledWith('s1', { permission_mode: 'auto' });
      expect(mark().auto).toBe('Auto');

      confirm.mockClear();
      items = await menu();
      expect(pressed()).toEqual(['Auto']);
      await act(async () => {
        items.find((b) => b.textContent === 'Ask')!.click();
        await new Promise((r) => setTimeout(r, 10));
      });
      expect(confirm).not.toHaveBeenCalled();
      expect(api.patchSession).toHaveBeenLastCalledWith('s1', { permission_mode: 'ask' });
      expect(mark()).toEqual({ auto: undefined, ask: 'Ask' });
    } finally {
      vi.unstubAllGlobals();
    }
  });

  // T39: a turn capped by a message from a session in Ask.
  it('shows a capped turn as Ask, who asked, and the own mode after it', async () => {
    const { app } = setup('running', {
      permission_mode: 'auto',
      effective_permission_mode: 'ask',
      permission_capped_by: 'personal/a',
    });
    await mount(app);
    expect(mark()).toEqual({ auto: undefined, ask: 'Ask (asked by personal/a) · Auto after this turn' });
    expect($('.perm-capped')!.getAttribute('title')).toContain('cannot raise permissions');

    // The turn ends: the summary drops the cap and the worker runs Auto again.
    const s = app.sessions.get('s1')!;
    await act(async () =>
      app.mergeSummary([
        {
          session_id: 's1',
          project_id: 'p1',
          title: s.title,
          state: 'idle',
          queue_len: 0,
          unread: 0,
          last_seq: 7,
          last_read_seq: 7,
          permission_mode: 'auto',
          effective_permission_mode: 'auto',
        },
      ]),
    );
    expect(mark()).toEqual({ auto: 'Auto', ask: undefined });
  });

  // SPEC 0.129: Allow all stands out like Auto; asked but not run yet it says so; a turn an
  // Auto cap holds down shows Auto, who asked, and Allow all after it.
  it('marks Allow all, what the worker runs below it, and an Auto cap', async () => {
    const all = () => $('.perm-allow-all')?.textContent;
    let { app } = setup('idle', { permission_mode: 'allow_all', effective_permission_mode: 'allow_all' });
    await mount(app);
    expect(all()).toBe('Allow all');
    expect(mark()).toEqual({ auto: undefined, ask: undefined });
    expect($('.perm-allow-all')!.getAttribute('title')).toContain('deny list');
    render(null, root);

    ({ app } = setup('idle', { permission_mode: 'allow_all', effective_permission_mode: 'auto' }));
    await mount(app);
    expect(mark()).toEqual({ auto: 'Auto (Allow all asked)', ask: undefined });
    render(null, root);

    ({ app } = setup('idle', { permission_mode: 'allow_all', effective_permission_mode: 'ask' }));
    await mount(app);
    expect(mark()).toEqual({ auto: undefined, ask: 'Ask (Allow all asked)' });
    render(null, root);

    ({ app } = setup('running', {
      permission_mode: 'allow_all',
      effective_permission_mode: 'auto',
      permission_capped_by: 'personal/a',
    }));
    await mount(app);
    expect(mark()).toEqual({ auto: 'Auto (asked by personal/a) · Allow all after this turn', ask: undefined });
    expect($('.perm-capped')!.getAttribute('title')).toContain('cannot raise permissions');
    render(null, root);

    // An Ask cap on an Allow all session names Allow all as the mode after it.
    ({ app } = setup('running', {
      permission_mode: 'allow_all',
      effective_permission_mode: 'ask',
      permission_capped_by: 'personal/a',
    }));
    await mount(app);
    expect(mark()).toEqual({ auto: undefined, ask: 'Ask (asked by personal/a) · Allow all after this turn' });
    render(null, root);

    // With no mode of its own it runs the worker's default, which the client does not know.
    ({ app } = setup('running', { effective_permission_mode: 'ask', permission_capped_by: 'personal/a' }));
    await mount(app);
    expect(mark()).toEqual({ auto: undefined, ask: "Ask (asked by personal/a) · the session's own mode after this turn" });
  });

  it('switches to Allow all after its own one-line confirmation', async () => {
    const { app, api } = setup('idle', { permission_mode: 'auto', effective_permission_mode: 'auto' });
    await mount(app);
    const confirm = vi.fn(() => false);
    vi.stubGlobal('confirm', confirm);
    try {
      let items = await menu();
      expect(pressed()).toEqual(['Auto']);
      await act(async () => items.find((b) => b.textContent === 'Allow all')!.click());
      expect(confirm).toHaveBeenCalledWith(ALLOW_ALL_CONFIRM);
      expect(ALLOW_ALL_CONFIRM.includes('\n')).toBe(false);
      expect(api.patchSession).not.toHaveBeenCalled();

      confirm.mockReturnValue(true);
      items = await menu();
      await act(async () => {
        items.find((b) => b.textContent === 'Allow all')!.click();
        await new Promise((r) => setTimeout(r, 10));
      });
      expect(api.patchSession).toHaveBeenCalledWith('s1', { permission_mode: 'allow_all' });
      expect($('.perm-allow-all')!.textContent).toBe('Allow all');
      await menu();
      expect(pressed()).toEqual(['Allow all']);
      // The pressed one only closes the menu.
      confirm.mockClear();
      await act(async () => root.querySelector<HTMLButtonElement>('.ctx-menu .btn-mode-allow_all')!.click());
      expect($('.ctx-menu')).toBeNull();
      expect(confirm).not.toHaveBeenCalled();
      expect(api.patchSession).toHaveBeenCalledTimes(1);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it('follows the mode a summary brings', async () => {
    const { app } = setup('idle', { permission_mode: 'auto', effective_permission_mode: 'ask' });
    await mount(app);
    const s = app.sessions.get('s1')!;
    await act(async () =>
      app.mergeSummary([
        {
          session_id: 's1',
          project_id: 'p1',
          title: s.title,
          state: 'idle',
          queue_len: 0,
          unread: 0,
          last_seq: 7,
          last_read_seq: 7,
          permission_mode: 'auto',
          effective_permission_mode: 'auto',
        },
      ]),
    );
    expect(mark().auto).toBe('Auto');
  });

  it('is a round pill with a chevron that opens its menu under it; with no mode known it says Default; archived, the mark alone', async () => {
    let { app } = setup('idle', { permission_mode: 'auto', effective_permission_mode: 'auto' });
    await mount(app);
    const pill = $('.chat-meta button.btn-mode-pill')!;
    expect(pill.classList.contains('perm-auto')).toBe(true);
    expect(pill.classList.contains('pill')).toBe(false);
    expect(pill.getAttribute('aria-label')).toBe('Permission mode: Auto');
    expect(pill.getAttribute('aria-haspopup')).toBe('menu');
    expect(pill.querySelector('svg')).not.toBeNull();
    await act(async () => pill.click());
    expect($('.ctx-menu')!.getAttribute('aria-label')).toBe('Permission mode');
    expect(pill.getAttribute('aria-expanded')).toBe('true');
    // The switch stands over the items, the session's own mode pressed and focused.
    expect($('.ctx-menu > .menu-top .mode-switch')).not.toBeNull();
    expect(document.activeElement).toBe($('.ctx-menu .btn-mode-auto'));
    await act(async () => pill.click());
    expect($('.ctx-menu')).toBeNull();
    render(null, root);

    ({ app } = setup('idle'));
    await mount(app);
    expect($('.btn-mode-pill')!.textContent).toBe('Default');
    await act(async () => $('.btn-mode-pill')!.click());
    expect(root.querySelectorAll('.ctx-menu .mode-switch [aria-pressed="true"]')).toHaveLength(0);
    render(null, root);

    ({ app } = setup('idle', { permission_mode: 'auto', effective_permission_mode: 'auto' }));
    app.sessions.set('s1', { ...app.sessions.get('s1')!, archived: true, state: 'archived' });
    await mount(app);
    expect($('button.btn-mode-pill')).toBeNull();
    expect($('span.btn-mode-pill.perm-auto')!.textContent).toBe('Auto');
    expect($('span.btn-mode-pill svg')).toBeNull();
  });
});

// The session's model in the header's model pill: a pick PATCHes at once, a typed name on
// Enter; `null` goes back to the runtime's default.
describe('model', () => {
  const open = () => act(async () => $('.btn-model-pill')!.click());
  /** The menu's own items, not its flyout's. */
  const items = () => [...root.querySelector('.ctx-menu')!.querySelectorAll<HTMLElement>(':scope > .menu-item')];
  const labels = () => items().map((i) => i.querySelector('.menu-label')!.textContent);
  const item = (sel: string) => root.querySelector<HTMLButtonElement>(`.ctx-menu ${sel}`)!;
  const pick = (sel: string) =>
    act(async () => {
      item(sel).click();
      await new Promise((r) => setTimeout(r, 10));
    });
  /** The mouse on a family: its versions in a flyout. */
  const hover = (sel: string) => act(async () => void item(sel).dispatchEvent(new MouseEvent('mouseenter')));
  const flyout = () => [...root.querySelectorAll('.ctx-flyout .menu-item')].map((i) => i.querySelector('.menu-label')!.textContent);
  const name = () => root.querySelector<HTMLInputElement>('.ctx-menu input[name=model_name]');
  const type = (value: string) =>
    act(async () => {
      name()!.value = value;
      name()!.dispatchEvent(new Event('input', { bubbles: true }));
      name()!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
      await new Promise((r) => setTimeout(r, 10));
    });

  it('switches the model from its pill: a family picks its alias, a version from its submenu, another name on Enter, the default as null', async () => {
    const { app, api } = setup('running');
    await mount(app);
    // The model of the last turn, as the header named it.
    const pill = $('.chat-meta button.btn-model-pill')!;
    expect(pill.textContent).toBe('Opus 5.5');
    expect(pill.getAttribute('aria-label')).toBe('Model: Opus 5.5');
    expect(pill.classList.contains('pill')).toBe(false);
    await open();
    expect($('.ctx-menu')!.getAttribute('aria-label')).toBe('Model');
    // A family each, its versions in its submenu; no flat list of versions.
    expect(labels()).toEqual(["Claude Code's default", 'Opus', 'Sonnet', 'Haiku', 'Fable', 'Another model…']);
    expect(item('.btn-model-default').getAttribute('aria-checked')).toBe('true');
    expect($('.ctx-menu .btn-model-claude-opus-4-8')).toBeNull();
    await hover('.btn-model-sonnet');
    expect(flyout()).toEqual(['Sonnet 5.5', 'Sonnet 5', 'Sonnet 4.6']);
    await pick('.btn-model-sonnet');
    expect(api.patchSession).toHaveBeenLastCalledWith('s1', { model: 'sonnet' });
    expect(app.sessions.get('s1')!.model).toBe('sonnet');
    expect($('.ctx-menu')).toBeNull();
    expect($('.btn-model-pill')!.textContent).toBe('Sonnet');

    await open();
    await hover('.btn-model-opus');
    expect(flyout()).toEqual(['Opus 5.5', 'Opus 5', 'Opus 4.8', 'Opus 4.7', 'Opus 4.6']);
    await pick('.btn-model-claude-opus-4-8');
    expect(api.patchSession).toHaveBeenLastCalledWith('s1', { model: 'claude-opus-4-8' });
    expect($('.btn-model-pill')!.textContent).toBe('Opus 4.8');

    await open();
    await pick('.btn-model-custom');
    expect(api.patchSession).toHaveBeenCalledTimes(2);
    // An empty name does nothing; one typed goes on Enter, trimmed.
    await type('  ');
    expect(api.patchSession).toHaveBeenCalledTimes(2);
    expect(name()).not.toBeNull();
    await type(' claude-opus-4-1 ');
    expect(api.patchSession).toHaveBeenLastCalledWith('s1', { model: 'claude-opus-4-1' });
    expect($('.ctx-menu')).toBeNull();

    await open();
    await pick('.btn-model-default');
    expect(api.patchSession).toHaveBeenLastCalledWith('s1', { model: null });
  });

  it('marks the family of the model, with the version on it; another name typed is "Another model…"; the entry the session has only closes the menu, a family checked for a version picks its alias', async () => {
    let { app, api } = setup('idle', { model: 'claude-opus-4-8' });
    await mount(app);
    await open();
    expect(item('.btn-model-opus').getAttribute('aria-checked')).toBe('true');
    expect(item('.btn-model-opus .menu-detail').textContent).toBe('4.8');
    expect(item('.btn-model-sonnet').getAttribute('aria-checked')).toBe('false');
    expect(item('.btn-model-sonnet .menu-detail')).toBeNull();
    await hover('.btn-model-opus');
    expect(item('.ctx-flyout .btn-model-claude-opus-4-8').getAttribute('aria-checked')).toBe('true');
    await pick('.ctx-flyout .btn-model-claude-opus-4-8');
    expect($('.ctx-menu')).toBeNull();
    expect(api.patchSession).not.toHaveBeenCalled();
    // The family itself, checked for its version, is its alias.
    await open();
    await pick('.btn-model-opus');
    expect(api.patchSession).toHaveBeenLastCalledWith('s1', { model: 'opus' });
    render(null, root);

    // The alias checks its family without a version; picked again, it only closes the menu.
    ({ app, api } = setup('idle', { model: 'fable' }));
    await mount(app);
    await open();
    expect(item('.btn-model-fable').getAttribute('aria-checked')).toBe('true');
    expect(item('.btn-model-fable .menu-detail')).toBeNull();
    await pick('.btn-model-fable');
    expect($('.ctx-menu')).toBeNull();
    expect(api.patchSession).not.toHaveBeenCalled();
    render(null, root);

    ({ app } = setup('idle', { model: 'claude-sonnet-4-5' }));
    await mount(app);
    expect($('.btn-model-pill')!.textContent).toBe('Sonnet 4.5');
    await open();
    expect(item('.btn-model-custom').getAttribute('aria-checked')).toBe('true');
    expect(items().filter((i) => i.getAttribute('aria-checked') === 'true')).toHaveLength(1);
    await pick('.btn-model-custom');
    expect(name()!.value).toBe('claude-sonnet-4-5');
  });

  it('switches to a provider the session\'s worker allows, after a confirmation, and back to a Claude model', async () => {
    const { app, api } = setup('running');
    app.providers = [
      { id: 'pr1', name: 'qwen-local', label: 'Qwen 3.8 (local)', base_url: 'http://192.168.1.229:8082', model: 'qwen3.8-27b', workers: ['wk1'] },
      { id: 'pr2', name: 'zai', label: 'GLM', base_url: 'https://api.z.ai/api/anthropic', model: 'glm-5', workers: ['wk2'] },
    ];
    const confirm = vi.fn(() => false);
    vi.stubGlobal('confirm', confirm);
    await mount(app);
    await open();
    // Only the worker's providers, under the Claude models.
    expect(labels().slice(-1)).toEqual(['Qwen 3.8 (local)']);
    expect(labels()).not.toContain('GLM');
    // Declined: nothing changes.
    await pick('.btn-model-provider');
    expect(confirm).toHaveBeenCalledTimes(1);
    expect(api.patchSession).not.toHaveBeenCalled();
    confirm.mockReturnValue(true);
    await open();
    await pick('.btn-model-provider');
    expect(api.patchSession).toHaveBeenLastCalledWith('s1', { provider_id: 'pr1' });
    expect(app.sessions.get('s1')!.provider?.label).toBe('Qwen 3.8 (local)');
    // The pill names the provider, and the header shows no cost.
    expect($('.btn-model-pill')!.textContent).toBe('Qwen 3.8 (local)');
    expect($('.chat-cost')).toBeNull();
    await open();
    expect(item('.btn-model-provider').getAttribute('aria-checked')).toBe('true');
    expect(item('.btn-model-opus').getAttribute('aria-checked')).toBe('false');
    // A Claude model leaves the provider in the same PATCH.
    await pick('.btn-model-opus');
    expect(api.patchSession).toHaveBeenLastCalledWith('s1', { provider_id: null, model: 'opus' });
    expect(app.sessions.get('s1')!.provider).toBeUndefined();
    vi.unstubAllGlobals();
  });

  it('gives the agent the browser from the mode pill\'s menu after a confirmation and takes it away, only on a worker that gives one', async () => {
    const { app, api } = setup('running', { effective_permission_mode: 'ask' });
    const confirm = vi.fn(() => false);
    vi.stubGlobal('confirm', confirm);
    const modeMenu = () => act(async () => $('.btn-mode-pill')!.click());
    // A worker without `browser`: no item, the switch alone.
    await mount(app);
    await modeMenu();
    expect($('.ctx-menu .mode-switch')).not.toBeNull();
    expect($('.btn-browser')).toBeNull();
    await modeMenu();
    // GET /api/workers: this worker's worker.toml has `browser = true`.
    Object.assign(api, { workers: vi.fn(async () => [{ id: 'wk1', name: 'drzlo-mac', browser: true }]) });
    await act(() => app.loadWorkers());
    await modeMenu();
    expect($('.ctx-menu .btn-browser-on')!.textContent).toBe('Browser: off · Give the browser');
    expect($('.ctx-menu .btn-browser-on')!.getAttribute('title')).toBe('The agent restarts with your Chrome on its worker');
    // Declined: nothing changes.
    await act(async () => $('.btn-browser-on')!.click());
    expect(confirm).toHaveBeenCalledTimes(1);
    expect(String((confirm.mock.calls[0] as unknown[])[0])).toContain('Give the agent your browser?');
    expect(api.patchSession).not.toHaveBeenCalled();
    confirm.mockReturnValue(true);
    await modeMenu();
    await act(async () => $('.btn-browser-on')!.click());
    expect(api.patchSession).toHaveBeenLastCalledWith('s1', { browser: true });
    expect(app.sessions.get('s1')!.browser).toBe(true);
    // The header says the agent has it.
    expect($('.chat-browser')!.textContent).toBe('Browser');
    expect($('.chat-browser')!.getAttribute('title')).toContain('The agent has your Chrome on');
    // And off again.
    await modeMenu();
    expect($('.btn-browser-off')!.textContent).toBe('Browser: on · Turn off');
    await act(async () => $('.btn-browser-off')!.click());
    expect(String((confirm.mock.calls.at(-1) as unknown[])[0])).toBe('The agent restarts without the browser.');
    expect(api.patchSession).toHaveBeenLastCalledWith('s1', { browser: false });
    expect($('.chat-browser')).toBeNull();
    vi.unstubAllGlobals();
  });

  it('the pill names a model switched to since the last turn at once', async () => {
    const { app } = setup('running', { model: 'fable' });
    await mount(app);
    expect($('.btn-model-pill')!.textContent).toBe('Fable');
    expect(shownModel('opus', 'claude-opus-5-5')).toBe('claude-opus-5-5');
    expect(shownModel('claude-fable-5-1', 'claude-fable-5-1')).toBe('claude-fable-5-1');
    expect(shownModel(undefined, 'claude-opus-5-5')).toBe('claude-opus-5-5');
    expect(shownModel('claude-fable-5-1', 'claude-opus-5-5')).toBe('claude-fable-5-1');
    expect(shownModel('sonnet', undefined)).toBe('sonnet');
    expect(modelName('sonnet')).toBe('Sonnet');
    expect(modelName('qwen3.8-27b:q4_K')).toBe('qwen3.8-27b:q4_K');
  });

  it('says Default before any turn named a model; archived, it is the name alone; a room and the service session have no pills', async () => {
    let { app } = setup('idle', { history: [HISTORY[0]] });
    await mount(app);
    expect($('.btn-model-pill')!.textContent).toBe('Default');
    render(null, root);

    ({ app } = setup('idle'));
    app.sessions.set('s1', { ...app.sessions.get('s1')!, archived: true, state: 'archived' });
    await mount(app);
    expect($('button.btn-model-pill')).toBeNull();
    expect($('span.btn-model-pill')!.textContent).toBe('Opus 5.5');
    expect($('span.btn-model-pill svg')).toBeNull();
    render(null, root);

    ({ app } = setup('idle', { room_mode: 'council', commands: [], history: [HISTORY[0]] }));
    await mount(app);
    expect($('.btn-model-pill, .btn-mode-pill')).toBeNull();
    render(null, root);

    root = document.createElement('div');
    document.body.appendChild(root);
    await act(async () => {
      render(<Chat app={setup('idle').app} sessionId="sys" />, root);
      await new Promise((r) => setTimeout(r, 50));
    });
    expect($('.chat-system-note')).not.toBeNull();
    expect($('.btn-model-pill, .btn-mode-pill')).toBeNull();
    render(null, root);

    // On a phone neither has a line of pills.
    root = document.createElement('div');
    document.body.appendChild(root);
    await act(async () => {
      render(<Chat app={setup('idle').app} sessionId="sys" phone />, root);
      await new Promise((r) => setTimeout(r, 50));
    });
    expect($('.chat-system-note')).not.toBeNull();
    expect($('.chat-pills')).toBeNull();
  });

  it('shows the model the runtime kept once it refused a switch (SPEC 0.179)', async () => {
    const { app, api } = setup('idle', { model: 'claude-nope' });
    await mount(app);
    expect($('.btn-model-pill')!.textContent).toBe('claude-nope');
    // GET /api/sessions/{id}: the model the hub took from the line.
    api.session.mockImplementation(async () => ({ ...app.sessions.get('s1')!, model: 'claude-opus-5-5', commands: undefined }));
    const fetched = api.session.mock.calls.length;
    await act(async () => {
      subscriber!.onEvent(ev('event.system.model_not_switched', 8));
      await new Promise((r) => setTimeout(r, 10));
    });
    expect(api.session).toHaveBeenCalledTimes(fetched + 1);
    expect($('.btn-model-pill')!.textContent).toBe('Opus 5.5');
  });

  it('shows the usage limits of the session\'s worker under the model menu\'s items', async () => {
    const { app, api } = setup('idle');
    const soon = Math.floor(Date.now() / 1000) + 2 * 3600;
    (api as any).workers = vi.fn(async () => [
      { id: 'other', name: 'x', limits: { five_hour: { utilization: 0.99, resets_at: soon }, reported_at: '' } },
      {
        id: 'wk1',
        name: 'drzlo-mac',
        limits: {
          five_hour: { utilization: 0.39, resets_at: soon },
          seven_day: { utilization: 0.934, resets_at: soon + 3 * 86400 },
          reported_at: '2026-11-08T12:00:00Z',
        },
      },
    ]);
    await mount(app);
    await open();
    await act(async () => new Promise((r) => setTimeout(r, 10)));
    const rows = [...root.querySelectorAll('.ctx-menu > .menu-bottom .limit-row')].map((r) => [
      r.querySelector('.limit-name')!.textContent,
      r.querySelector('.limit-pct')!.textContent,
    ]);
    expect(rows).toEqual([
      ['Session', '39%'],
      ['Week', '93%'],
    ]);
    expect(root.querySelectorAll('.limit-high')).toHaveLength(1);
    const week = root.querySelectorAll('.limit-reset')[1].textContent!;
    expect(week).toMatch(/^resets \S+ \d/);
    // Under the items: the last thing in the menu.
    expect($('.ctx-menu')!.lastElementChild!.classList.contains('menu-bottom')).toBe(true);
  });

  it('shows no limits while the worker has reported none', async () => {
    const { app, api } = setup('idle');
    (api as any).workers = vi.fn(async () => [{ id: 'wk1', name: 'drzlo-mac' }]);
    await mount(app);
    await open();
    await act(async () => new Promise((r) => setTimeout(r, 10)));
    expect(root.querySelector('.limits')).toBeNull();
  });

  it('shows a bar above the input at 70 % of a window and when a limit is hit (SPEC 0.179)', async () => {
    const { app, api } = setup('idle');
    const soon = Math.floor(Date.now() / 1000) + 2 * 3600;
    let limits: object = { five_hour: { utilization: 0.39, resets_at: soon }, reported_at: '' };
    (api as any).workers = vi.fn(async () => [{ id: 'wk1', name: 'drzlo-mac', limits }]);
    await mount(app);
    expect($('.limit-notice')).toBeNull();
    // Each change of the session's state reads the limits again.
    const state = async (s: SessionState) => {
      const info = app.sessions.get('s1')!;
      await act(async () =>
        app.mergeSummary([
          { session_id: 's1', project_id: 'p1', title: info.title, state: s, queue_len: 0, unread: 0, last_seq: 7, last_read_seq: 7 },
        ]),
      );
      await act(async () => new Promise((r) => setTimeout(r, 10)));
    };
    limits = {
      five_hour: { utilization: 0.72, resets_at: soon },
      seven_day: { utilization: 0.934, resets_at: soon + 3 * 86400 },
      reported_at: '',
    };
    await state('running');
    expect($('.limit-notice')!.textContent).toMatch(/^You've used 93% of your weekly limit · resets \S+ \d/);
    limits = { ...limits, status: 'rejected', rate_limit_type: 'five_hour', resets_at: soon };
    await state('idle');
    expect($('.limit-notice')!.textContent).toMatch(/^You've hit your 5-hour limit · resets \d/);
    expect($('.limit-notice-hit')).not.toBeNull();
    // Above the subagents, which stay right above the input.
    expect($('.limit-notice + .composer-wrap')).not.toBeNull();
  });

  it("takes the bar's threshold from the hub (SPEC 0.190)", async () => {
    const { app, api } = setup('idle');
    const soon = Math.floor(Date.now() / 1000) + 2 * 3600;
    const limits = { five_hour: { utilization: 0.85, resets_at: soon }, reported_at: '' };
    const workers = vi.fn(async () => [{ id: 'wk1', name: 'drzlo-mac', limits }]);
    (api as any).workers = workers;
    // At 90 % a window at 85 % says nothing.
    app.me = { ...app.me!, limit_notice_percent: 90 };
    await mount(app);
    await act(async () => new Promise((r) => setTimeout(r, 10)));
    expect($('.limit-notice')).toBeNull();
    expect(limitNotice({ ...limits, status: 'rejected', rate_limit_type: 'five_hour', resets_at: soon }, Date.now(), 90)).toMatch(
      /^You've hit your 5-hour limit/,
    );
    expect(limitNotice({ five_hour: { utilization: 0.6, resets_at: soon }, reported_at: '' }, Date.now(), 50)).toMatch(
      /^You've used 60% of your 5-hour limit/,
    );
  });

  it('shows no limit bar on a hub with HQC_LIMIT_NOTICE_PERCENT=0, not even a hit limit (SPEC 0.190)', async () => {
    const { app, api } = setup('idle');
    const soon = Math.floor(Date.now() / 1000) + 2 * 3600;
    const limits = { status: 'rejected', rate_limit_type: 'five_hour', resets_at: soon, reported_at: '' };
    const workers = vi.fn(async () => [{ id: 'wk1', name: 'drzlo-mac', limits }]);
    (api as any).workers = workers;
    app.me = { ...app.me!, limit_notice_percent: 0 };
    await mount(app);
    await act(async () => new Promise((r) => setTimeout(r, 10)));
    expect($('.limit-notice')).toBeNull();
  });

  it('shows no limit bar in a session on a provider, whose model is not on the subscription (SPEC 0.179)', async () => {
    const { app, api } = setup('idle');
    const soon = Math.floor(Date.now() / 1000) + 2 * 3600;
    const limits = { status: 'rejected', rate_limit_type: 'five_hour', resets_at: soon, reported_at: '' };
    (api as any).workers = vi.fn(async () => [{ id: 'wk1', name: 'drzlo-mac', limits }]);
    // On the session object the hub serves too: the chat fetches it again on opening.
    app.sessions.get('s1')!.provider = { id: 'pr1', label: 'GLM' };
    await mount(app);
    await act(async () => new Promise((r) => setTimeout(r, 10)));
    expect($('.limit-notice')).toBeNull();
  });

  it('says nothing of a limit or a window that has reset since it was reported (SPEC 0.179)', () => {
    const now = Date.UTC(2026, 10, 13, 12);
    const past = now / 1000 - 60;
    const soon = now / 1000 + 3600;
    expect(limitNotice({ status: 'rejected', rate_limit_type: 'five_hour', resets_at: past, reported_at: '' }, now)).toBeUndefined();
    expect(limitNotice({ five_hour: { utilization: 0.95, resets_at: past }, reported_at: '' }, now)).toBeUndefined();
    expect(
      limitNotice({ five_hour: { utilization: 0.95, resets_at: past }, seven_day: { utilization: 0.8, resets_at: soon }, reported_at: '' }, now),
    ).toMatch(/^You've used 80% of your weekly limit · resets /);
    // Without a reset time the limit is as current as its report.
    expect(limitNotice({ status: 'rejected', rate_limit_type: 'seven_day_overage_included', reported_at: '' }, now)).toBe(
      "You've hit your Fable limit",
    );
  });

});

// SPEC 0.168: the runtime's reply to a model refusal offers another model while no message of the
// user came after it; the choice switches the session as the model pill does.
describe('a refusal of the model', () => {
  const REFUSAL =
    "API Error: Opus 5.5's safeguards flagged this session. Claude Code can't respond to your last message with Opus 5.5. Try rephrasing the request in a new session or change your model.";
  const reply = (seq: number, refusal = true) =>
    ev('event.agent.message', seq, (e) => (e.payload = { text: REFUSAL, final: true, runtime: true, refusal }));
  const offer = () => $('.line-refusal .btn-switch-model');
  /** The mouse on Opus: its versions in a flyout beside it. */
  const opusVersions = () =>
    act(async () => void root.querySelector('.ctx-menu .btn-model-opus')!.dispatchEvent(new MouseEvent('mouseenter')));

  it('offers "Switch model" under the last one; a version picked switches the session and the line says so', async () => {
    const { app, api } = setup('idle', { history: [HISTORY[0], reply(2)] });
    await mount(app);
    expect($('.line-refusal')!.textContent).toBe(`${REFUSAL}Switch model`);
    await act(async () => offer()!.click());
    const items = [...root.querySelectorAll<HTMLButtonElement>('.ctx-menu .menu-item')].map((b) => b.textContent);
    // The families, each with its versions in a submenu.
    expect(items).toEqual(['Opus', 'Sonnet', 'Haiku', 'Fable', 'Another model…']);
    // A running agent is not switched to the default: it is not offered.
    expect(items).not.toContain("Claude Code's default");
    await opusVersions();
    expect([...root.querySelectorAll('.ctx-flyout .menu-item')].map((b) => b.textContent)).toContain('Opus 4.8');
    await act(async () => {
      root.querySelector<HTMLButtonElement>('.ctx-menu .btn-model-claude-opus-4-8')!.click();
      await new Promise((r) => setTimeout(r, 10));
    });
    expect(api.patchSession).toHaveBeenLastCalledWith('s1', { model: 'claude-opus-4-8' });
    expect($('.ctx-menu')).toBeNull();
    expect(offer()).toBeNull();
    expect($('.line-refusal .refusal-offer')!.textContent).toBe('Switched to Opus 4.8. Send the message again.');
  });

  it('a switch the runtime refused brings the offer back (SPEC 0.179)', async () => {
    const { app } = setup('idle', { history: [HISTORY[0], reply(2)] });
    await mount(app);
    await act(async () => offer()!.click());
    await opusVersions();
    await act(async () => {
      root.querySelector<HTMLButtonElement>('.ctx-menu .btn-model-claude-opus-4-8')!.click();
      await new Promise((r) => setTimeout(r, 10));
    });
    expect($('.line-refusal .refusal-offer')!.textContent).toBe('Switched to Opus 4.8. Send the message again.');
    // The CLI kept its model: the line says so, and the refusal offers another one again.
    await act(async () => {
      subscriber!.onEvent(ev('event.system.model_not_switched', 3));
      await new Promise((r) => setTimeout(r, 10));
    });
    expect(offer()).not.toBeNull();
  });

  it('a refused switch keeps the offer', async () => {
    const { app, api } = setup('idle', { history: [HISTORY[0], reply(2)] });
    api.patchSession.mockRejectedValueOnce(new Error('session is archived'));
    await mount(app);
    await act(async () => offer()!.click());
    await act(async () => {
      root.querySelector<HTMLButtonElement>('.ctx-menu .btn-model-sonnet')!.click();
      await new Promise((r) => setTimeout(r, 10));
    });
    expect($('.notice')!.textContent).toBe('Not changed: session is archived');
    expect(offer()).not.toBeNull();
  });

  it('"Another model…" switches to the name typed; an empty one does nothing', async () => {
    const { app, api } = setup('idle', { history: [HISTORY[0], reply(2)] });
    await mount(app);
    await act(async () => offer()!.click());
    await act(async () => root.querySelector<HTMLButtonElement>('.ctx-menu .btn-model-custom')!.click());
    const name = () => root.querySelector<HTMLInputElement>('.ctx-menu input[name=model_name]');
    const enter = (value: string) =>
      act(async () => {
        name()!.value = value;
        name()!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
        await new Promise((r) => setTimeout(r, 10));
      });
    await enter('  ');
    expect(api.patchSession).not.toHaveBeenCalled();
    expect(name()).not.toBeNull();
    await enter(' claude-sonnet-4-5 ');
    expect(api.patchSession).toHaveBeenLastCalledWith('s1', { model: 'claude-sonnet-4-5' });
    expect($('.ctx-menu')).toBeNull();
    expect($('.refusal-offer')!.textContent).toBe('Switched to Sonnet 4.5. Send the message again.');
  });

  it('a provider switches the session to it after the confirmation, and the line names it', async () => {
    const { app, api } = setup('idle', { history: [HISTORY[0], reply(2)] });
    app.providers = [
      { id: 'pr1', name: 'qwen-local', label: 'Qwen 3.8 (local)', base_url: 'http://192.168.1.229:8082', model: 'qwen3.8-27b', workers: ['wk1'] },
    ];
    vi.stubGlobal('confirm', vi.fn(() => true));
    try {
      await mount(app);
      await act(async () => offer()!.click());
      await act(async () => {
        root.querySelector<HTMLButtonElement>('.ctx-menu .btn-model-provider')!.click();
        await new Promise((r) => setTimeout(r, 10));
      });
      expect(api.patchSession).toHaveBeenLastCalledWith('s1', { provider_id: 'pr1' });
      expect($('.refusal-offer')!.textContent).toBe('Switched to Qwen 3.8 (local). Send the message again.');
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it('the session\'s model is checked, and picking it only closes the menu', async () => {
    const { app, api } = setup('idle', { history: [HISTORY[0], reply(2)], model: 'claude-opus-4-8' });
    await mount(app);
    await act(async () => offer()!.click());
    expect(root.querySelector('.ctx-menu .btn-model-opus')!.getAttribute('aria-checked')).toBe('true');
    await opusVersions();
    const checked = root.querySelector<HTMLButtonElement>('.ctx-menu .btn-model-claude-opus-4-8')!;
    expect(checked.getAttribute('aria-checked')).toBe('true');
    await act(async () => {
      checked.click();
      await new Promise((r) => setTimeout(r, 10));
    });
    expect(api.patchSession).not.toHaveBeenCalled();
    expect($('.ctx-menu')).toBeNull();
    expect(offer()).not.toBeNull();
  });

  it('none while VS Code holds the session; once it is taken over, the offer is there', async () => {
    const { app } = setup('locked_vscode', { history: [HISTORY[0], reply(2)] });
    await mount(app);
    expect($('.line-runtime')!.textContent).toBe(REFUSAL);
    expect(offer()).toBeNull();
    await act(async () =>
      app.mergeSummary([
        { session_id: 's1', project_id: 'p1', title: 'test 12', state: 'idle', queue_len: 0, unread: 0, last_seq: 7, last_read_seq: 7 },
      ]),
    );
    expect(offer()).not.toBeNull();
  });

  it('the offer goes once the user writes after it; a runtime reply that is no refusal has none', async () => {
    const { app } = setup('idle', { history: [HISTORY[0], reply(2), reply(3, false)] });
    await mount(app);
    const lines = () => [...root.querySelectorAll('.line-runtime')].map((l) => [l.textContent, !!l.querySelector('.btn-switch-model')]);
    expect(lines()).toEqual([
      [`${REFUSAL}Switch model`, true],
      [REFUSAL, false],
    ]);
    await act(async () => subscriber!.onEvent(HISTORY[3]));
    expect(lines()).toEqual([
      [REFUSAL, false],
      [REFUSAL, false],
    ]);
  });

  it('none over a message that waited through the refused turn: it stands under the line', async () => {
    const waited = ev('event.user.message', 2, (e) => {
      e.event_id = '0199f3a2-7c10-7000-8000-000000000330';
      e.client_op_id = 'op-2';
      e.payload = { text: 'sent meanwhile', artifact_ids: [] };
      e.delivery = 'pending';
      delete e.delivered_at;
    });
    const refused = ev('event.agent.message', 3, (e) => {
      e.payload = { text: REFUSAL, final: true, runtime: true, refusal: true, in_reply_to: HISTORY[0].event_id };
    });
    const { app } = setup('running', { history: [HISTORY[0], waited, refused] });
    await mount(app);
    const line = $('.line-runtime')!;
    const message = [...root.querySelectorAll('.msg-user')].find((m) => m.textContent?.includes('sent meanwhile'))!;
    expect(line.compareDocumentPosition(message) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(line.textContent).toBe(REFUSAL);
    expect(offer()).toBeNull();
  });
});

describe('header', () => {
  it('shows the subagents running next to the state, only while there are any', async () => {
    let { app } = setup('running', { subagents: 2 });
    await mount(app);
    expect($('.chat-agents')!.textContent).toBe('2 agents');
    expect($('.pill + .chat-agents')).not.toBeNull();
    const s = app.sessions.get('s1')!;
    const summary = (subagents?: number) =>
      app.mergeSummary([
        { session_id: 's1', project_id: 'p1', title: s.title, state: 'running', queue_len: 0, unread: 0, last_seq: 7, last_read_seq: 7, subagents },
      ]);
    await act(async () => summary(1));
    expect($('.chat-agents')!.textContent).toBe('1 agent');
    await act(async () => summary(undefined));
    expect($('.chat-agents')).toBeNull();
    render(null, root);

    ({ app } = setup('running', { subagents: 0 }));
    await mount(app);
    expect($('.chat-agents')).toBeNull();
  });

  it('says Running while a background subagent works after the turn ended', async () => {
    let { app } = setup('idle', { subagents: 1 });
    await mount(app);
    expect($('.pill')!.textContent).toBe('Running');
    expect($('.pill.state-running .pulse')).not.toBeNull();
    // No turn for Stop to stop, and Stop spares the subagent; Pause ends it with the process.
    expect(button('Stop')).toBeUndefined();
    expect($('.btn-pause')).not.toBeNull();
    render(null, root);

    ({ app } = setup('idle', { subagents: 0 }));
    await mount(app);
    expect($('.pill')!.textContent).toBe('Idle');
    expect($('.pill .pulse')).toBeNull();
    expect($('.btn-pause')).toBeNull();
  });

  it('says "Pausing" while a paused session\'s turn runs, Paused once it is paused', async () => {
    let { app } = setup('running', { paused_at: '2026-11-15T09:00:00Z' });
    await mount(app);
    expect($('.pill')!.textContent).toBe('Pausing');
    render(null, root);

    ({ app } = setup('paused', { paused_at: '2026-11-15T09:00:00Z' }));
    await mount(app);
    expect($('.pill')!.textContent).toBe('Paused');
  });

  it('says Running while a background task runs after the turn ended, Idle once it ends', async () => {
    const { app } = setup('idle', { background_tasks: 1 });
    await mount(app);
    expect($('.pill')!.textContent).toBe('Running');
    expect($('.chat-agents')).toBeNull();
    expect($('.btn-pause')).not.toBeNull();
    const s = app.sessions.get('s1')!;
    await act(async () =>
      app.mergeSummary([
        { session_id: 's1', project_id: 'p1', title: s.title, state: 'idle', queue_len: 0, unread: 0, last_seq: 7, last_read_seq: 7 },
      ]),
    );
    expect($('.pill')!.textContent).toBe('Idle');
    expect($('.btn-pause')).toBeNull();
  });

  it('offers "Run in background" for a running call the worker reports in the foreground', async () => {
    // SPEC 0.125: the running `ls /` is `toolu_01`; a summary without it takes the button away.
    const { api, app } = setup('running', { history: HISTORY.slice(0, 5), foreground_calls: ['toolu_01'] });
    await mount(app);
    await act(async () => $('button[aria-label="Run in background"]')!.click());
    expect(api.background).toHaveBeenCalledWith('s1', 'toolu_01');
    const s = app.sessions.get('s1')!;
    await act(async () =>
      app.mergeSummary([
        { session_id: 's1', project_id: 'p1', title: s.title, state: 'running', queue_len: 0, unread: 0, last_seq: 7, last_read_seq: 7 },
      ]),
    );
    expect($('button[aria-label="Run in background"]')).toBeNull();
  });

  it("shows what the agent waits on from the session's summary, until a summary without it", async () => {
    // SPEC 0.179: `activity` comes and goes with the summaries the hub pushes.
    const { app } = setup('running', { history: [] });
    await mount(app);
    expect($('.live-agents-row')!.textContent).toBe('Main agent: Working');
    const s = app.sessions.get('s1')!;
    const summary = { session_id: 's1', project_id: 'p1', title: s.title, state: 'running' as const, queue_len: 0, unread: 0, last_seq: 7, last_read_seq: 7 };
    await act(async () =>
      app.mergeSummary([
        { ...summary, activity: { kind: 'retrying', attempt: 2, max_retries: 10, retry_delay_ms: 4000, error_status: 529 } },
      ]),
    );
    expect($('.live-agents-row')!.textContent).toBe('Main agent: Retrying 2/10 (529), next in 4 s');
    await act(async () => app.mergeSummary([summary]));
    expect($('.live-agents-row')!.textContent).toBe('Main agent: Working');
  });

  it('shows the state, the mode and model pills, the worker, path and cost of the session, "Stop" and "More"', async () => {
    const { app } = setup('waiting', { effective_permission_mode: 'ask' });
    await mount(app);
    expect($('.chat-title')!.textContent).toBe('test 12');
    expect($('.pill')!.textContent).toBe('Waiting for permission');
    expect($('.pill .pulse')).toBeNull();
    // The pills next to the state; the worker after them reads as where the session runs.
    expect([...$('.chat-meta')!.children].map((c) => c.classList[0])).toEqual([
      'state',
      'btn-mode-pill',
      'btn-model-pill',
      'chat-worker',
      'sep',
      'chat-path',
      'sep',
      'chat-cost',
    ]);
    expect($('.chat-worker')!.textContent).toBe('on drzlo-mac');
    expect($('.chat-path .mono')!.textContent).toBe('~/work/hqc');
    // The row is one line and the path may end in an ellipsis: the whole of it on hover.
    expect($('.chat-path .mono')!.getAttribute('title')).toBe('~/work/hqc');
    expect($('.chat-path button')!.getAttribute('aria-label')).toBe('Copy path');
    // The model of the last usage that names one, in its pill; the sum of cost_usd.
    expect($('.btn-mode-pill')!.textContent).toBe('Ask');
    expect($('.btn-model-pill')!.textContent).toBe('Opus 5.5');
    expect($('.chat-cost')!.textContent).toBe('$0.07 this session');
    expect(button('Stop')).toBeDefined();
    await act(async () => $('.btn-more')!.click());
    // T50: archiving is on the list's rows, not in the header; the title is renamed in place, the
    // mode and the model have their pills.
    expect([...root.querySelectorAll('.menu-item')].map((b) => b.textContent)).toEqual([
      'Schedules',
      'Move to project…',
      'Folder rules',
      'Restart session',
    ]);
  });

  it('shows an archived session read-only: the chip, the band with the time, a dimmed feed, no input (T50)', async () => {
    const { app } = setup('idle');
    const at = new Date();
    at.setHours(23, 45, 0, 0);
    app.sessions.set('s1', { ...app.sessions.get('s1')!, archived: true, state: 'archived', archived_at: at.toISOString() });
    await mount(app);
    expect($('.pill')!.textContent).toBe('Archived');
    expect($('.archived-band')!.textContent).toBe(
      'Archived today at 23:45 · read-only. Restore or delete it from the list on the left.',
    );
    expect($('.chat.chat-archived .feed')).not.toBeNull();
    expect($('.chat textarea')).toBeNull();
    await act(async () => $('.btn-more')!.click());
    expect([...root.querySelectorAll('.menu-item')].map((b) => b.textContent)).toEqual(['Schedules', 'Folder rules']);
    // Read-only: its title is not renamed.
    expect($('.btn-rename-title')).toBeNull();
    render(null, root);

    // Archived before the hub kept the time: no time.
    app.sessions.set('s1', { ...app.sessions.get('s1')!, archived_at: undefined });
    await mount(app, true);
    expect($('.archived-band')!.textContent).toBe('Archived · read-only. Restore or delete it from the archive.');
  });

  it('links to the session that started it with hqc_spawn (T34)', async () => {
    let { app } = setup('idle');
    await mount(app);
    expect($('.chat-spawned')).toBeNull();
    render(null, root);
    ({ app } = setup('idle', { spawned_by: { session_id: 's0', address: 'hqc/planner' } }));
    await mount(app);
    expect($('.chat-spawned')!.textContent).toBe('started by hqc/planner');
    expect($('.chat-spawned a')!.getAttribute('href')).toBe('#/s/s0');
    expect($('.chat-spawned a')!.getAttribute('title')).toBe('hqc/planner');
    expect($('.chat-meta .chat-spawned'), 'in the meta row on a computer').not.toBeNull();
    // "started by <address> on <worker>": the machine after the address says where it runs.
    expect($('.chat-meta .chat-spawned + .chat-worker')!.textContent).toBe('on drzlo-mac');
  });

  it('on a phone "started by" stands after the pills, on their line, not in the meta row nor on a line of its own', async () => {
    let { app } = setup('running', { spawned_by: { session_id: 's0', address: 'mpv-sendspin/session-4' } });
    await mount(app, true);
    expect($('.chat-meta .chat-spawned')).toBeNull();
    expect($('.chat-head > .chat-spawned')).toBeNull();
    expect([...$('.chat-head')!.children].map((c) => c.classList[0])).toEqual(['chat-head-top', 'chat-meta', 'chat-pills']);
    expect([...$('.chat-pills')!.children].map((c) => c.classList[0])).toEqual(['btn-mode-pill', 'btn-model-pill', 'chat-spawned']);
    const by = $('.chat-pills > .chat-spawned')!;
    expect(by.textContent).toBe('started by mpv-sendspin/session-4');
    expect(by.querySelector('a')!.getAttribute('href')).toBe('#/s/s0');
    render(null, root);

    // Without pills (a room an agent made) "started by" is the line alone, under the room's
    // agents, which stay right under its state.
    ({ app } = setup('idle', { room_mode: 'council', commands: [], history: [HISTORY[0]], spawned_by: { session_id: 's0', address: 'hqc/planner' } }));
    const room = app.sessions.get('s1')!;
    app.sessions.set('m1', { ...room, id: 'm1', title: 'Opus', slug: 'opus', room_mode: undefined, spawned_by: undefined, room_id: 's1', state: 'idle' });
    await mount(app, true);
    expect([...$('.chat-head')!.children].map((c) => c.classList[0])).toEqual(['chat-head-top', 'chat-meta', 'chat-room-agents', 'chat-pills']);
    expect([...$('.chat-pills')!.children].map((c) => c.classList[0])).toEqual(['chat-spawned']);
  });

  it('a room: its mode in place of the pills, its agents linked with their states, an agent\'s post as its message; no model or permission switch', async () => {
    const post = ev('event.peer.message', 2, (e) => {
      e.payload = { from_session_id: 'm1', from: 'glazes/opus', mode: 'send', text: 'Cone 6 is **fine**' };
      delete e.chain;
    });
    const second = ev('event.peer.message', 3, (e) => {
      e.payload = { from_session_id: 'm2', from: 'glazes/sonnet', mode: 'send', text: 'Agreed' };
      delete e.chain;
    });
    const { app } = setup('idle', { room_mode: 'council', commands: [], history: [HISTORY[0], post, second] });
    app.projects.set('p1', { id: 'p1', name: 'Glazes', slug: 'glazes', description: '', is_personal: false, color: 'blue', quick_messages: [], created_at: '2026-10-20T10:00:00Z' });
    const room = app.sessions.get('s1')!;
    const member = (id: string, title: string, more: Partial<SessionInfo>) =>
      app.sessions.set(id, { ...room, id, title, slug: title.toLowerCase(), room_mode: undefined, room_id: 's1', ...more });
    member('m1', 'Opus', { state: 'waiting', model: 'claude-opus-5-5' });
    member('m2', 'Sonnet', { state: 'idle', subagents: 1, model: 'claude-sonnet-5-5' });
    member('m9', 'Elsewhere', { room_id: 'r9' });
    await mount(app);
    expect($('.chat-room')!.textContent).toBe('Room · Council');
    expect($('.btn-mode-pill, .btn-model-pill')).toBeNull();
    expect([...root.querySelectorAll('.chat-room-agent')].map((a) => [a.textContent, a.getAttribute('href')])).toEqual([
      ['Opus · Waiting for reply', '#/s/m1'],
      ['Sonnet · Running', '#/s/m2'],
    ]);
    // The agent's post in the room: a message from its session, linked. The first chat of a
    // run renders its history later than the 50 ms `mount` waits.
    for (let i = 0; i < 20 && !$('.room-post'); i++) await act(async () => void (await new Promise((r) => setTimeout(r, 10))));
    // A chat message of the agent, not a relay from another session.
    expect($('.relay')).toBeNull();
    for (let i = 0; i < 20 && root.querySelectorAll('.room-post').length < 2; i++) await act(async () => void (await new Promise((r) => setTimeout(r, 10))));
    const names = [...root.querySelectorAll<HTMLElement>('.room-post-name')];
    expect(names.map((n) => n.textContent)).toEqual(['Opus', 'Sonnet']);
    // Each agent has a color of its own; the model is muted beside the name, if it has one.
    expect(names[0].style.color).not.toBe('');
    expect(names[0].style.color).not.toBe(names[1].style.color);
    expect([...root.querySelectorAll('.room-post-model')].map((m) => m.textContent)).toEqual(['Opus 5.5', 'Sonnet 5.5']);
    expect($('.room-post .text strong')!.textContent).toBe('fine');
    // No "More": no schedules or move for a room, and its title is renamed in place.
    expect($('.btn-more')).toBeNull();
    expect($('.btn-rename-title')).not.toBeNull();
    render(null, root);

    // A phone's header says the same: the room's mode on the state's line, no line of pills, its
    // agents' line.
    await mount(app, true);
    expect($('.chat-meta > .chat-room')!.textContent).toBe('Room · Council');
    expect([...$('.chat-head')!.children].map((c) => c.classList[0])).toEqual(['chat-head-top', 'chat-meta', 'chat-room-agents']);
    expect($('.chat-pills')).toBeNull();
    expect(root.querySelectorAll('.chat-room-agent')).toHaveLength(2);
  });

  it("a room's pill has its agents' state, as the list does; Stop stops them while one runs or waits", async () => {
    const { app, api } = setup('idle', { room_mode: 'conversation', commands: [], history: [HISTORY[0]] });
    const room = app.sessions.get('s1')!;
    const member = (id: string, state: SessionState) =>
      app.sessions.set(id, { ...room, id, title: id, slug: id, room_mode: undefined, room_id: 's1', state });
    member('m1', 'running');
    member('m2', 'idle');
    await mount(app);
    expect($('.pill')!.textContent).toBe('Running');
    expect($('.pill')!.classList.contains('state-running')).toBe(true);
    // The room's own state is idle; the agents' decide.
    expect(app.sessions.get('s1')!.state).toBe('idle');
    await act(async () => $('.btn-stop')!.click());
    expect(api.stop).toHaveBeenCalledWith('s1');
    render(null, root);
    // One of them waits for the owner: the room does.
    member('m2', 'waiting');
    await mount(app);
    expect($('.pill')!.textContent).toBe('Waiting for reply');
    expect($('.btn-stop')).not.toBeNull();
    render(null, root);
    // Nobody works: nothing to stop.
    member('m1', 'idle');
    member('m2', 'idle');
    await mount(app);
    expect($('.btn-stop')).toBeNull();
  });

  it("a room's live block has a row for each agent at work, in the agents' order, with the command it runs", async () => {
    const { app, api } = setup('idle', { room_mode: 'conversation', commands: [], history: [HISTORY[0]] });
    const room = app.sessions.get('s1')!;
    const member = (id: string, title: string, state: SessionState) =>
      app.sessions.set(id, { ...room, id, title, slug: id, room_mode: undefined, room_id: 's1', state });
    // Registered out of order: the lines go by id.
    member('m3', 'Haiku', 'queued');
    member('m2', 'Sonnet', 'running');
    member('m1', 'Opus', 'running');
    member('m4', 'Fable', 'idle');
    // Opus runs a command (for a minute); the others have no call open.
    // Opus has a long history: more than the newest page.
    const own = (id: string) => (id === 'm1' ? [...Array.from({ length: 150 }, (_, i) => ev('event.agent.message', i + 1)), ev('event.tool.call', 151, (e) => {
      e.ts = new Date(Date.now() - 60_000).toISOString();
      e.payload = { call_id: 'c1', tool: 'Bash', args_preview: '{"command":"make test"}' };
    })] : id === 's1' ? [HISTORY[0]] : []);
    const page = (id: string, after: number, before = Number.MAX_SAFE_INTEGER, limit = 200) => {
      const events = own(id).filter((e) => e.seq! > after && e.seq! < before).slice(-limit);
      const seqs = events.map((e) => e.seq!);
      return { events, invalid: [], maxSeq: Math.max(0, ...seqs), minSeq: seqs[0], count: events.length };
    };
    api.events.mockImplementation(async (id: string, after: number) => page(id, after));
    api.eventsBefore.mockImplementation(async (id: string, before?: number, limit?: number) => page(id, 0, before, limit));
    await mount(app);
    const lead = () => [...root.querySelectorAll('.live-agents-row[role="status"]')];
    for (let i = 0; i < 20 && lead().length < 3; i++) await act(async () => void (await new Promise((r) => setTimeout(r, 10))));
    const lines = lead();
    expect(lines.map((l) => l.textContent!.replace(/\d+m \d+s$/, 'M:SS'))).toEqual(['Opus · Command: make testM:SS', 'Sonnet · Working', 'Haiku · Input queued']);
    // The room's own row is gone: it is never at work itself. The rows stand in the block, not the feed.
    expect(root.querySelectorAll('.feed .live-agents-row')).toHaveLength(0);
    // The agents' chats load the newest page only, not their older history.
    expect(api.eventsBefore.mock.calls.filter(([id]) => id === 'm1')).toHaveLength(1);
    // The agents' chats stay open while the room's does, and close with it.
    expect(app.chat('m1').isOpen).toBe(true);
    await act(async () => render(null, root));
    expect(app.chat('m1').isOpen).toBe(false);
  });

  it('a room has no "More" on a computer, nor on a phone with nothing to show; there Retry brings it; an archived room has none', async () => {
    const { app, api } = setup('idle', { room_mode: 'council', commands: [], history: [HISTORY[0]] });
    app.sessions.set('s1', { ...app.sessions.get('s1')!, workspace_id: undefined, worker_id: undefined });
    // The hub's answer about the session, which an idle one asks for, has no workspace either.
    api.session.mockImplementation(async () => ({ ...app.sessions.get('s1')! }));
    const items = () => [...root.querySelectorAll('.menu-item')].map((b) => b.textContent);
    await mount(app);
    expect($('.btn-more')).toBeNull();
    render(null, root);
    await mount(app, true);
    expect($('.btn-more')).toBeNull();
    render(null, root);
    app.sessions.set('s1', { ...app.sessions.get('s1')!, state: 'error' });
    await mount(app, true);
    await act(async () => $('.btn-more')!.click());
    expect(items()).toEqual(['Retry']);
    render(null, root);
    app.sessions.set('s1', { ...app.sessions.get('s1')!, archived: true, state: 'archived' });
    await mount(app);
    expect($('.btn-more')).toBeNull();
  });

  it('renames in place: the pencil after the title opens a field; Enter or a press away saves the name trimmed, Esc keeps the old one, an empty or unchanged one sends nothing; a refusal goes to the notice', async () => {
    const { app, api } = setup('idle');
    api.patchSession.mockImplementation(async (_id: string, patch: SessionPatch) => ({ ...app.sessions.get('s1')!, title: patch.title ?? '' }) as never);
    await mount(app);
    const pencil = () => $('.chat-title .btn-rename-title')!;
    const field = () => root.querySelector<HTMLInputElement>('.chat-title input.chat-title-input');
    const edit = async (value: string) => {
      await act(async () => pencil().click());
      await act(async () => {
        field()!.value = value;
        field()!.dispatchEvent(new Event('input', { bubbles: true }));
      });
    };
    const key = (k: string) =>
      act(async () => {
        const e = new KeyboardEvent('keydown', { key: k, bubbles: true, cancelable: true });
        field()!.dispatchEvent(e);
        // Handled by the field: a request's card in the chat skips a prevented Enter or Esc.
        expect(e.defaultPrevented).toBe(true);
        await new Promise((r) => setTimeout(r, 10));
      });
    expect(pencil().getAttribute('aria-label')).toBe('Rename');
    await act(async () => pencil().click());
    // The field in the title's place, with the title, selected for typing.
    expect(field()!.value).toBe('test 12');
    expect(document.activeElement).toBe(field());
    expect($('.chat-title .btn-rename-title')).toBeNull();
    await act(async () => {
      field()!.value = 'Other';
      field()!.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await key('Escape');
    expect(field()).toBeNull();
    expect($('.chat-title')!.textContent).toBe('test 12');
    await edit('   ');
    await key('Enter');
    await edit('test 12 ');
    await key('Enter');
    expect(api.patchSession).not.toHaveBeenCalled();
    await edit('  Network sync ');
    await key('Enter');
    expect(api.patchSession).toHaveBeenCalledTimes(1);
    expect(api.patchSession).toHaveBeenCalledWith('s1', { title: 'Network sync' });
    expect(field()).toBeNull();
    expect($('.chat-title')!.textContent).toBe('Network sync');
    // A press away saves too, once.
    await edit('Net');
    await act(async () => {
      field()!.dispatchEvent(new FocusEvent('blur'));
      await new Promise((r) => setTimeout(r, 10));
    });
    expect(api.patchSession).toHaveBeenCalledTimes(2);
    expect(api.patchSession).toHaveBeenLastCalledWith('s1', { title: 'Net' });
    api.patchSession.mockRejectedValueOnce(new Error('the title is taken'));
    await edit('Other');
    await key('Enter');
    expect($('.notice')!.textContent).toBe('Not renamed: the title is taken');
    // A title that changes while the field is open (the agent names the session) is not put
    // back by a press away with the field unchanged.
    const calls = api.patchSession.mock.calls.length;
    await act(async () => pencil().click());
    expect(field()!.value).toBe('Net');
    await act(async () => {
      app.sessions.set('s1', { ...app.sessions.get('s1')!, title: 'Named by the agent' });
      (app as unknown as { changed(): void }).changed();
    });
    await act(async () => {
      field()!.dispatchEvent(new FocusEvent('blur'));
      await new Promise((r) => setTimeout(r, 10));
    });
    expect(api.patchSession).toHaveBeenCalledTimes(calls);
    expect($('.chat-title')!.textContent).toBe('Named by the agent');
    // Not in "More" any more.
    await act(async () => $('.btn-more')!.click());
    expect($('.menu .btn-rename')).toBeNull();
  });

  it('on a phone a tap on the title edits it; a room is renamed in place too', async () => {
    const { app, api } = setup('idle', { room_mode: 'council', commands: [], history: [HISTORY[0]] });
    api.patchSession.mockImplementation(async (_id: string, patch: SessionPatch) => ({ ...app.sessions.get('s1')!, title: patch.title ?? '' }) as never);
    await mount(app, true);
    // No pencil on a phone: the title itself takes the tap.
    expect($('.btn-rename-title')).toBeNull();
    await act(async () => $('.chat-title')!.click());
    const field = root.querySelector<HTMLInputElement>('.chat-title input.chat-title-input')!;
    expect(field.value).toBe('test 12');
    await act(async () => {
      field.value = ' Glaze review ';
      field.dispatchEvent(new Event('input', { bubbles: true }));
      field.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
      await new Promise((r) => setTimeout(r, 10));
    });
    expect(api.patchSession).toHaveBeenCalledWith('s1', { title: 'Glaze review' });
    expect($('.chat-title')!.textContent).toBe('Glaze review');
    render(null, root);

    // An archived one: a tap does nothing.
    app.sessions.set('s1', { ...app.sessions.get('s1')!, archived: true, state: 'archived' });
    await mount(app, true);
    await act(async () => $('.chat-title')!.click());
    expect($('.chat-title input')).toBeNull();
  });

  it("a room's header shows the sum of its agents' costs as \"this room\", not an agent on a provider's; the phone's \"More\" too", async () => {
    const { app, api } = setup('idle', { room_mode: 'council', commands: [], history: [HISTORY[0]] });
    const room = app.sessions.get('s1')!;
    const member = (id: string, more: Partial<SessionInfo> = {}) =>
      app.sessions.set(id, { ...room, id, title: id, slug: id, room_mode: undefined, room_id: 's1', ...more });
    member('m1');
    member('m2');
    member('m3', { provider: { id: 'pr1', label: 'Qwen' } });
    const spent = (id: string, usd: number) => [ev('event.usage', 1, (e) => ((e.session_id = id), (e.payload = { input_tokens: 1, output_tokens: 1, cost_usd: usd })))];
    const own = (id: string) => (id === 'm1' ? spent(id, 0.3) : id === 'm2' ? spent(id, 0.2) : id === 'm3' ? spent(id, 5) : id === 's1' ? [HISTORY[0]] : []);
    const page = (id: string) => {
      const events = own(id);
      return { events, invalid: [], maxSeq: events.length ? 1 : 0, minSeq: events.length ? 1 : 0, count: events.length };
    };
    api.events.mockImplementation(async (id: string) => page(id));
    api.eventsBefore.mockImplementation(async (id: string) => page(id));
    const waitCost = async () => {
      for (let i = 0; i < 20 && !$('.chat-cost'); i++) await act(async () => void (await new Promise((r) => setTimeout(r, 10))));
    };
    await mount(app);
    await waitCost();
    expect($('.chat-cost')!.textContent).toBe('$0.50 this room');
    render(null, root);
    await mount(app, true);
    await act(async () => void (await new Promise((r) => setTimeout(r, 50))));
    await act(async () => $('.btn-more')!.click());
    expect($('.menu-info')!.textContent).toContain('$0.50 this room');
  });

  it('pulses while a turn is on, no "Stop" when idle', async () => {
    let { app } = setup('running');
    await mount(app);
    expect($('.pill')!.textContent).toBe('Running');
    expect($('.pill .pulse')).not.toBeNull();
    render(null, root);
    ({ app } = setup('idle', { history: HISTORY.slice(0, 3) }));
    await mount(app);
    expect($('.pill')!.textContent).toBe('Idle');
    expect(button('Stop')).toBeUndefined();
  });

  it('offers Pause right before Stop while the agent works, on a computer and a phone; not when idle, archived or in a room', async () => {
    const wait = () => act(async () => void (await new Promise((r) => setTimeout(r, 10))));
    for (const state of ['running', 'waiting', 'queued'] as const) {
      const { app, api } = setup(state);
      await mount(app);
      const pause = $('.btn-pause')!;
      expect(pause.textContent).toBe('Pause');
      // The pause stops a running turn at once (SPEC 0.191), not after it.
      expect(pause.title).toBe('The agent stops now; messages wait for Resume');
      expect(pause.nextElementSibling!.classList.contains('btn-stop')).toBe(true);
      await act(async () => pause.click());
      expect(api.pauseSession).toHaveBeenCalledWith('s1');
      render(null, root);
      await mount(app, true);
      expect($('.btn-pause')!.getAttribute('aria-label')).toBe('Pause session');
      expect($('.btn-pause')!.nextElementSibling!.classList.contains('btn-stop')).toBe(true);
      render(null, root);
    }
    let { app, api } = setup('idle');
    await mount(app);
    expect($('.btn-pause')).toBeNull();
    render(null, root);
    ({ app } = setup('running', { room_mode: 'conversation', commands: [], history: [HISTORY[0]] }));
    await mount(app);
    expect($('.btn-pause')).toBeNull();
    render(null, root);
    ({ app } = setup('running'));
    app.sessions.set('s1', { ...app.sessions.get('s1')!, archived: true });
    await mount(app);
    expect($('.btn-pause')).toBeNull();
    render(null, root);

    // A refusal goes to the chat's notice.
    ({ app, api } = setup('running'));
    api.pauseSession.mockRejectedValueOnce(new Error('the worker is offline'));
    await mount(app);
    await act(async () => $('.btn-pause')!.click());
    await wait();
    expect($('.notice')!.textContent).toBe('Not paused: the worker is offline');
  });

  it("a paused session has Resume in Pause's place, before Stop while its turn still runs; not when archived", async () => {
    let { app, api } = setup('running', { paused_at: '2026-11-15T09:00:00Z' });
    await mount(app);
    expect($('.btn-pause')).toBeNull();
    const resume = $('.btn-resume')!;
    expect(resume.textContent).toBe('Resume');
    expect(resume.nextElementSibling!.classList.contains('btn-stop')).toBe(true);
    api.resumeSession.mockRejectedValueOnce(new Error('the session is not paused'));
    await act(async () => {
      resume.click();
      await new Promise((r) => setTimeout(r, 10));
    });
    expect(api.resumeSession).toHaveBeenCalledWith('s1');
    expect($('.notice')!.textContent).toBe('Not resumed: the session is not paused');
    render(null, root);

    ({ app, api } = setup('paused', { paused_at: '2026-11-15T09:00:00Z' }));
    await mount(app, true);
    expect($('.btn-resume')!.getAttribute('aria-label')).toBe('Resume');
    expect($('.btn-stop')).toBeNull();
    await act(async () => $('.btn-resume')!.click());
    expect(api.resumeSession).toHaveBeenCalledWith('s1');
    render(null, root);

    app.sessions.set('s1', { ...app.sessions.get('s1')!, archived: true, state: 'archived' });
    await mount(app);
    expect($('.btn-resume')).toBeNull();
    expect($('.btn-pause')).toBeNull();
  });

  it('"Restart session" in "More" restarts the agent, a refusal to the notice; not for a paused session or one VS Code holds', async () => {
    let { app, api } = setup('idle');
    await mount(app);
    const items = () => [...root.querySelectorAll('.menu-item')].map((b) => b.textContent);
    await act(async () => $('.btn-more')!.click());
    expect(items()).toContain('Restart session');
    await act(async () => $('.menu .btn-restart')!.click());
    expect(api.restartSession).toHaveBeenCalledWith('s1');
    expect($('.menu')).toBeNull();
    api.restartSession.mockRejectedValueOnce(new Error('the worker is offline'));
    await act(async () => $('.btn-more')!.click());
    await act(async () => {
      $('.menu .btn-restart')!.click();
      await new Promise((r) => setTimeout(r, 10));
    });
    expect($('.notice')!.textContent).toBe('Not restarted: the worker is offline');
    render(null, root);

    ({ app } = setup('paused', { paused_at: '2026-11-15T09:00:00Z' }));
    await mount(app);
    await act(async () => $('.btn-more')!.click());
    expect(items()).not.toContain('Restart session');
    render(null, root);

    ({ app } = setup('locked_vscode', { held_by: { entrypoint: 'claude-vscode', status: 'idle' } }));
    await mount(app);
    await act(async () => $('.btn-more')!.click());
    expect(items()).not.toContain('Restart session');
  });

  it('"Folder rules" is a link in "More" to the rules of the session\'s folder, not a button of the header', async () => {
    const { app } = setup('idle');
    await mount(app);
    expect(root.querySelector('.btn-folder-rules')).toBeNull();
    await act(async () => $('.btn-more')!.click());
    const link = root.querySelector<HTMLAnchorElement>('.menu a.btn-folder-rules')!;
    expect(link.textContent).toBe('Folder rules');
    expect(link.getAttribute('href')).toBe('#/p/p1/rules/w1?session=s1');
    expect(link.getAttribute('title')).toBe('Rules for ~/work/hqc');
  });

  it('on a phone the header is three lines: "‹", the title and the buttons, the state, the pills; "More" says where the session runs, "<worker> · <folder>" with the copy button', async () => {
    const { app } = setup('running', { effective_permission_mode: 'allow_all', subagents: 2, model: 'claude-haiku-5-5' });
    await mount(app, true);
    expect([...$('.chat-head')!.children].map((c) => c.classList[0])).toEqual(['chat-head-top', 'chat-meta', 'chat-pills']);
    expect($('.chat-head-main')).toBeNull();
    // Line 1: the buttons on the title's line, after it.
    expect([...$('.chat-head-top')!.children].map((c) => c.getAttribute('aria-label') ?? c.classList[0])).toEqual([
      'Back to sessions',
      'chat-title',
      'Pause session',
      'Stop the agent',
      'menu-box',
    ]);
    // The state's line: the state at its full text and the subagents; no worker or folder.
    expect([...$('.chat-meta')!.children].map((c) => c.classList[0])).toEqual(['state', 'chat-agents']);
    expect($('.chat-meta .state')!.textContent).toBe('Running');
    expect($('.chat-meta')!.textContent).not.toContain('drzlo-mac');
    // The pills' line: the mode, then the model, each with its chevron.
    const pills = [...$('.chat-pills')!.children];
    expect(pills.map((c) => [c.classList[0], c.textContent])).toEqual([
      ['btn-mode-pill', 'Allow all'],
      ['btn-model-pill', 'Haiku 5.5'],
    ]);
    expect(pills.every((p) => p.querySelector('svg'))).toBe(true);
    await act(async () => $('.btn-more')!.click());
    const place = $('.menu-info .menu-place')!;
    expect(place.querySelector('.mono')!.textContent).toBe('drzlo-mac · ~/work/hqc');
    expect(place.querySelector('button')!.getAttribute('aria-label')).toBe('Copy path');
    expect($('.menu-info')!.firstElementChild).toBe(place);
  });

  it("on a phone the mode and model pills are on their own line, as pills of their own and not the state's", async () => {
    const { app } = setup('idle', { effective_permission_mode: 'auto', model: 'claude-sonnet-5-5' });
    await mount(app, true);
    const mode = $('.chat-pills > .btn-mode-pill')!;
    const model = $('.chat-pills > .btn-model-pill')!;
    expect(mode.textContent).toBe('Auto');
    expect(model.textContent).toBe('Sonnet 5.5');
    // `.chat-phone .pill` shrinks the state's pill; these are not it.
    expect(mode.classList.contains('pill')).toBe(false);
    expect(model.classList.contains('pill')).toBe(false);
    await act(async () => model.click());
    // The menu over a backdrop on a phone.
    expect($('.ctx-menu')).not.toBeNull();
  });
});

// The subagents running now, one line each, pinned above the input (SPEC §6, since 0.101).
describe('live subagents', () => {
  const live = (patch: (e: any) => void = () => {}) =>
    ev('event.subagents.live', 0, (e) => {
      delete e.seq;
      patch(e);
    });
  // The rows of the tasks: the main agent's row, `role="status"`, is looked at on its own.
  const rows = () =>
    [...root.querySelectorAll('.live-agents-row:not([role="status"])')].map((r) => [
      r.querySelector('.live-agents-what')!.textContent,
      r.querySelector('.live-agents-stats')!.textContent,
    ]);

  it('shows a row per subagent above the input, outside the feed, none with an empty list', async () => {
    const { app } = setup('running', { subagents: 2 });
    await mount(app);
    expect(rows()).toEqual([]);
    await act(async () => subscriber!.onEvent(live((e) => (e.payload.agents = e.payload.agents.slice(0, 2)))));
    expect(rows()).toEqual([
      [
        'Subagent: Fix schematic power nets in import: Running Verify the final sample runs are byte-identical',
        '155.3k tokens · 188 tools · 4m 13s',
      ],
      ['Subagent: Reply with number', '1s'],
    ]);
    // What a phone hides (styles.css, SPEC 0.205): the word "tokens" and the tool calls; the
    // count and the run time stay.
    expect([...root.querySelectorAll('.live-agents-stats :is(.stats-word, .stats-tools)')].map((s) => s.textContent)).toEqual([
      ' tokens',
      ' · 188 tools',
    ]);
    // Pinned: outside the feed's scroller, right above the input.
    expect($('.timeline .live-agents')).toBeNull();
    expect($('.live-agents + .composer-wrap')).not.toBeNull();
    await act(async () => subscriber!.onEvent(live((e) => (e.payload.agents = []))));
    expect(rows()).toEqual([]);
  });

  it('shows no empty block while the session idles and the list of its counted tasks has not come', async () => {
    const { app } = setup('idle', { subagents: 2 });
    await mount(app);
    expect($('.live-agents')).toBeNull();
    await act(async () => subscriber!.onEvent(live((e) => (e.payload.agents = e.payload.agents.slice(0, 1)))));
    expect(rows().length).toBe(1);
    await act(async () => subscriber!.onEvent(live((e) => (e.payload.agents = []))));
    expect($('.live-agents')).toBeNull();
  });

  it('shows a background command while no subagent runs: "Command:", its description and run time', async () => {
    const { app } = setup('idle', { subagents: 0, background_tasks: 1 });
    await mount(app);
    await act(async () =>
      subscriber!.onEvent(
        live(
          (e) =>
            (e.payload.agents = [
              {
                task_id: 'b2st7oc3z',
                description: 'Full run over 1000 boards on cirunner',
                duration_ms: 65_000,
                kind: 'command',
              },
            ]),
        ),
      ),
    );
    expect(rows()).toEqual([['Command: Full run over 1000 boards on cirunner', '1m 5s']]);
    // A row of an older worker has no `kind`, nor a word before its task.
    await act(async () =>
      subscriber!.onEvent(
        live((e) => (e.payload.agents = [{ task_id: 'bold', description: 'A command of an older worker', duration_ms: 1000 }])),
      ),
    );
    expect(rows()).toEqual([['A command of an older worker', '1s']]);
  });

  it("stops a subagent or a command from its row, the button off until the hub answers; a row without stoppable has none", async () => {
    // SPEC 0.161: the worker marks a subagent's row and a command's `stoppable`; an older
    // worker marks none.
    const { api, app } = setup('idle', { subagents: 1, background_tasks: 3 });
    await mount(app);
    await act(async () =>
      subscriber!.onEvent(
        live(
          (e) =>
            (e.payload.agents = [
              { task_id: 'a52af2b436ded4cea', description: 'Sleep and reply with number', duration_ms: 3000, stoppable: true },
              { task_id: 'b2st7oc3z', description: 'Full run over 1000 boards on cirunner', duration_ms: 65_000, stoppable: true },
              { task_id: 'bold', description: 'A command of an older worker', duration_ms: 1000 },
            ]),
        ),
      ),
    );
    const stops = () => [...root.querySelectorAll<HTMLElement>('.live-agents-row')].map((r) => r.querySelector<HTMLButtonElement>('.live-agents-stop'));
    expect(stops().map((b) => b !== null)).toEqual([true, true, false]);
    let answer = () => {};
    api.stopTask.mockImplementationOnce(() => new Promise<void>((r) => (answer = () => r())));
    await act(async () => stops()[0]!.click());
    expect(api.stopTask).toHaveBeenCalledWith('s1', 'a52af2b436ded4cea');
    expect(stops()[0]!.disabled).toBe(true);
    // The answer settles through the chat's notice handler first.
    await act(async () => {
      answer();
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(stops()[0]!.disabled).toBe(false);
    await act(async () => stops()[1]!.click());
    expect(api.stopTask).toHaveBeenLastCalledWith('s1', 'b2st7oc3z');
  });

  it('sends the stop as POST /api/sessions/{id}/tasks/{task_id}/stop', async () => {
    const fetchImpl = vi.fn(async (_url: string, _init?: RequestInit) => new Response(JSON.stringify({ sent: true })));
    await new Api('', fetchImpl as never).stopTask('s1', 'a52af2b436ded4cea');
    expect(fetchImpl).toHaveBeenCalledWith('/api/sessions/s1/tasks/a52af2b436ded4cea/stop', expect.objectContaining({ method: 'POST' }));
  });

  const schedulesLive = (patch: (e: any) => void = () => {}) =>
    ev('event.schedules.live', 0, (e) => {
      delete e.seq;
      patch(e);
    });

  it('shows the Monitor, Wakeup and Cron rows of the agent, the timers with a countdown and a still dot', async () => {
    // No subagent and no background task: the agent is idle, its timers wait.
    const { app } = setup('idle', { subagents: 0, background_tasks: 0 });
    await mount(app);
    await act(async () => subscriber!.onEvent(live((e) => (e.payload.agents = e.payload.agents.slice(2)))));
    const got = rows();
    expect(got.map((r) => r[0])).toEqual([
      "Monitor: Watch the run's log",
      'Wakeup: Check the build again',
      'Cron: Pull the CI status and report failures · Every 5 minutes',
    ]);
    // The watcher is a command: its run time; the timers count down to their fire.
    expect(got[0][1]).toMatch(/^1m \ds$/);
    expect(got[1][1]).toMatch(/^in 4m \d+s$/);
    expect(got[2][1]).toMatch(/^in 4\ds$/);
    const dots = [...root.querySelectorAll('.live-agents-dot')].map((d) => d.classList.contains('live-agents-dot-still'));
    expect(dots).toEqual([false, true, true]);
    // The watcher is a task the CLI stops alone; a timer is none.
    const stops = [...root.querySelectorAll('.live-agents-row')].map((r) => r.querySelector('.live-agents-stop') !== null);
    expect(stops).toEqual([true, false, false]);
    // Only a subagent's row opens its history: these are none.
    expect(root.querySelectorAll('button.live-agents-opens')).toHaveLength(0);
  });

  it('shows the block for the session\'s schedules alone: "Scheduled:", the message, the countdown; a click opens the schedules', async () => {
    const { api, app } = setup('idle', { subagents: 0, background_tasks: 0 });
    await mount(app);
    expect($('.live-agents')).toBeNull();
    await act(async () => subscriber!.onEvent(schedulesLive()));
    expect(rows().map((r) => r[0])).toEqual([
      'Scheduled: Check the nightly build and report failures',
      "Scheduled: Summarize yesterday's commits",
    ]);
    expect(rows()[0][1]).toMatch(/^in 1[12]m \d+s$/);
    expect(rows()[1][1]).toMatch(/^in 22h \d+m \d+s$/);
    expect($('.live-agents-dot')!.classList.contains('live-agents-dot-still')).toBe(true);
    expect($('.schedules')).toBeNull();
    await act(async () => $('.live-agents-open')!.click());
    expect($('.schedules')).not.toBeNull();
    expect(api.schedules).toHaveBeenCalledWith('s1');
    expect(api.subagentHistory).not.toHaveBeenCalled();
    // The list emptied: the block goes.
    await act(async () => subscriber!.onEvent(schedulesLive((e) => (e.payload.schedules = []))));
    expect($('.live-agents')).toBeNull();
  });

  it('counts a timer down from the time its list came, "due" at 0, and the running rows up', async () => {
    root = document.createElement('div');
    document.body.appendChild(root);
    const agents = [
      { task_id: 'w', description: 'Check', duration_ms: 1000, kind: 'wakeup' as const, due_in_ms: 270_000 },
      { task_id: 'c', description: 'Poll', activity: 'Every 5 minutes', duration_ms: 1000, kind: 'cron' as const },
      { task_id: 'a', description: 'Fix', duration_ms: 1000, kind: 'agent' as const },
    ];
    const schedules = [{ schedule_id: 's', message: 'Ping', due_in_ms: 41_000, timezone: 'UTC' }];
    const at = async (ms: number) => {
      await act(async () =>
        render(
          <LiveSubagents agents={agents} receivedAt={1000} schedules={schedules} schedulesAt={1000} onSchedules={() => {}} now={() => 1000 + ms} />,
          root,
        ),
      );
      return rows().map((r) => r[1]);
    };
    // A cron job whose next fire is not known has no countdown.
    expect(await at(0)).toEqual(['in 4m 30s', '', '1s', 'in 41s']);
    expect(await at(41_000)).toEqual(['in 3m 49s', '', '42s', 'due']);
    expect(await at(300_000)).toEqual(['due', '', '5m 1s', 'due']);
  });

  it('a subagent whose "what it does now" is its task says the task once', async () => {
    root = document.createElement('div');
    document.body.appendChild(root);
    // After a call of a tool without its own words (an MCP tool) the CLI sends the task itself.
    const agents = [
      { task_id: 'a', description: 'Review links, part A', activity: 'Review links, part A', duration_ms: 1000, kind: 'agent' as const },
      { task_id: 'b', description: 'Review links, part B', activity: 'Reading notes.md', duration_ms: 1000, kind: 'agent' as const },
    ];
    await act(async () =>
      render(<LiveSubagents agents={agents} receivedAt={1000} schedules={[]} schedulesAt={1000} onSchedules={() => {}} now={() => 1000} />, root),
    );
    expect(rows().map((r) => r[0])).toEqual(['Subagent: Review links, part A', 'Subagent: Review links, part B: Reading notes.md']);
  });

  it('says on its row that a subagent retries a request to the model API (SPEC 0.179)', async () => {
    const { app } = setup('running', { subagents: 1 });
    await mount(app);
    await act(async () => subscriber!.onEvent(ev('event.subagents.live.retry', 0, (e) => delete e.seq)));
    expect(rows()).toEqual([['Reply with number: Retrying 2/10 (no response), next in 4 s', '9s']]);
  });

  it('the main agent has the first row of the block, which shows with no subagent, and the feed has no activity line', async () => {
    const { app } = setup('running', { history: [] });
    await mount(app);
    expect($('.timeline .live-agents-row')).toBeNull();
    expect($('.live-agents + .composer-wrap')).not.toBeNull();
    expect(root.querySelectorAll('.live-agents-row')).toHaveLength(1);
    await act(async () => subscriber!.onEvent(live((e) => (e.payload.agents = e.payload.agents.slice(0, 1)))));
    const all = [...root.querySelectorAll('.live-agents-row')];
    expect(all).toHaveLength(2);
    expect(all[0].textContent).toBe('Main agent: Working');
    expect(all[1].textContent).toContain('Subagent: ');
    render(null, root);
    const idle = setup('idle', { history: [] });
    await mount(idle.app);
    expect($('.live-agents')).toBeNull();
  });

  it('says a subagent waits for permission while a request names it', async () => {
    const { app } = setup('waiting', {
      subagents: 2,
      history: [...HISTORY.slice(0, 3), ev('event.approval.request.subagent', 4)],
    });
    await mount(app);
    await act(async () => subscriber!.onEvent(live((e) => (e.payload.agents = e.payload.agents.slice(0, 2)))));
    expect(rows().map((r) => r[0])).toEqual([
      'Subagent: Fix schematic power nets in import: waiting for permission',
      'Subagent: Reply with number',
    ]);
    await act(async () =>
      subscriber!.onEvent(ev('event.approval.response', 5, (e) => (e.payload.request_id = 'req-3'))),
    );
    expect(rows().map((r) => r[0])).toEqual([
      'Subagent: Fix schematic power nets in import: Running Verify the final sample runs are byte-identical',
      'Subagent: Reply with number',
    ]);
  });

  // Since 0.181: what the subagent has done, under its line, as the feed draws it.
  it("opens a subagent's history under its line, fetches it again, closes it, and it goes with the line", async () => {
    // Its `Agent` call is not among the loaded events (a subagent's subagent, or one started
    // long ago): its line opens all the same.
    const { api, app } = setup('running', { subagents: 1, background_tasks: 1, history: HISTORY.slice(0, 3) });
    await mount(app);
    const step = (name: string, id: string, payload: unknown) =>
      ev(name, 0, (e) => {
        delete e.seq;
        e.event_id = `0199f3a2-7c10-7000-8000-0000000002${id}`;
        e.payload = payload;
      });
    const page = {
      events: [
        step('event.agent.thinking', 'e1', { text: 'The nets come from the netlist.' }),
        step('event.tool.call', 'e2', { call_id: 'toolu_s1', tool: 'Bash', args_preview: '{"command":"cargo test -p import"}' }),
        step('event.tool.result', 'e3', { call_id: 'toolu_s1', result_preview: 'test result: ok', error: false }),
      ],
      truncated: false,
    };
    // A new body each time, as parsed from the hub's answer.
    api.subagentHistory.mockImplementation(async () => ({ ...page }));
    const subagent = { task_id: 'a52af2b436ded4cea', description: 'Fix the nets', duration_ms: 1000, call_id: 'toolu_ag' };
    const command = { task_id: 'b2st7oc3z', description: 'Full run over 1000 boards', duration_ms: 65_000, call_id: 'toolu_bg', kind: 'command' as const };
    await act(async () => subscriber!.onEvent(live((e) => (e.payload.agents = [subagent, command]))));
    // The subagent's line opens; the command's is no button.
    const opens = [...root.querySelectorAll<HTMLButtonElement>('button.live-agents-opens')];
    expect(opens.map((b) => b.querySelector('.live-agents-task')!.textContent)).toEqual(['Fix the nets']);
    expect(rows()).toHaveLength(2);
    const panel = () => $('.live-agents li .live-agents-history');
    const flush = () => act(async () => void (await vi.advanceTimersByTimeAsync(0)));
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    try {
      await act(async () => opens[0].click());
      await flush();
      expect(api.subagentHistory).toHaveBeenCalledWith('s1', 'a52af2b436ded4cea', undefined);
      expect(opens[0].getAttribute('aria-expanded')).toBe('true');
      expect(panel()!.querySelector('.thinking summary')!.textContent).toContain('The nets come from the netlist.');
      expect(panel()!.querySelector('.tool-group.tool-ok .tool-peek')!.textContent).toContain('cargo test -p import');
      expect(panel()!.querySelector('.msg-agent')).toBeNull();

      // Fetched again 2 s after the answer, the new step at the bottom.
      page.events = [...page.events, step('event.agent.message', 'e4', { text: 'The import keeps the power nets.', final: false })];
      await act(async () => void (await vi.advanceTimersByTimeAsync(2000)));
      expect(api.subagentHistory).toHaveBeenCalledTimes(2);
      expect(panel()!.querySelector('.feed-block:last-child .msg-agent')!.textContent).toContain('The import keeps the power nets.');

      // A second click closes it, and nothing is fetched any more.
      await act(async () => opens[0].click());
      expect(panel()).toBeNull();
      await act(async () => void (await vi.advanceTimersByTimeAsync(6000)));
      expect(api.subagentHistory).toHaveBeenCalledTimes(2);

      // Open again: the subagent ends, its line goes and the history with it.
      await act(async () => opens[0].click());
      await flush();
      expect(panel()).not.toBeNull();
      await act(async () => subscriber!.onEvent(live((e) => (e.payload.agents = [command]))));
      expect(panel()).toBeNull();
      expect(rows().map((r) => r[0])).toEqual(['Command: Full run over 1000 boards']);
      const calls = api.subagentHistory.mock.calls.length;
      await act(async () => void (await vi.advanceTimersByTimeAsync(6000)));
      expect(api.subagentHistory).toHaveBeenCalledTimes(calls);
      // Continued later under the same id, its line comes back closed.
      await act(async () => subscriber!.onEvent(live((e) => (e.payload.agents = [subagent, command]))));
      expect(root.querySelector('button.live-agents-opens')!.getAttribute('aria-expanded')).toBe('false');
      expect(panel()).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("a subagent's history has the message buttons too: Quote and Copy take the selection there, Search shows", async () => {
    const { api, app } = setup('running', { subagents: 1, background_tasks: 1, history: HISTORY.slice(0, 3) });
    await mount(app);
    api.subagentHistory.mockImplementation(async () => ({
      events: [
        ev('event.agent.message', 0, (e) => {
          delete e.seq;
          e.event_id = '0199f3a2-7c10-7000-8000-0000000002e4';
          e.payload = { text: 'The import keeps the power nets.', final: false };
        }),
      ],
      truncated: false,
    }));
    await act(async () => subscriber!.onEvent(live((e) => (e.payload.agents = [{ task_id: 'a52af2b436ded4cea', description: 'Fix the nets', duration_ms: 1000, call_id: 'toolu_ag' }]))));
    await act(async () => void root.querySelector<HTMLButtonElement>('button.live-agents-opens')!.click());
    await act(async () => void (await new Promise((r) => setTimeout(r, 0))));
    const panel = $('.live-agents-history')!;
    const text = document.createTreeWalker(panel, NodeFilter.SHOW_TEXT);
    let node: Node | null = text.nextNode();
    while (node && node.textContent !== 'The import keeps the power nets.') node = text.nextNode();
    await act(async () => {
      const r = document.createRange();
      r.setStart(node!, 4);
      r.setEnd(node!, 10);
      document.getSelection()!.removeAllRanges();
      document.getSelection()!.addRange(r);
      document.dispatchEvent(new Event('selectionchange'));
    });
    expect(panel.querySelector(`.msg-agent ${SEARCH}`)).not.toBeNull();
    await act(async () => void panel.querySelector<HTMLButtonElement>('.msg-agent .btn-msg-quote')!.click());
    expect(($('.composer-input') as HTMLTextAreaElement).value).toBe('> import\n\n');
  });

  it('fetches an open history again after its last event, appends what came since, and takes a whole page in its place', async () => {
    const { api, app } = setup('running', { subagents: 1 });
    await mount(app);
    const said = (id: string, text: string) =>
      ev('event.agent.message', 0, (e) => {
        delete e.seq;
        e.event_id = `0199f3a2-7c10-7000-8000-0000000002${id}`;
        e.payload = { text, final: false };
      });
    const replies: SubagentHistory[] = [
      { events: [said('e1', 'One.'), said('e2', 'Two.')], truncated: false },
      // What came after e2, with e2 again.
      { events: [said('e2', 'Two.'), said('e3', 'Three.')], truncated: false, continues: true },
      // Nothing new.
      { events: [], truncated: false, continues: true },
      // e3 is no longer in the worker's page: the whole page.
      { events: [said('e8', 'Eight.'), said('e9', 'Nine.')], truncated: true },
    ];
    api.subagentHistory.mockImplementation(async () => replies.shift()!);
    const subagent = { task_id: 'a52af2b436ded4cea', description: 'Fix the nets', duration_ms: 1000 };
    await act(async () => subscriber!.onEvent(live((e) => (e.payload.agents = [subagent]))));
    const panel = () => $('.live-agents li .live-agents-history')!;
    const texts = () => [...panel().querySelectorAll('.msg-agent > .text')].map((m) => m.textContent!.trim());
    const tick = (ms: number) => act(async () => void (await vi.advanceTimersByTimeAsync(ms)));
    // The `after` of the latest fetch.
    const after = () => api.subagentHistory.mock.lastCall![2];
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    try {
      await act(async () => root.querySelector<HTMLButtonElement>('button.live-agents-opens')!.click());
      await tick(0);
      expect(after()).toBeUndefined();
      expect(texts()).toEqual(['One.', 'Two.']);
      await tick(2000);
      expect(after()).toBe('0199f3a2-7c10-7000-8000-0000000002e2');
      expect(texts()).toEqual(['One.', 'Two.', 'Three.']);
      const shown = panel().innerHTML;
      await tick(2000);
      expect(after()).toBe('0199f3a2-7c10-7000-8000-0000000002e3');
      expect(panel().innerHTML).toBe(shown);
      await tick(2000);
      expect(after()).toBe('0199f3a2-7c10-7000-8000-0000000002e3');
      expect(texts()).toEqual(['Eight.', 'Nine.']);
      expect(panel().querySelector('.live-agents-note')!.textContent).toBe('Earlier steps are not shown.');
    } finally {
      vi.useRealTimers();
    }
  });

  it('shows a spinner and "Loading…" in an open history until its first page is in', async () => {
    const { api, app } = setup('running', { subagents: 1 });
    await mount(app);
    api.subagentHistory.mockImplementation(() => new Promise(() => {}) as never);
    const subagent = { task_id: 'a52af2b436ded4cea', description: 'Fix the nets', duration_ms: 1000 };
    await act(async () => subscriber!.onEvent(live((e) => (e.payload.agents = [subagent]))));
    await act(async () => root.querySelector<HTMLButtonElement>('button.live-agents-opens')!.click());
    const loading = $('.live-agents-history > .live-agents-note > .loading')!;
    expect(loading.getAttribute('role')).toBe('status');
    expect(loading.textContent).toBe('Loading…');
    expect(loading.querySelector('.spinner')).not.toBeNull();
  });

  it('keeps the newest 200 steps of an open history and says earlier ones are not shown', async () => {
    const { api, app } = setup('running', { subagents: 1 });
    await mount(app);
    const said = (n: number) =>
      ev('event.agent.message', 0, (e) => {
        delete e.seq;
        e.event_id = `0199f3a2-7c10-7000-8000-${n.toString().padStart(12, '0')}`;
        e.payload = { text: `Step ${n}.`, final: false };
      });
    const replies: SubagentHistory[] = [
      { events: Array.from({ length: 199 }, (_, i) => said(i + 1)), truncated: false },
      { events: [said(200), said(201), said(202)], truncated: false, continues: true },
    ];
    api.subagentHistory.mockImplementation(async () => replies.shift() ?? { events: [], truncated: false, continues: true });
    const subagent = { task_id: 'a52af2b436ded4cea', description: 'Fix the nets', duration_ms: 1000 };
    await act(async () => subscriber!.onEvent(live((e) => (e.payload.agents = [subagent]))));
    const panel = () => $('.live-agents li .live-agents-history')!;
    const texts = () => [...panel().querySelectorAll('.msg-agent > .text')].map((m) => m.textContent!.trim());
    const tick = (ms: number) => act(async () => void (await vi.advanceTimersByTimeAsync(ms)));
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    try {
      await act(async () => root.querySelector<HTMLButtonElement>('button.live-agents-opens')!.click());
      await tick(0);
      expect(texts()).toHaveLength(199);
      expect(panel().querySelector('.live-agents-note')).toBeNull();
      await tick(2000);
      expect(texts()).toHaveLength(200);
      expect([texts()[0], texts()[199]]).toEqual(['Step 3.', 'Step 202.']);
      expect(panel().querySelector('.live-agents-note')!.textContent).toBe('Earlier steps are not shown.');
    } finally {
      vi.useRealTimers();
    }
  });

  it('fetches an open history again only while the page is shown, and at once when it is shown again', async () => {
    const { api, app } = setup('running', { subagents: 1 });
    await mount(app);
    api.subagentHistory.mockImplementation(async () => ({ events: [], truncated: false }));
    const subagent = { task_id: 'a52af2b436ded4cea', description: 'Fix the nets', duration_ms: 1000 };
    await act(async () => subscriber!.onEvent(live((e) => (e.payload.agents = [subagent]))));
    let hidden = false;
    Object.defineProperty(document, 'hidden', { configurable: true, get: () => hidden });
    const visibility = (h: boolean) => {
      hidden = h;
      document.dispatchEvent(new Event('visibilitychange'));
    };
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    try {
      await act(async () => root.querySelector<HTMLButtonElement>('button.live-agents-opens')!.click());
      await act(async () => void (await vi.advanceTimersByTimeAsync(0)));
      expect(api.subagentHistory).toHaveBeenCalledTimes(1);
      await act(async () => visibility(true));
      await act(async () => void (await vi.advanceTimersByTimeAsync(10_000)));
      expect(api.subagentHistory).toHaveBeenCalledTimes(1);
      await act(async () => visibility(false));
      await act(async () => void (await vi.advanceTimersByTimeAsync(0)));
      expect(api.subagentHistory).toHaveBeenCalledTimes(2);
      // And on every 2 s again.
      await act(async () => void (await vi.advanceTimersByTimeAsync(2000)));
      expect(api.subagentHistory).toHaveBeenCalledTimes(3);
    } finally {
      vi.useRealTimers();
      delete (document as { hidden?: boolean }).hidden;
    }
  });
});

describe('the request card through the chat', () => {
  it('folds the tool call into the card, explains, and takes Enter', async () => {
    // Asked 42 s before this test, not before the file loaded: the tests ahead of it take time.
    const asked = new Date(Date.now() - 42_000).toISOString();
    const { api, app } = setup('waiting', { history: HISTORY.map((e) => (e.seq === 6 ? { ...e, ts: asked } : e)) });
    await mount(app);
    expect($('.tool-group')).toBeNull();
    expect($('.perm-what')!.textContent).toBe('$ ls /');
    expect($('.perm-note')!.textContent).toBe(
      'A worker rule allows Bash, but Claude Code asked anyway: the path / is outside the working folder ~/work/hqc.',
    );
    expect($('.perm-timer')!.textContent).toMatch(/^waiting 0:4\d$/);
    await act(async () => void document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })));
    expect(api.approve.mock.calls[0].slice(0, 3)).toEqual(['s1', 'req-1', 'allow']);
  });
});

describe("an answer of this client, before the hub's state and events come", () => {
  const enter = () => act(async () => void document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })));
  const settle = () => act(async () => void (await new Promise((r) => setTimeout(r, 50))));

  it('the pill and the activity line do not wait for the closed request', async () => {
    const { app } = setup('waiting');
    await mount(app);
    await settle();
    expect($('.pill')!.textContent).toBe('Waiting for permission');
    await enter();
    await settle();
    expect($('.pill')!.textContent).toBe('Running');
    expect(root.textContent).not.toContain('Waiting for approval');
    // The call waited 42 s for the answer: it counts as running from the answer, so the line
    // does not name it at once (SPEC §17.1).
    expect($('.live-agents-row')!.textContent).toBe('Main agent: Working');
  });

  it('the Requests list drops a request answered in its chat', async () => {
    const { app } = setup('waiting');
    const asked = HISTORY.find((e) => e.type === 'approval.request') as EventOf<'approval.request'>;
    const open: OpenRequest = { session_id: 's1', request_id: 'req-1', kind: 'approval', address: 'hqc/s1', title: 'hqc', system: false, event: asked, waiting: [] };
    app.setRequests([open]);
    await mount(app);
    await settle();
    await enter();
    await settle();
    expect(app.requests).toEqual([]);
  });

  it('a request closed from another device shows what became of it, not an error', async () => {
    const history = [...HISTORY];
    const { api, app } = setup('waiting', { history });
    await mount(app);
    await settle();
    api.approve.mockImplementation(async () => {
      history.push(ev('event.approval.response', 8, (e) => (e.payload = { request_id: 'req-1', option_id: 'deny', by: 'u2' })));
      throw new ApiError(409, 'the request is already closed', 'request_closed');
    });
    await enter();
    await act(async () => {});
    expect($('.perm .error-text')).toBeNull();
    expect($('.perm-done-row .outcome')!.textContent).toContain('denied');
  });
});

describe('the focus when a request arrives', () => {
  it('leaves an empty field so Enter reaches the card, keeps a field with text', async () => {
    for (const [text, blurred] of [['', true], ['typing a reply', false]] as const) {
      const field = document.createElement('textarea');
      field.value = text;
      document.body.appendChild(field);
      field.focus();
      expect(document.activeElement).toBe(field);
      const { app } = setup('waiting');
      await mount(app);
      expect(document.activeElement === field).toBe(!blurred);
      render(null, root);
      field.remove();
    }
  });
});

describe('the VS Code strip instead of the composer (SPEC §9.3)', () => {
  it('open in VS Code right now: where once, in the pill; the strip says to close it there, no button', async () => {
    const { app } = setup('locked_vscode', { held_by: { entrypoint: 'claude-vscode', status: 'idle' }, history: HISTORY.slice(0, 3) });
    await mount(app);
    expect($('.pill')!.textContent).toBe('In VS Code on drzlo-mac');
    expect($('.pill .pill-dot')).toBeNull();
    expect($('.pill svg')).not.toBeNull();
    // Said once: no "/ VS Code" after the path, no machine of its own.
    const meta = [...$('.chat-meta')!.children].map((c) => c.textContent);
    expect(meta.filter((t) => t?.includes('VS Code'))).toEqual(['In VS Code on drzlo-mac']);
    expect(meta.filter((t) => t?.includes('drzlo-mac'))).toHaveLength(1);
    // Nor a "/" left before the path.
    expect($('.chat-meta .chat-path')!.previousElementSibling!.classList.contains('sep')).toBe(false);
    expect($('.lockbar.lockbar-held')!.textContent).toBe('Close the session in VS Code to write from here.');
    expect($('.lockbar svg')).not.toBeNull();
    expect(button('Take over here')).toBeUndefined();
    expect($('.composer')).toBeNull();
    expect(root.textContent).not.toContain('read-only');
  });

  it('on a phone too the machine is named once, in the pill: "More" has the folder without it', async () => {
    const { app } = setup('locked_vscode', { held_by: { entrypoint: 'claude-vscode', status: 'idle' }, history: HISTORY.slice(0, 3) });
    await mount(app, true);
    expect($('.pill')!.textContent).toBe('In VS Code on drzlo-mac');
    expect($('.chat-meta')!.textContent!.split('drzlo-mac')).toHaveLength(2);
    await act(async () => $('.btn-more')!.click());
    expect($('.menu-place .mono')!.textContent).toBe('~/work/hqc');
    expect($('.lockbar.lockbar-held')).not.toBeNull();
  });

  it('open in Claude Code in a terminal: the pill and the strip name it', async () => {
    const { app } = setup('locked_vscode', { held_by: { entrypoint: 'cli', status: 'busy' }, history: HISTORY.slice(0, 3) });
    await mount(app);
    expect($('.pill')!.textContent).toBe('In Claude Code on drzlo-mac');
    expect($('.lockbar')!.textContent).toBe('Close the session in Claude Code to write from here.');
  });

  it('last driven by VS Code, nobody holds it: "Take over here"', async () => {
    const { api, app } = setup('locked_vscode', { history: HISTORY.slice(0, 3) });
    await mount(app);
    expect($('.pill')!.textContent).toBe('In VS Code on drzlo-mac');
    expect($('.lockbar')!.classList.contains('lockbar-held')).toBe(false);
    expect($('.lockbar-text')!.textContent).toBe('VS Code was the last to run this session.');
    expect($('.composer')).toBeNull();
    await act(async () => button('Take over here')!.click());
    expect(api.takeover).toHaveBeenCalledWith('s1');
  });

  it('a session hqc drives has the composer', async () => {
    const { app } = setup('idle', { history: HISTORY.slice(0, 3) });
    await mount(app);
    expect($('.lockbar')).toBeNull();
    expect($('.composer textarea')).not.toBeNull();
    expect($('.chat-meta')!.textContent).toContain('drzlo-mac');
  });
});

// SPEC 0.161: a question a worker restart cancelled is answered by a message, so only where the
// session takes one (§10.1).
describe('an earlier question of the agent', () => {
  const history = [
    ev('event.clarify.request', 1),
    ev('event.clarify.cancel', 2, (e) => (e.payload.reason = 'worker_restart')),
  ];
  const answer = () => $('.card-clarify button[type=submit]');
  // The first chat of a run may take longer than `mount` waits to show its feed.
  const card = () => vi.waitFor(() => expect($('.card-clarify')).not.toBeNull());

  it('is answered where the composer is', async () => {
    const { app } = setup('idle', { history });
    await mount(app);
    await card();
    expect(answer()).not.toBeNull();
  });

  it('is not in a session VS Code holds', async () => {
    const { app } = setup('locked_vscode', { history });
    await mount(app);
    await card();
    expect(answer()).toBeNull();
  });

  it('is not in an archived session', async () => {
    const { app } = setup('idle', { history });
    app.sessions.set('s1', { ...app.sessions.get('s1')!, archived: true, state: 'archived' });
    await mount(app);
    await card();
    expect(answer()).toBeNull();
  });
});

describe('composer', () => {
  it('is one row: attach, the field with the hint, send; the field grows with its lines', async () => {
    const { app } = setup('idle', { history: HISTORY.slice(0, 3) });
    await mount(app);
    const row = $('.composer .composer-row')!;
    expect([...row.children].map((c) => c.getAttribute('aria-label'))).toEqual(['Attach files', 'Message', 'Send']);
    const area = row.querySelector('textarea')!;
    expect(area.getAttribute('rows')).toBe('1');
    expect(area.getAttribute('placeholder')).toBe('Message · Enter to send, Shift+Enter for a new line');
    expect($('.composer-hint')).toBeNull();
    // One line keeps the stylesheet's height; more lines set it (happy-dom has no layout: the
    // heights are stubbed).
    Object.defineProperty(area, 'clientHeight', { configurable: true, get: () => 36 });
    Object.defineProperty(area, 'scrollHeight', { configurable: true, get: () => (area.value.includes('\n') ? 64 : 36) });
    const type = (v: string) => act(() => {
      area.value = v;
      area.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await type('one');
    expect(area.style.height).toBe('');
    await type('one\ntwo\nthree');
    expect(area.style.height).toBe('64px');
    await type('one');
    expect(area.style.height).toBe('');
  });

  it("keeps the form's height while the field is measured at one line, so the feed above does not lose its bottom", async () => {
    const { app } = setup('idle', { history: HISTORY.slice(0, 3) });
    await mount(app);
    const area = root.querySelector<HTMLTextAreaElement>('.composer textarea')!;
    const form = area.form!;
    Object.defineProperty(form, 'offsetHeight', { configurable: true, get: () => 120 });
    // The form's min-height at the moment the field's lines are measured.
    const held: string[] = [];
    Object.defineProperty(area, 'clientHeight', { configurable: true, get: () => 36 });
    Object.defineProperty(area, 'scrollHeight', { configurable: true, get: () => (held.push(form.style.minHeight), 64) });
    await act(() => {
      area.value = 'one\ntwo';
      area.dispatchEvent(new Event('input', { bubbles: true }));
    });
    expect(held.at(-1)).toBe('120px');
    expect(form.style.minHeight).toBe('');
    expect(area.style.height).toBe('64px');
  });

  it('has the caret when the chat opens on a computer; not on a phone, nor over an open request', async () => {
    const { app } = setup('idle', { history: HISTORY.slice(0, 3) });
    await mount(app);
    expect(document.activeElement).toBe($('.composer textarea'));
    render(null, root);
    const { app: phone } = setup('idle', { history: HISTORY.slice(0, 3) });
    await mount(phone, true);
    expect(document.activeElement!.tagName).not.toBe('TEXTAREA');
    render(null, root);
    // Enter and Esc stay the card's.
    const { app: asked } = setup('waiting');
    await mount(asked);
    expect($('.perm')).not.toBeNull();
    expect(document.activeElement!.tagName).not.toBe('TEXTAREA');
    render(null, root);
    // A chat kept in memory without the request that came while it was closed: the session
    // waits all the same.
    const { app: stale } = setup('waiting', { history: HISTORY.slice(0, 3) });
    await mount(stale);
    expect($('.perm')).toBeNull();
    expect(document.activeElement!.tagName).not.toBe('TEXTAREA');
    render(null, root);
  });

  it('leaves the caret in a field the user went to while the chat loaded', async () => {
    const search = document.createElement('input');
    document.body.appendChild(search);
    search.focus();
    const { app } = setup('idle', { history: HISTORY.slice(0, 3) });
    await mount(app);
    expect(document.activeElement).toBe(search);
    search.remove();
  });

  it('on a phone the same row says only "Message"', async () => {
    const { app } = setup('idle', { history: HISTORY.slice(0, 3) });
    await mount(app, true);
    const row = $('.composer .composer-row')!;
    expect([...row.children].map((c) => c.getAttribute('aria-label'))).toEqual(['Attach files', 'Message', 'Send']);
    expect(row.querySelector('textarea')!.getAttribute('placeholder')).toBe('Message');
  });

  it("a press on the composer's buttons leaves the focus in the field (a phone keeps its keyboard and the buttons where the tap is)", async () => {
    const { app } = setup('idle', { history: HISTORY.slice(0, 3) });
    await mount(app, true);
    const area = $('.composer textarea') as HTMLTextAreaElement;
    await act(() => {
      area.value = 'deploy';
      area.dispatchEvent(new Event('input', { bubbles: true }));
    });
    area.focus();
    const press = (el: Element) => {
      const e = new MouseEvent('mousedown', { bubbles: true, cancelable: true });
      el.dispatchEvent(e);
      return e.defaultPrevented;
    };
    expect(press($('.composer .btn-send')!)).toBe(true);
    expect(press($('.composer .btn-send svg')!)).toBe(true);
    expect(press($('.composer .btn-attach')!)).toBe(true);
    // The field itself takes the press as always.
    expect(press(area)).toBe(false);
  });
});

describe('quick messages', () => {
  const MESSAGES = ['Continue', 'Run the tests\nand fix what fails'];
  const area = () => $('.composer-input') as HTMLTextAreaElement;
  const type = (text: string) =>
    act(() => {
      area().value = text;
      area().dispatchEvent(new Event('input', { bubbles: true }));
    });
  /** A chat whose project has `messages`; sending reaches the hub as `api.input`. */
  async function chat(messages: string[]) {
    const { app, api } = setup('idle', { history: HISTORY.slice(0, 3) });
    app.projects.set('p1', { id: 'p1', name: 'hqc', slug: 'hqc', description: '', is_personal: false, color: 'teal', quick_messages: messages, created_at: '2026-10-01T00:00:00Z' });
    // GET /api/projects/{id}, which opening the list asks for: the hub's list, which another
    // device may have changed.
    Object.assign(api, { input: vi.fn(() => new Promise(() => {})), project: vi.fn(async () => app.project('p1')) });
    await mount(app);
    return api as unknown as { input: ReturnType<typeof vi.fn>; project: ReturnType<typeof vi.fn> };
  }
  const open = () => act(async () => $('.composer .btn-quick')!.click());
  const items = () => [...root.querySelectorAll('button.quick-item')];

  it('is a button after attach; it opens the messages, each on one line with its whole text as the title', async () => {
    await chat(MESSAGES);
    const row = $('.composer .composer-row')!;
    expect([...row.children].map((c) => c.getAttribute('aria-label'))).toEqual(['Attach files', 'Quick messages', 'Message', 'Send']);
    const button = $('.btn-quick')!;
    expect(button.getAttribute('aria-expanded')).toBe('false');
    expect(button.getAttribute('aria-controls')).toBeNull();
    expect($('.quick-popup')).toBeNull();
    await open();
    expect(button.getAttribute('aria-expanded')).toBe('true');
    expect(button.getAttribute('aria-controls')).toBe('composer-quick');
    expect(items().map((i) => i.textContent)).toEqual(['Continue', 'Run the tests']);
    expect(items().map((i) => i.getAttribute('title'))).toEqual(MESSAGES);
    expect($('.quick-edit a')!.textContent).toBe('Edit quick messages');
    expect($('.quick-edit a')!.getAttribute('href')).toBe('#/p/p1/settings');
    await open();
    expect($('.quick-popup')).toBeNull();
  });

  it('sends the exact text of the pressed message and leaves the typed text and the files alone', async () => {
    const api = await chat(MESSAGES);
    await type('a draft');
    const input = $('input[type=file]') as HTMLInputElement;
    Object.defineProperty(input, 'files', { value: [new File(['x'], 'note.txt', { type: 'text/plain' })], configurable: true });
    await act(async () => void input.dispatchEvent(new Event('change', { bubbles: true })));
    await vi.waitFor(() => expect($('.composer-files .attachment-chip')?.textContent).toContain('note.txt'));
    await open();
    await act(async () => (items()[1] as HTMLElement).click());
    await vi.waitFor(() => expect(api.input).toHaveBeenCalledTimes(1));
    expect(api.input).toHaveBeenCalledWith('s1', MESSAGES[1], expect.any(String), []);
    expect($('.quick-popup')).toBeNull();
    expect(area().value).toBe('a draft');
    expect($('.composer-files .attachment-chip')?.textContent).toContain('note.txt');
  });

  it('closes on Esc and on a press outside it, and keeps the focus in the field', async () => {
    await chat(MESSAGES);
    area().focus();
    await open();
    await act(async () => void document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true })));
    expect($('.quick-popup')).toBeNull();
    await open();
    await act(async () => void $('.feed')!.dispatchEvent(new Event('pointerdown', { bubbles: true })));
    expect($('.quick-popup')).toBeNull();
    // The press on its button and on an item leaves the focus where it is.
    await open();
    for (const el of [$('.btn-quick')!, items()[0]]) {
      const e = new MouseEvent('mousedown', { bubbles: true, cancelable: true });
      el.dispatchEvent(e);
      expect(e.defaultPrevented).toBe(true);
    }
    expect(document.activeElement).toBe(area());
  });

  it('shows the kept list at once on opening and the hub\'s when its answer comes', async () => {
    const { app, api } = setup('idle', { history: HISTORY.slice(0, 3) });
    const project = { id: 'p1', name: 'hqc', slug: 'hqc', description: '', is_personal: false, color: 'teal' as const, quick_messages: ['Continue'], created_at: '2026-10-01T00:00:00Z' };
    app.projects.set('p1', project);
    let answer!: () => void;
    Object.assign(api, {
      project: vi.fn(
        () =>
          new Promise((resolve) => {
            answer = () => resolve({ ...project, quick_messages: ['Continue', 'Added on the phone'] });
          }),
      ),
    });
    await mount(app);
    await open();
    expect((api as unknown as { project: ReturnType<typeof vi.fn> }).project).toHaveBeenCalledWith('p1');
    expect(items().map((i) => i.textContent)).toEqual(['Continue']);
    await act(async () => answer());
    expect(items().map((i) => i.textContent)).toEqual(['Continue', 'Added on the phone']);
  });

  it('is never open together with the list of completions: each closes the other', async () => {
    const { app, api } = setup('idle', { history: HISTORY.slice(0, 3), commands: ['compact', 'context'] });
    app.projects.set('p1', { id: 'p1', name: 'hqc', slug: 'hqc', description: '', is_personal: false, color: 'teal', quick_messages: MESSAGES, created_at: '2026-10-01T00:00:00Z' });
    Object.assign(api, { project: vi.fn(async () => app.project('p1')) });
    await mount(app);
    await type('/co');
    expect($('#composer-commands')).not.toBeNull();
    await open();
    expect($('#composer-commands')).toBeNull();
    expect($('.quick-popup')).not.toBeNull();
    await type('/con');
    expect($('.quick-popup')).toBeNull();
    expect($('#composer-commands')).not.toBeNull();
  });

  it('with no messages says where to add them, with the same link', async () => {
    await chat([]);
    await open();
    expect(items()).toEqual([]);
    expect($('.quick-edit')!.textContent).toBe('No quick messages yet — add them in project settings');
    expect($('.quick-edit a')!.getAttribute('href')).toBe('#/p/p1/settings');
  });

  describe('added from a message', () => {
    const TEXT = (HISTORY[0] as EventOf<'user.message'>).payload.text;
    const PROJECT = { id: 'p1', name: 'hqc', slug: 'hqc', description: '', is_personal: false, color: 'teal' as const, created_at: '2026-10-01T00:00:00Z' };
    /** The kept list is `kept`, the hub's is `hub`; `patchProject` answers with the list sent. */
    async function chatWith(kept: string[], hub: string[]) {
      const { app, api } = setup('idle', { history: HISTORY.slice(0, 3) });
      app.projects.set('p1', { ...PROJECT, quick_messages: kept });
      Object.assign(api, {
        project: vi.fn(async () => ({ ...PROJECT, quick_messages: hub })),
        patchProject: vi.fn(async (_id: string, p: { quick_messages: string[] }) => ({ ...PROJECT, quick_messages: p.quick_messages })),
      });
      await mount(app);
      return api as unknown as { project: ReturnType<typeof vi.fn>; patchProject: ReturnType<typeof vi.fn> };
    }
    const button = () => $('.msg-user .btn-to-quick') as HTMLButtonElement;
    const press = () => act(async () => button().click());

    it('a message of the user has the button right before fork; the agent\'s has none', async () => {
      await chatWith([], []);
      const acts = [...$('.msg-user .msg-buttons')!.querySelectorAll('button')];
      expect(acts.map((b) => b.getAttribute('aria-label'))).toEqual(['Search in Google', 'Copy', 'Quote', 'Add to quick messages', 'Fork from here']);
      expect($('.msg-agent .btn-to-quick')).toBeNull();
    });

    it('adds the text at the end of the hub\'s list, then is a check', async () => {
      const api = await chatWith(['Continue'], ['Continue', 'Added on the phone']);
      expect(button().disabled).toBe(false);
      await press();
      await vi.waitFor(() => expect(api.patchProject).toHaveBeenCalledTimes(1));
      expect(api.project).toHaveBeenCalledWith('p1');
      expect(api.patchProject).toHaveBeenCalledWith('p1', { quick_messages: ['Continue', 'Added on the phone', TEXT] });
      await vi.waitFor(() => expect(button().getAttribute('aria-label')).toBe('In quick messages'));
      expect(button().disabled).toBe(true);
      expect(button().getAttribute('title')).toBe('In quick messages');
    });

    it('a text already in the list is a check', async () => {
      await chatWith([TEXT], [TEXT]);
      expect(button().getAttribute('aria-label')).toBe('In quick messages');
      expect(button().disabled).toBe(true);
    });

    it('a text another device added meanwhile sends nothing and is a check', async () => {
      const api = await chatWith([], [TEXT]);
      await press();
      await vi.waitFor(() => expect(button().getAttribute('aria-label')).toBe('In quick messages'));
      expect(api.patchProject).not.toHaveBeenCalled();
    });

    it('a refusal of the hub is said, and the button works again', async () => {
      const api = await chatWith([], []);
      api.patchProject.mockRejectedValueOnce(new Error('too long'));
      await press();
      await vi.waitFor(() => expect(root.textContent).toContain('Not added to quick messages: too long'));
      expect(button().disabled).toBe(false);
      await press();
      await vi.waitFor(() => expect(api.patchProject).toHaveBeenCalledTimes(2));
    });
  });
});

describe('phone (Mobile-Session)', () => {
  it('the open request is a bottom sheet in place of the composer, with the text answer', async () => {
    const { api, app } = setup('waiting');
    await mount(app, true);
    expect($('.feed .perm')).toBeNull();
    const sheet = $('.sheet-wrap .perm-sheet')!;
    expect(sheet.querySelector('.perm-title')!.textContent).toBe('Run the command?');
    expect($('.composer')).toBeNull();
    expect($('a[aria-label="Back to sessions"]')!.getAttribute('href')).toBe('#/p/p1');
    expect($('button[aria-label="Stop the agent"]')).not.toBeNull();
    // "More" on a phone also has where it runs and the cost; the model is a pill of the header.
    await act(async () => $('.btn-more')!.click());
    expect($('.menu-info')!.textContent).toContain('drzlo-mac · ~/work/hqc');
    expect($('.menu-info')!.textContent).not.toContain('Opus 5.5');
    expect($('.chat-pills .btn-model-pill')!.textContent).toBe('Opus 5.5');
    await act(async () => $('.btn-more')!.click());

    await act(async () => button('Reply with text instead')!.click());
    const area = sheet.querySelector<HTMLTextAreaElement>('textarea')!;
    await act(() => {
      area.value = 'Leave the root alone';
      area.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await act(async () => button('Deny and send')!.click());
    expect(api.approve).toHaveBeenCalledWith('s1', 'req-1', 'deny', 'Leave the root alone');
  });
});

describe('cost (T27)', () => {
  it('shows the runtime’s session total in the header and each turn’s own cost under it', async () => {
    const history = [
      ev('event.user.message', 1, (e) => (e.payload.artifact_ids = [])),
      ev('event.agent.message', 2),
      ev('event.usage', 3, (e) => (e.payload = { input_tokens: 2, output_tokens: 5, cost_usd: 0.4, session_cost_usd: 44.0 })),
      ev('event.user.message', 4, (e) => ((e.event_id = '0199f3a2-7c10-7000-8000-0000000001c4'), (e.payload.artifact_ids = []))),
      ev('event.agent.message', 5, (e) => (e.event_id = '0199f3a2-7c10-7000-8000-0000000001c5')),
      ev('event.usage', 6, (e) => (e.payload = { input_tokens: 2, output_tokens: 5, cost_usd: 0.401, session_cost_usd: 44.401 })),
    ];
    const { app } = setup('idle', { history, commands: [] });
    await mount(app);
    expect($('.chat-cost')!.textContent).toBe('$44.40 this session');
    const lines = [...root.querySelectorAll('.usage')].map((l) => l.textContent!);
    expect(lines).toHaveLength(2);
    expect(lines[0]).toContain('$0.400');
    expect(lines[1]).toContain('$0.401');
  });

  it('sums cost_usd when no usage has the session total (older events)', async () => {
    const { app } = setup('idle', { commands: [] });
    await mount(app);
    expect($('.chat-cost')!.textContent).toBe('$0.07 this session');
  });
});

describe('slash command completion (T27)', () => {
  const COMMANDS = ['compact', 'context', 'code-review', 'review', 'anthropic-skills:pdf'];
  const area = () => $('.composer-input') as HTMLTextAreaElement;
  const type = (text: string) =>
    act(() => {
      area().value = text;
      area().dispatchEvent(new Event('input', { bubbles: true }));
    });
  const press = async (key: string, init: KeyboardEventInit = {}) => {
    const e = new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...init });
    await act(() => void area().dispatchEvent(e));
    return e;
  };
  const options = () => [...root.querySelectorAll('.cmd-item')].map((o) => o.textContent);
  const active = () => root.querySelector('.cmd-item.is-active')?.textContent;

  it('lists the commands matching what follows «/», moves with the arrows, inserts with Enter', async () => {
    const { app } = setup('idle', { commands: COMMANDS });
    await mount(app);
    await type('/co');
    expect(options()).toEqual(['/compact', '/context', '/code-review']);
    expect(active()).toBe('/compact');
    expect(area().getAttribute('aria-expanded')).toBe('true');
    expect(area().getAttribute('aria-activedescendant')).toBe('composer-command-0');
    await press('ArrowDown');
    await press('ArrowDown');
    expect(active()).toBe('/code-review');
    await press('ArrowDown');
    expect(active(), 'wraps around').toBe('/compact');
    await press('ArrowUp');
    expect(active()).toBe('/code-review');
    const enter = await press('Enter');
    expect(enter.defaultPrevented).toBe(true);
    expect(area().value).toBe('/code-review ');
    expect(options(), 'closed once a command is chosen').toEqual([]);
  });

  it('Tab inserts, Esc closes until the text changes, other text has no popup', async () => {
    const { app } = setup('idle', { commands: COMMANDS });
    await mount(app);
    await type('/AN');
    expect(options(), 'case does not matter').toEqual(['/anthropic-skills:pdf']);
    await press('Tab');
    expect(area().value).toBe('/anthropic-skills:pdf ');

    await type('/re');
    expect(options()).toEqual(['/review']);
    await press('Escape');
    expect(options()).toEqual([]);
    await type('/rev');
    expect(options(), 'typing on reopens it').toEqual(['/review']);
    await type('/review');
    expect(options(), 'typed in full').toEqual([]);
    await type('please /review');
    expect(options()).toEqual([]);
    await type('/nothing');
    expect(options()).toEqual([]);
  });

  it('a click (a tap on a phone) inserts the command', async () => {
    // Without the open request of HISTORY: on a phone it would take the composer's place.
    const { app } = setup('idle', { commands: COMMANDS, history: HISTORY.slice(0, 3) });
    await mount(app, true);
    await type('/');
    expect(options()).toHaveLength(COMMANDS.length);
    const item = [...root.querySelectorAll<HTMLElement>('.cmd-item')].find((o) => o.textContent === '/review')!;
    await act(() => item.click());
    expect(area().value).toBe('/review ');
  });

  it('shows what the runtime says of each command, asked for when the session came from a list (SPEC 0.162)', async () => {
    const info: SlashCommand[] = [
      { name: 'compact', description: 'Clear conversation history but keep a summary in context', argument_hint: '<instructions>' },
      { name: 'context', description: 'Visualize current context usage' },
    ];
    const { api, app } = setup('idle', { commands: COMMANDS, fetched_info: info });
    await mount(app);
    expect(api.session, 'a list gives no command_info').toHaveBeenCalledTimes(1);
    await type('/co');
    const items = [...root.querySelectorAll('.cmd-item')];
    expect(items[0].querySelector('.cmd-args')?.textContent).toBe('<instructions>');
    expect(items[0].querySelector('.cmd-desc')?.textContent).toBe('Clear conversation history but keep a summary in context');
    expect(items[1].querySelector('.cmd-args'), 'no hint').toBeNull();
    expect(items[1].querySelector('.cmd-desc')?.textContent).toBe('Visualize current context usage');
    expect(items[2].textContent, 'nothing said of it').toBe('/code-review');
    await press('Enter');
    expect(area().value, 'only the name goes in').toBe('/compact ');
  });

  it('no list, no popup; an idle session without a list fetches the session again', async () => {
    const { api, app } = setup('idle', { fetched: ['compact'] });
    await mount(app);
    expect(api.session).toHaveBeenCalledTimes(1);
    await type('/c');
    expect(options()).toEqual(['/compact']);

    const empty = setup('running');
    await act(async () => render(null, root));
    await mount(empty.app);
    expect(empty.api.session).not.toHaveBeenCalled();
    await type('/c');
    expect(options()).toEqual([]);
    expect(area().hasAttribute('aria-expanded')).toBe(false);
  });
});

describe('an image in the feed', () => {
  it('a chat opened again shows it by the link it had; after the hub reconnects, by a new one', async () => {
    const pic = ev('event.attachment', 4, (e) => (e.payload.artifact_id = 'img'));
    const { api, app } = setup('idle', { history: [...HISTORY.slice(0, 3), pic] });
    const artifactLink = vi.fn(async (id: string) => ({
      url: `/artifacts/${id}?sig=${artifactLink.mock.calls.length}`,
      expires_at: new Date(Date.now() + 600_000).toISOString(),
    }));
    const artifact = vi.fn(async (id: string) => ({ id, kind: 'image', name: 'screen.png', mime: 'image/png', size: 10, sha256: '', created_at: '' }));
    Object.assign(api, { artifact, artifactLink });
    let opened!: (s: 'open') => void;
    Object.assign(app.socket, {
      onState: (fn: (s: 'open') => void) => {
        opened = fn;
        return () => {};
      },
    });
    const src = () => $('.attachment-image img')?.getAttribute('src');
    /** The chat, its history and its image link in. */
    const open = async () => {
      await mount(app);
      for (let i = 0; i < 20 && !src(); i++) await act(async () => void (await new Promise((r) => setTimeout(r, 10))));
    };
    await open();
    expect(src()).toBe('/artifacts/img?sig=1');
    render(null, root);
    await open();
    expect(src()).toBe('/artifacts/img?sig=1');
    expect(artifactLink).toHaveBeenCalledTimes(1);
    render(null, root);
    // A restarted hub may sign with another key: the links are asked for again.
    opened('open');
    await open();
    expect(src()).toBe('/artifacts/img?sig=2');
  });
});

describe('history', () => {
  const long = Array.from({ length: 450 }, (_, i) =>
    ev('event.agent.message', i + 1, (e) => (e.payload.text = `answer ${i + 1}`)),
  );

  // happy-dom lays nothing out: every element is 0 px high. The feed below is taller than its window.
  const tall = () => {
    const h = (px: number) =>
      function (this: HTMLElement) {
        return this.classList.contains('timeline') ? px : 0;
      };
    vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockImplementation(h(5000));
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(h(600));
  };
  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it('stays at the bottom when the window over the feed shrinks (the composer grows, the keyboard opens), unless scrolled up', async () => {
    // The observers the chat makes, by what they watch; happy-dom resizes nothing by itself.
    const watched = new Map<Element, () => void>();
    vi.stubGlobal(
      'ResizeObserver',
      class {
        cb: () => void;
        constructor(cb: () => void) {
          this.cb = cb;
        }
        observe(el: Element) {
          watched.set(el, this.cb);
        }
        disconnect() {}
      },
    );
    let win = 600;
    vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return this.classList.contains('timeline') ? 5000 : 0;
    });
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return this.classList.contains('timeline') ? win : 0;
    });
    const { app } = setup('idle', { history: long });
    await mount(app);
    const scroller = root.querySelector<HTMLElement>('.timeline')!;
    expect(scroller.scrollTop).toBe(5000);
    // The window is 100 px shorter and the scroll position stayed: the bottom is under the
    // composer until the chat follows.
    win = 500;
    scroller.scrollTop = 4400;
    await act(async () => watched.get(scroller)?.());
    expect(scroller.scrollTop).toBe(5000);
    // Scrolled up, the feed stays where the reader is.
    for (const top of [5000, 3000]) {
      await act(async () => {
        scroller.scrollTop = top;
        scroller.dispatchEvent(new Event('scroll'));
      });
    }
    win = 400;
    await act(async () => watched.get(scroller)?.());
    expect(scroller.scrollTop).toBe(3000);
  });

  it('shows the newest page at once and loads the older history behind it', async () => {
    tall();
    const { api, app } = setup('idle', { history: long });
    let older!: () => void;
    const tail = api.eventsBefore.getMockImplementation()!;
    api.eventsBefore.mockImplementationOnce(tail);
    api.eventsBefore.mockImplementationOnce(
      (id, before, limit) => new Promise((r) => (older = () => r(tail(id, before, limit)))),
    );
    await mount(app);
    // The newest page, once it is in: a run that starts with this test pays for the first
    // imports and renders it later than the 50 ms of `mount` (#47).
    for (let i = 0; i < 300 && !root.querySelector('.feed-block'); i++) {
      await act(async () => void (await new Promise((r) => setTimeout(r, 10))));
    }
    expect(api.events).not.toHaveBeenCalled();
    expect(root.querySelector('.feed .empty')).toBeNull();
    expect(api.eventsBefore.mock.calls[1]?.[1]).toBe(351);
    // The last 60 blocks of the newest 100 events.
    const blocks = () => [...root.querySelectorAll<HTMLElement>('.feed-block')];
    expect(blocks().length).toBe(60);
    expect(blocks().at(-1)!.textContent).toContain('answer 450');
    await act(async () => {
      older();
      await new Promise((r) => setTimeout(r, 20));
    });
    // What is on screen stays; scrolling to the top renders more.
    expect(blocks().length).toBe(60);
    const scroller = root.querySelector<HTMLElement>('.timeline')!;
    await act(async () => {
      scroller.scrollTop = 0;
      scroller.dispatchEvent(new Event('scroll'));
    });
    expect(blocks().length).toBe(120);
    expect(blocks()[0].textContent).toContain('answer 331');
  });

  // Every block 10 px high, the window 1000 px: 60 blocks do not fill it, 120 do.
  const blockHigh = () => {
    vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return this.classList.contains('timeline') ? this.querySelectorAll('.feed-block').length * 10 : 0;
    });
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return this.classList.contains('timeline') ? 1000 : 0;
    });
  };
  const settle = () =>
    act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
  const blocks = () => [...root.querySelectorAll<HTMLElement>('.feed-block')];

  it('a feed shorter than its window renders the older history until the window is full', async () => {
    blockHigh();
    const { app } = setup('idle', { history: long });
    await mount(app);
    await settle();
    expect(blocks().length).toBe(120);
    expect(blocks()[0].textContent).toContain('answer 331');
  });

  it('"Requests only" there and back renders the last blocks of the log, not all of it', async () => {
    blockHigh();
    const log = [ev('event.approval.request', 1, (e) => delete e.payload.expires_at), ...long.map((e) => ({ ...e, seq: e.seq! + 1 }))];
    const { app } = setup('idle', { history: log });
    app.sessions.get('s1')!.system = true;
    await mount(app);
    await settle();
    expect(blocks().length).toBe(120);
    await act(async () => button('Requests only')!.click());
    await settle();
    expect(root.querySelector('.perm')).not.toBeNull();
    expect(root.querySelector('.feed-note')).toBeNull();
    await act(async () => button('Requests only')!.click());
    await settle();
    expect(blocks().length).toBe(120);
    expect(blocks().at(-1)!.textContent).toContain('answer 450');
  });

  it('a scroll that lands after the feed grew does not let go of the bottom; a scroll up does', async () => {
    let height = 5000;
    vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return this.classList.contains('timeline') ? height : 0;
    });
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return this.classList.contains('timeline') ? 600 : 0;
    });
    const { app } = setup('idle', { history: long });
    await mount(app);
    const scroller = root.querySelector<HTMLElement>('.timeline')!;
    const scroll = (top: number) =>
      act(async () => {
        scroller.scrollTop = top;
        scroller.dispatchEvent(new Event('scroll'));
      });
    const next = (seq: number) => act(async () => subscriber!.onEvent(ev('event.agent.message', seq, (e) => (e.payload.text = `more ${seq}`))));
    await scroll(4400);
    // An image loaded: the feed is 400 px taller, and the event of a small shift comes first.
    height = 5400;
    await scroll(4418);
    await next(451);
    expect(scroller.scrollTop).toBe(5400);
    // The user scrolls up: the feed stays where they are.
    await scroll(4000);
    height = 5600;
    await next(452);
    expect(scroller.scrollTop).toBe(4000);
  });

  it('a move up the browser made as the feed got shorter, delivered after it grew again, does not let go of the bottom', async () => {
    let height = 5000;
    vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return this.classList.contains('timeline') ? height : 0;
    });
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return this.classList.contains('timeline') ? 600 : 0;
    });
    const { app } = setup('idle', { history: long });
    await mount(app);
    const scroller = root.querySelector<HTMLElement>('.timeline')!;
    const scroll = (top: number) =>
      act(async () => {
        scroller.scrollTop = top;
        scroller.dispatchEvent(new Event('scroll'));
      });
    const next = (seq: number) => act(async () => subscriber!.onEvent(ev('event.agent.message', seq, (e) => (e.payload.text = `more ${seq}`))));
    await scroll(4400);
    // The names of four images give way to the images not loaded yet: the feed is 300 px
    // shorter and the browser moves the position up with it. WebKit delivers that scroll after
    // the images loaded and the feed grew 1000 px.
    height = 6000;
    await scroll(4100);
    await next(451);
    expect(scroller.scrollTop).toBe(6000);
    // A scroll up of the user's still lets go.
    await scroll(3000);
    height = 6200;
    await next(452);
    expect(scroller.scrollTop).toBe(3000);
  });

  it('while a wheel gesture goes on, the feed neither follows its bottom nor renders older blocks; both come once it settles', async () => {
    // Every block 100 px high, the window 600 px.
    vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return this.classList.contains('timeline') ? this.querySelectorAll('.feed-block').length * 100 : 0;
    });
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return this.classList.contains('timeline') ? 600 : 0;
    });
    const { app } = setup('idle', { history: long });
    await mount(app);
    await settle();
    const scroller = root.querySelector<HTMLElement>('.timeline')!;
    expect(blocks().length).toBe(60);
    expect(scroller.scrollTop).toBe(6000);
    const wheel = () =>
      act(async () => {
        scroller.dispatchEvent(new Event('wheel'));
      });
    const gestureEnds = () => act(async () => new Promise((r) => setTimeout(r, 450)));
    // At the bottom: an answer comes in while the trackpad moves; the feed follows it after.
    await wheel();
    await act(async () => subscriber!.onEvent(ev('event.agent.message', 451, (e) => (e.payload.text = 'more 451'))));
    expect(scroller.scrollTop).toBe(6000);
    await gestureEnds();
    expect(scroller.scrollTop).toBe(6100);
    // The browser's scroll event for that move (happy-dom sends none).
    await act(async () => {
      scroller.dispatchEvent(new Event('scroll'));
    });
    // A gesture that held nothing up leaves the feed where it took it, a little above the bottom.
    await wheel();
    await act(async () => {
      scroller.scrollTop = 6050;
      scroller.dispatchEvent(new Event('scroll'));
    });
    await gestureEnds();
    expect(scroller.scrollTop).toBe(6050);
    // Scrolled near the top by the gesture: the older blocks wait for its end, then come in
    // above with what is on screen kept in place.
    await wheel();
    await act(async () => {
      scroller.scrollTop = 300;
      scroller.dispatchEvent(new Event('scroll'));
    });
    expect(blocks().length).toBe(61);
    expect(scroller.scrollTop).toBe(300);
    await gestureEnds();
    expect(blocks().length).toBe(121);
    expect(scroller.scrollTop).toBe(6300);
  });

  it('while a wheel gesture goes on, a resize does not pull the feed to its bottom; it follows once the gesture settles', async () => {
    const watched = new Map<Element, () => void>();
    vi.stubGlobal(
      'ResizeObserver',
      class {
        cb: () => void;
        constructor(cb: () => void) {
          this.cb = cb;
        }
        observe(el: Element) {
          watched.set(el, this.cb);
        }
        disconnect() {}
      },
    );
    let win = 600;
    vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return this.classList.contains('timeline') ? 5000 : 0;
    });
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return this.classList.contains('timeline') ? win : 0;
    });
    const { app } = setup('idle', { history: long });
    await mount(app);
    const scroller = root.querySelector<HTMLElement>('.timeline')!;
    expect(scroller.scrollTop).toBe(5000);
    await act(async () => {
      scroller.dispatchEvent(new Event('wheel'));
    });
    win = 500;
    scroller.scrollTop = 4400;
    await act(async () => watched.get(scroller)?.());
    expect(scroller.scrollTop).toBe(4400);
    await act(async () => new Promise((r) => setTimeout(r, 450)));
    expect(scroller.scrollTop).toBe(5000);
  });

  it('in Chromium a wheel gesture holds nothing back', async () => {
    vi.spyOn(Object.getPrototypeOf(navigator), 'userAgent', 'get').mockReturnValue(
      'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36',
    );
    vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return this.classList.contains('timeline') ? this.querySelectorAll('.feed-block').length * 100 : 0;
    });
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return this.classList.contains('timeline') ? 600 : 0;
    });
    const { app } = setup('idle', { history: long });
    await mount(app);
    await settle();
    const scroller = root.querySelector<HTMLElement>('.timeline')!;
    await act(async () => {
      scroller.dispatchEvent(new Event('wheel'));
      scroller.scrollTop = 300;
      scroller.dispatchEvent(new Event('scroll'));
    });
    expect(blocks().length).toBe(120);
  });

  describe("the user's own scroll", () => {
    const CHROMIUM = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36';
    // Fake clocks for the settle, `performance.now()` included, after the chat is mounted.
    const clock = () => vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'performance'] });
    const tick = (ms: number) => act(async () => void (await vi.advanceTimersByTimeAsync(ms)));
    const on = (target: EventTarget, e: Event) => act(async () => void target.dispatchEvent(e));
    const touch = (type: string, y?: number) =>
      Object.assign(new Event(type, { bubbles: true }), { touches: y === undefined ? [] : [{ clientY: y }] });
    const pointer = (type: string, pointerType: string) =>
      Object.assign(new Event(type, { bubbles: true }), { pointerId: 1, pointerType, button: 0, ctrlKey: false });
    const scrollTo = (scroller: HTMLElement, top: number) =>
      act(async () => {
        scroller.scrollTop = top;
        scroller.dispatchEvent(new Event('scroll'));
      });
    /** The feed `height()` px high in a 600 px window, the chat mounted at its bottom. */
    async function feedOf(height: () => number) {
      vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockImplementation(function (this: HTMLElement) {
        return this.classList.contains('timeline') ? height() : 0;
      });
      vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(function (this: HTMLElement) {
        return this.classList.contains('timeline') ? 600 : 0;
      });
      const { app } = setup('idle', { history: long });
      await mount(app);
      await settle();
      return root.querySelector<HTMLElement>('.timeline')!;
    }
    const next = (seq: number) => act(async () => subscriber!.onEvent(ev('event.agent.message', seq, (e) => (e.payload.text = `more ${seq}`))));
    const blocksHigh = () => root.querySelectorAll('.feed-block').length * 100;

    it('while the agent writes, a wheel up lets go of the bottom though the feed grew under it', async () => {
      vi.spyOn(Object.getPrototypeOf(navigator), 'userAgent', 'get').mockReturnValue(CHROMIUM);
      let height = 5000;
      const scroller = await feedOf(() => height);
      await scrollTo(scroller, 4400);
      clock();
      try {
        // The wheel, and the answer grows by 200 px before the browser's scroll event for it
        // came: a position set now would drop the rest of the smooth scroll.
        await on(scroller, new WheelEvent('wheel', { deltaY: -100 }));
        height = 5200;
        await next(451);
        expect(scroller.scrollTop).toBe(4400);
        await scrollTo(scroller, 4000);
        height = 5400;
        await next(452);
        await tick(450);
        expect(scroller.scrollTop).toBe(4000);
      } finally {
        vi.useRealTimers();
      }
    });

    it('a wheel down does not let the move up the browser made as the feed changed go of the bottom', async () => {
      let height = 5000;
      const scroller = await feedOf(() => height);
      await scrollTo(scroller, 4400);
      clock();
      try {
        await on(scroller, new WheelEvent('wheel', { deltaY: 100 }));
        // As in the test of the images above: delivered after the feed grew again.
        height = 6000;
        await scrollTo(scroller, 4100);
        await tick(450);
        await next(451);
        expect(scroller.scrollTop).toBe(6000);
      } finally {
        vi.useRealTimers();
      }
    });

    it('a key that scrolls holds the follow back and lets go of the bottom on its way up; a key in a field does neither', async () => {
      vi.spyOn(Object.getPrototypeOf(navigator), 'userAgent', 'get').mockReturnValue(CHROMIUM);
      let height = 5000;
      const scroller = await feedOf(() => height);
      await scrollTo(scroller, 4400);
      const field = root.querySelector<HTMLTextAreaElement>('textarea')!;
      clock();
      try {
        await on(field, new KeyboardEvent('keydown', { key: ' ', bubbles: true }));
        height = 5200;
        await next(451);
        expect(scroller.scrollTop).toBe(5200);
        await scrollTo(scroller, 4600);
        await on(document.body, new KeyboardEvent('keydown', { key: 'PageUp', bubbles: true }));
        height = 5400;
        await next(452);
        expect(scroller.scrollTop).toBe(4600);
        await scrollTo(scroller, 4000);
        await tick(450);
        await next(453);
        expect(scroller.scrollTop).toBe(4000);
      } finally {
        vi.useRealTimers();
      }
    });

    it('the scrollbar held down holds the follow back until it is let go', async () => {
      vi.spyOn(Object.getPrototypeOf(navigator), 'userAgent', 'get').mockReturnValue(CHROMIUM);
      let height = 5000;
      const scroller = await feedOf(() => height);
      await scrollTo(scroller, 4400);
      clock();
      try {
        await on(scroller, pointer('pointerdown', 'mouse'));
        height = 5200;
        await next(451);
        await tick(450);
        expect(scroller.scrollTop).toBe(4400);
        await on(window, pointer('pointerup', 'mouse'));
        await tick(450);
        expect(scroller.scrollTop).toBe(5200);
      } finally {
        vi.useRealTimers();
      }
    });

    it('in WebKit a finger and its fling hold the older blocks back until the scroll settles', async () => {
      const scroller = await feedOf(blocksHigh);
      expect(blocks().length).toBe(60);
      expect(scroller.scrollTop).toBe(6000);
      clock();
      try {
        await on(scroller, touch('touchstart', 100));
        // The browser cancels the finger's pointer once the touch turns into a scroll.
        await on(scroller, pointer('pointercancel', 'touch'));
        await on(scroller, touch('touchmove', 400));
        await scrollTo(scroller, 300);
        // The finger rests on the feed longer than the scroll takes to settle.
        await tick(450);
        expect(blocks().length).toBe(60);
        await on(scroller, touch('touchend'));
        // The fling goes on with scroll events only.
        await tick(300);
        await scrollTo(scroller, 200);
        await tick(300);
        await scrollTo(scroller, 100);
        expect(blocks().length).toBe(60);
        expect(scroller.scrollTop).toBe(100);
        await tick(450);
        expect(blocks().length).toBe(120);
        expect(scroller.scrollTop).toBe(6100);
      } finally {
        vi.useRealTimers();
      }
    });

    it('in WebKit the bounce that comes after the last wheel event holds the older blocks back too', async () => {
      const scroller = await feedOf(blocksHigh);
      clock();
      try {
        await on(scroller, new WheelEvent('wheel', { deltaY: -100 }));
        await scrollTo(scroller, 500);
        await tick(300);
        // The feed hit its top and bounces back, 600 ms after the last wheel event.
        await scrollTo(scroller, 0);
        await tick(300);
        await scrollTo(scroller, 20);
        expect(blocks().length).toBe(60);
        await tick(450);
        expect(blocks().length).toBe(120);
        expect(scroller.scrollTop).toBe(6020);
      } finally {
        vi.useRealTimers();
      }
    });
  });
});

describe('read cursor', () => {
  it('an opened chat is read up to its last event, and the session asked for before stays read', async () => {
    const { api, app } = setup('idle');
    const unread = { ...app.sessions.get('s1')!, unread: 1, last_read_seq: 6 };
    app.sessions.set('s1', unread);
    // An idle session without commands is asked for again; the hub counted it before the read.
    let answer!: () => void;
    api.session.mockImplementationOnce(() => new Promise((r) => (answer = () => r({ ...unread, commands: undefined }))));
    await mount(app);
    expect(api.readCursor).toHaveBeenCalledWith('s1', 7);
    expect(app.sessions.get('s1')).toMatchObject({ unread: 0, last_read_seq: 7 });
    await act(async () => {
      answer();
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(app.sessions.get('s1')).toMatchObject({ unread: 0, last_read_seq: 7 });
  });
});

describe('an input that came while the agent writes (SPEC §6)', () => {
  const first = ev('event.user.message', 1, (e) => (e.payload.artifact_ids = []));
  const meanwhile = (seq: number) =>
    ev('event.user.message', seq, (e) => {
      e.event_id = `0199f3a2-7c10-7000-8000-0000000003${seq}0`;
      e.payload = { text: 'written meanwhile', artifact_ids: [], during_text: true };
    });
  /** The chat of `history`, mounted, with the agent writing a text; `push` brings live events. */
  async function writing(history: HqcEvent[]) {
    const { app, api } = setup('running', { history });
    let push!: (e: HqcEvent) => void;
    (app.socket as unknown as { subscribe: unknown }).subscribe = (_id: string, s: { onEvent: (e: HqcEvent) => void }) => {
      push = s.onEvent;
      return () => {};
    };
    await mount(app);
    await act(async () => push(ev('event.agent.delta', 0, (e) => delete e.seq)));
    return { api, push };
  }
  const under = (el: Element) => !!($('.msg-streaming')!.compareDocumentPosition(el) & Node.DOCUMENT_POSITION_FOLLOWING);
  const block = (text: string) => [...root.querySelectorAll('.feed-block')].find((b) => b.textContent?.includes(text))!;

  it('stands under the text being written', async () => {
    const { push } = await writing([first]);
    await act(async () => push(meanwhile(2)));
    expect(under(block('written meanwhile'))).toBe(true);
  });

  it('a message not stored yet stands under it too', async () => {
    const { api } = await writing([first]);
    Object.assign(api, { input: vi.fn(() => new Promise(() => {})) });
    const area = $('.composer-input') as HTMLTextAreaElement;
    await act(() => {
      area.value = 'not stored yet';
      area.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await act(async () => $('.composer button[type=submit]')!.click());
    expect(under($('.msg-pending')!)).toBe(true);
  });

  it('one queued while the turn runs stands under the text and under the inputs written during it', async () => {
    const { push } = await writing([first]);
    const queued = ev('event.user.message', 2, (e) => {
      e.event_id = '0199f3a2-7c10-7000-8000-000000000320';
      e.payload = { text: 'queued meanwhile', artifact_ids: [] };
      e.delivery = 'pending';
      delete e.delivered_at;
    });
    await act(async () => push(queued));
    await act(async () => push(meanwhile(3)));
    expect(under(block('queued meanwhile'))).toBe(true);
    expect(block('written meanwhile').compareDocumentPosition(block('queued meanwhile')) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(block('queued meanwhile').querySelector('.msg-time')!.textContent).toMatch(/ · queued$/);
  });

  it('one that came during an earlier text stays above it', async () => {
    const answer = ev('event.agent.message', 3, (e) => (e.payload = { text: 'the earlier answer', final: true }));
    const idle = ev('event.status', 4, (e) => (e.payload = { state: 'idle' }));
    await writing([first, meanwhile(2), answer, idle]);
    expect(under(block('written meanwhile'))).toBe(false);
  });

  it('the thinking written after the text being written stands under it, unfolded, above the inputs written meanwhile', async () => {
    const call = ev('event.tool.call', 2);
    const done = ev('event.tool.result', 3, (e) => (e.payload = { call_id: 'toolu_01', result_preview: 'ok', error: false }));
    const { push } = await writing([first, call, done]);
    await act(async () =>
      push(
        ev('event.agent.delta', 0, (e) => {
          delete e.seq;
          e.payload = { text: 'Weighing it', thinking: true };
        }),
      ),
    );
    await act(async () => push(meanwhile(4)));
    const live = $('details.thinking')!;
    expect(live.querySelector('summary')!.textContent).toBe('Thinking · Weighing it');
    expect(under(live)).toBe(true);
    expect(live.compareDocumentPosition(block('written meanwhile')) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it('one that came during the thinking before the text stays above it', async () => {
    const thought = ev('event.agent.thinking', 3, (e) => (e.payload = { text: 'Checking' }));
    await writing([first, meanwhile(2), thought]);
    expect(under(block('written meanwhile'))).toBe(false);
  });
});

// SPEC §6: the client keeps the chats used most recently in memory.
describe('switching chats', () => {
  /** The socket's subscriptions, each as it was made. */
  function subscriptions(app: AppState) {
    const subs: SessionSubscriber[] = [];
    const subscribe = vi.fn((_id: string, sub: SessionSubscriber) => {
      subs.push(sub);
      return () => {};
    });
    Object.assign(app.socket, { subscribe });
    return { subs, subscribe };
  }
  /** `mount`, then what the chat's opening causes: run alone, these tests saw the mount's
   *  effects run only as its `act` ended, before the chat had subscribed. */
  async function mountChat(app: AppState) {
    await mount(app);
    await act(async () => void (await new Promise((r) => setTimeout(r, 20))));
  }

  it('a chat whose first page is not in yet shows a spinner and "Loading…" in the empty feed', async () => {
    const { api, app } = setup('idle');
    api.eventsBefore.mockReturnValue(new Promise(() => {}) as never);
    await mountChat(app);
    const loading = root.querySelector('.feed > .feed-note > .loading.loading-center')!;
    expect(loading.getAttribute('role')).toBe('status');
    expect(loading.textContent).toBe('Loading…');
    expect(loading.querySelector('.spinner')!.getAttribute('aria-hidden')).toBe('true');
    expect(root.querySelectorAll('.feed > .feed-note')).toHaveLength(1);
  });

  it('"Requests only" of a log without requests says "No requests" in the middle of the feed, and not while its first page loads', async () => {
    const { api, app } = setup('idle', { history: [HISTORY[0], HISTORY[1]] });
    app.sessions.get('s1')!.system = true;
    const page = api.eventsBefore.getMockImplementation()!;
    let release!: () => void;
    const held = new Promise<void>((r) => (release = r));
    api.eventsBefore.mockImplementation(async (...a: Parameters<typeof page>) => {
      await held;
      return page(...a);
    });
    await mountChat(app);
    await act(async () => button('Requests only')!.click());
    const notes = () => [...root.querySelectorAll('.feed > .feed-note')].map((n) => n.textContent);
    expect(notes()).toEqual(['Loading…']);
    await act(async () => release());
    await act(async () => void (await new Promise((r) => setTimeout(r, 20))));
    expect(notes()).toEqual(['No requests']);
  });

  it('a chat opened before shows its feed in the first frame, resubscribes after its last seq, then fetches its newest page again', async () => {
    const { api, app } = setup('idle');
    const { subs, subscribe } = subscriptions(app);
    await mountChat(app);
    expect(subs.map((s) => s.afterSeq())).toEqual([7]);
    await act(async () => render(null, root));

    root = document.createElement('div');
    document.body.appendChild(root);
    // No act: the frame before any effect has run.
    render(<Chat app={app} sessionId="s1" />, root);
    expect(root.textContent).not.toContain('Loading…');
    expect(root.querySelector('.feed')!.textContent).toContain('All tests pass.');
    await act(async () => void (await new Promise((r) => setTimeout(r, 100))));
    expect(subs.map((s) => s.afterSeq())).toEqual([7, 7]);
    expect(api.eventsBefore).toHaveBeenCalledTimes(2);
    expect(subscribe.mock.invocationCallOrder[1]).toBeLessThan(api.eventsBefore.mock.invocationCallOrder[1]);
  });

  it('a kept chat takes the delivery the hub changed while it was closed from its newest page, not new events or lastSeq', async () => {
    const input = (delivery?: 'failed') =>
      ev('event.user.message', 1, (e) => {
        e.payload.artifact_ids = [];
        if (delivery) {
          e.delivery = delivery;
          delete e.delivered_at;
        }
      });
    const { api, app } = setup('idle', { history: [input('failed'), ev('event.agent.message', 2)] });
    await mountChat(app);
    expect($('.btn-retry')).not.toBeNull();
    await act(async () => render(null, root));
    // Retried from another device and delivered there, which the hub told no subscription
    // here; and a newer event, which the subscription brings.
    api.eventsBefore.mockImplementation(async () => ({
      events: [input(), ev('event.agent.message', 2), ev('event.agent.message', 3)],
      invalid: [],
      maxSeq: 3,
      minSeq: 1,
      count: 3,
    }));
    await mountChat(app);
    expect($('.btn-retry')).toBeNull();
    const t = app.chat('s1').timeline;
    expect(t.lastSeq).toBe(2);
    expect(t.events().map((e) => e.seq)).toEqual([1, 2]);
  });

  it('a half-written answer does not carry over to the next open: the hub sends it whole', async () => {
    const { app } = setup('idle', { history: HISTORY.slice(0, 3) });
    const { subs } = subscriptions(app);
    const delta = (text: string) =>
      ev('event.agent.delta', 0, (e) => {
        delete e.seq;
        e.payload.text = text;
      });
    await mountChat(app);
    await act(async () => subs[0].onEvent(delta('Hel')));
    expect($('.msg-streaming')!.textContent).toBe('Hel');
    await act(async () => render(null, root));
    await mountChat(app);
    await act(async () => subs[1].onEvent(delta('Hello')));
    expect($('.msg-streaming')!.textContent).toBe('Hello');
  });

  it('a chat closed and opened again while its page loads subscribes once', async () => {
    const { api, app } = setup('idle');
    const { subscribe } = subscriptions(app);
    let page!: () => void;
    const tail = api.eventsBefore.getMockImplementation()!;
    api.eventsBefore.mockImplementationOnce(
      (id, before, limit) => new Promise((r) => (page = () => r(tail(id, before, limit)))),
    );
    const chat = app.chat('s1');
    const first = chat.open();
    chat.close();
    const second = chat.open();
    page();
    await Promise.all([first, second]);
    expect(subscribe).toHaveBeenCalledTimes(1);
  });

  it('keeps the 12 most recently used chats, and never drops an open one', async () => {
    const { app } = setup('idle');
    const open = app.chat('s1');
    await open.open();
    const others = Array.from({ length: 12 }, (_, i) => app.chat(`o${i}`));
    expect(app.chat('s1')).toBe(open);
    expect(open.isOpen).toBe(true);
    expect(app.chat('o11')).toBe(others[11]);
    expect(app.chat('o0')).not.toBe(others[0]);
  });
});

describe('fork', () => {
  it('forks before a message of the user: the new session opens with the message as its draft', async () => {
    const { app, api } = setup('idle');
    await mount(app);
    const forks = [...root.querySelectorAll<HTMLButtonElement>('.btn-fork')];
    // The user's two messages; none on the agent's.
    expect(forks).toHaveLength(2);
    expect(root.querySelector('.msg-agent .btn-fork')).toBeNull();
    await act(async () => {
      forks[1].click();
      await new Promise((r) => setTimeout(r, 10));
    });
    const second = HISTORY[3] as EventOf<'user.message'>;
    expect(api.forkSession).toHaveBeenCalledWith('s1', second.event_id);
    expect(app.sessions.get('s2')!.title).toBe('test 12 (fork)');
    expect(app.draft('s2')).toBe(second.payload.text);
    expect(location.hash).toBe('#/s/s2');
  });

  it('while the hub makes the fork the message says so, and a second click makes no second session', async () => {
    const { app, api } = setup('idle');
    // The hub answers once it has copied the history: seconds for a long session.
    let answer!: () => void;
    const made = await api.forkSession();
    api.forkSession.mockClear();
    location.hash = '';
    api.forkSession.mockImplementationOnce(() => new Promise((r) => (answer = () => r(made))));
    await mount(app);
    const second = HISTORY[3] as EventOf<'user.message'>;
    const fork = () => root.querySelectorAll<HTMLButtonElement>('.btn-fork')[1];
    await act(async () => {
      fork().click();
      await new Promise((r) => setTimeout(r, 10));
    });
    expect($('.forking')?.textContent).toBe(' · forking…');
    expect(fork().disabled).toBe(true);
    // Another request for the same message, as a click that reached the button would make.
    expect(await app.forkSession('s1', second.event_id, second.payload.text)).toBeUndefined();
    expect(api.forkSession).toHaveBeenCalledTimes(1);
    await act(async () => {
      answer();
      await new Promise((r) => setTimeout(r, 10));
    });
    expect($('.forking')).toBeNull();
    expect(fork().disabled).toBe(false);
    expect(location.hash).toBe('#/s/s2');
  });

  it('a fork the hub refuses frees the message: no "forking", the button works, the reason shown', async () => {
    const { app, api } = setup('idle');
    api.forkSession.mockRejectedValueOnce(new Error('boom'));
    await mount(app);
    const fork = () => root.querySelectorAll<HTMLButtonElement>('.btn-fork')[1];
    await act(async () => {
      fork().click();
      await new Promise((r) => setTimeout(r, 10));
    });
    expect($('.forking')).toBeNull();
    expect(fork().disabled).toBe(false);
    expect(root.textContent).toContain('Not forked: boom');
    await act(async () => {
      fork().click();
      await new Promise((r) => setTimeout(r, 10));
    });
    expect(api.forkSession).toHaveBeenCalledTimes(2);
  });

  it('the service session has no fork', async () => {
    const { app } = setup('idle');
    root = document.createElement('div');
    document.body.appendChild(root);
    await act(async () => {
      render(<Chat app={app} sessionId="sys" />, root);
      await new Promise((r) => setTimeout(r, 50));
    });
    expect(root.querySelector('.btn-fork')).toBeNull();
  });
});

describe('draft', () => {
  const area = () => $('.composer-input') as HTMLTextAreaElement;
  const type = (text: string) =>
    act(() => {
      area().value = text;
      area().dispatchEvent(new Event('input', { bubbles: true }));
    });

  it('keeps an unsent message when the chat closes, on this device too, and drops it once sent', async () => {
    let { app } = setup('idle');
    await mount(app);
    await type('not sent yet');
    render(null, root);
    expect(app.draft('s1')).toBe('not sent yet');

    // Another page load reads it from the device.
    ({ app } = setup('idle'));
    await mount(app);
    expect(area().value).toBe('not sent yet');
    await type('');
    expect(app.draft('s1')).toBe('');
    expect(localStorage.getItem('hqc.drafts')).toBe('{}');
  });
});

describe('Quote', () => {
  const area = () => $('.composer-input') as HTMLTextAreaElement;
  /** The text node of the feed (or of `scope`) that reads `text`. */
  const textNode = (text: string, scope = '.feed') => {
    const w = document.createTreeWalker($(scope)!, NodeFilter.SHOW_TEXT);
    for (let n = w.nextNode(); n; n = w.nextNode()) if (n.textContent === text) return n;
    throw new Error(`no text «${text}»`);
  };
  /** Selects `node`'s text from `from` to `to` (all of it by default), or nothing. */
  const select = (node: Node | null, from = 0, to?: number) =>
    act(async () => {
      const sel = document.getSelection()!;
      sel.removeAllRanges();
      if (node) {
        const r = document.createRange();
        r.setStart(node, from);
        r.setEnd(node, to ?? node.textContent!.length);
        sel.addRange(r);
      }
      document.dispatchEvent(new Event('selectionchange'));
    });
  const agentButton = (cls: string) => $(`.msg-agent .msg-buttons ${cls}`) as HTMLButtonElement | null;
  const withAgentText = (text: string) => [HISTORY[0], ev('event.agent.message', 2, (e) => (e.payload.text = text)), ...HISTORY.slice(2, 3)];
  const clipboard = () => {
    const writeText = vi.fn(async (_: string) => {});
    vi.stubGlobal('isSecureContext', true);
    Object.defineProperty(window.navigator, 'clipboard', { value: { writeText }, configurable: true });
    return writeText;
  };

  it('quotes the text selected in a message from its Quote icon, after the draft, and focuses the field', async () => {
    const { app } = setup('idle');
    await mount(app);
    await act(() => {
      area().value = 'Look at this';
      area().dispatchEvent(new Event('input', { bubbles: true }));
    });
    await select(textNode('All tests pass.'), 4, 9);
    expect(agentButton('.btn-msg-quote')!.getAttribute('aria-label')).toBe('Quote');
    await act(() => agentButton('.btn-msg-quote')!.click());
    expect(area().value).toBe('Look at this\n\n> tests\n\n');
    expect(document.activeElement).toBe(area());
    expect(area().selectionStart).toBe(area().value.length);
    expect(document.getSelection()!.rangeCount).toBe(0);
    // The next quote goes after the first.
    await select(textNode('All tests pass.'));
    await act(() => agentButton('.btn-msg-quote')!.click());
    expect(area().value).toBe('Look at this\n\n> tests\n\n> All tests pass.\n\n');
    // The pill and the menu of the old Quote are gone.
    expect($('.btn-quote')).toBeNull();
    expect($('.ctx-menu')).toBeNull();
  });

  it('quotes the first two sentences of a message with nothing selected, with «…» when more follows', async () => {
    const { app } = setup('idle', { history: withAgentText('First one. Second one! Third one? Fourth.') });
    await mount(app);
    await act(() => agentButton('.btn-msg-quote')!.click());
    expect(area().value).toBe('> First one. Second one! …\n\n');
    await act(() => {
      area().value = '';
      area().dispatchEvent(new Event('input', { bubbles: true }));
    });
    // A selection in another message does not count: this one is quoted by its text.
    await select(textNode('Run the tests', '.msg-user'), 0, 3);
    await act(() => agentButton('.btn-msg-quote')!.click());
    expect(area().value).toBe('> First one. Second one! …\n\n');
  });

  it('quotes a short message whole, without «…»', async () => {
    const { app } = setup('idle', { history: withAgentText('One. Two.') });
    await mount(app);
    await act(() => agentButton('.btn-msg-quote')!.click());
    expect(area().value).toBe('> One. Two.\n\n');
  });

  const twoParagraphs = () => [HISTORY[0], ev('event.agent.message', 2, (e) => (e.payload.text = 'Para one.\n\nPara two.')), HISTORY[3]];
  const selectRange = (from: [Node, number], to: [Node, number]) =>
    act(async () => {
      const r = document.createRange();
      r.setStart(...from);
      r.setEnd(...to);
      document.getSelection()!.removeAllRanges();
      document.getSelection()!.addRange(r);
      document.dispatchEvent(new Event('selectionchange'));
    });

  it('a triple click, which ends at the start of the next block, is the message\'s paragraph only', async () => {
    const writeText = clipboard();
    const { app } = setup('idle', { history: twoParagraphs() });
    await mount(app);
    const next = $('.msg-agent')!.closest('.feed-block')!.nextElementSibling!;
    await selectRange([textNode('Para two.'), 0], [next, 0]);
    expect(document.querySelectorAll(SEARCH).length).toBe(1);
    expect(agentButton(SEARCH)).not.toBeNull();
    await act(async () => agentButton(COPY)!.click());
    expect(writeText).toHaveBeenLastCalledWith('Para two.');
    await act(() => agentButton('.btn-msg-quote')!.click());
    expect(area().value).toBe('> Para two.\n\n');
    vi.unstubAllGlobals();
  });

  it('quotes the paragraphs of a clipped range on separate lines, and a range that enters a message leaves its time out', async () => {
    const { app } = setup('idle', { history: twoParagraphs() });
    await mount(app);
    const next = $('.msg-agent')!.closest('.feed-block')!.nextElementSibling!;
    await selectRange([textNode('Para one.'), 0], [next, 0]);
    await act(() => agentButton('.btn-msg-quote')!.click());
    expect(area().value).toBe('> Para one.\n>\n> Para two.\n\n');
    await act(() => {
      area().value = '';
      area().dispatchEvent(new Event('input', { bubbles: true }));
    });
    // From outside any message into the middle of one: its time (and buttons) are not text.
    await selectRange([$('.feed')!, 0], [textNode('Para two.'), 4]);
    await act(() => agentButton('.btn-msg-quote')!.click());
    expect(area().value).toBe('> Para one.\n>\n> Para\n\n');
  });

  it('puts a space before «…» after a sentence mark, and skips the blank line between paragraphs', () => {
    expect(firstSentences('Done.\n\nThe tests pass. Next.')).toBe('Done.\n\nThe tests pass. …');
    expect(firstSentences('Done.\n\nThe tests pass.')).toBe('Done.\n\nThe tests pass.');
    expect(firstSentences('One. Two. Three')).toBe('One. Two. …');
    expect(firstSentences('one\n\ntwo\n\nthree')).toBe('one\n\ntwo…');
  });

  it('lets go of a held press the browser took over (pointercancel), and reads the selection anew at a press', async () => {
    // Without the open request of HISTORY: on a phone it would take the composer's place.
    vi.stubGlobal('matchMedia', (media: string) => ({ matches: true, media, addEventListener: () => {}, removeEventListener: () => {} }));
    try {
      const { app } = setup('idle', { history: HISTORY.slice(0, 3) });
      // The chat never hears of the selection changing: only a press tells it.
      const add = document.addEventListener.bind(document);
      vi.spyOn(document, 'addEventListener').mockImplementation(((type: string, ...rest: [any]) => {
        if (type !== 'selectionchange') add(type, ...rest);
      }) as typeof document.addEventListener);
      await mount(app, true);
      const row = $('.msg-agent .msg-buttons')!;
      const press = () =>
        act(() => void row.querySelector('.btn-msg-quote, .btn-copy')!.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, cancelable: true })));
      // A selection the chat has not heard of: the press reads it.
      await act(() => {
        const r = document.createRange();
        r.selectNodeContents(textNode('All tests pass.'));
        document.getSelection()!.removeAllRanges();
        document.getSelection()!.addRange(r);
      });
      expect(row.querySelector(SEARCH)).toBeNull();
      await press();
      expect(row.querySelector(SEARCH)).not.toBeNull();
      await select(null);
      expect(row.querySelector(SEARCH), 'held').not.toBeNull();
      await act(() => void document.dispatchEvent(new Event('pointercancel')));
      expect(row.querySelector(SEARCH), 'let go').toBeNull();
    } finally {
      vi.restoreAllMocks();
      vi.unstubAllGlobals();
    }
  });

  it('quotes the earlier selection when iOS has cleared it before the press', async () => {
    vi.stubGlobal('matchMedia', (media: string) => ({ matches: true, media, addEventListener: () => {}, removeEventListener: () => {} }));
    // The chat hears of the selection once, as it changed; what the test does after is not heard.
    let heard: (() => void) | undefined;
    const add = document.addEventListener.bind(document);
    vi.spyOn(document, 'addEventListener').mockImplementation(((type: string, ...rest: [any]) => {
      if (type === 'selectionchange') heard = rest[0];
      else add(type, ...rest);
    }) as typeof document.addEventListener);
    try {
      const { app } = setup('idle', { history: HISTORY.slice(0, 3) });
      await mount(app, true);
      const row = $('.msg-agent .msg-buttons')!;
      await act(() => {
        const r = document.createRange();
        r.setStart(textNode('All tests pass.'), 4);
        r.setEnd(textNode('All tests pass.'), 9);
        document.getSelection()!.removeAllRanges();
        document.getSelection()!.addRange(r);
        heard!();
      });
      expect(row.querySelector(SEARCH)).not.toBeNull();
      // Cleared before the pointerdown reached the page.
      document.getSelection()!.removeAllRanges();
      await act(() => void row.querySelector('.btn-msg-quote')!.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, cancelable: true })));
      expect(row.querySelector(SEARCH), 'still offered').not.toBeNull();
      await act(() => row.querySelector<HTMLButtonElement>('.btn-msg-quote')!.click());
      expect(area().value).toBe('> tests\n\n');
    } finally {
      vi.restoreAllMocks();
      vi.unstubAllGlobals();
    }
  });

  it('a range across two messages is the first one\'s part, held by the first only', async () => {
    const { app } = setup('idle', { history: twoParagraphs() });
    await mount(app);
    await selectRange([textNode('Para two.'), 5], [[...root.querySelectorAll('.msg-user .msg-text')].pop()!.firstChild!, 3]);
    expect(document.querySelectorAll(SEARCH).length).toBe(1);
    expect(agentButton(SEARCH)).not.toBeNull();
    await act(() => agentButton('.btn-msg-quote')!.click());
    expect(area().value).toBe('> two.\n\n');
  });

  it('has Search, Copy and Quote on the user\'s message, the Search slot hidden until it holds a selection', async () => {
    const { app } = setup('idle');
    await mount(app);
    const labels = () =>
      [...$('.msg-user .msg-buttons')!.querySelectorAll('button')].map((b) => b.getAttribute('aria-label')).filter((l) => l !== 'Fork from here');
    const slot = () => $('.msg-user .btn-msg-search') as HTMLElement;
    expect(labels()).toEqual(['Search in Google', 'Copy', 'Quote']);
    expect(slot().classList.contains('btn-msg-search-idle')).toBe(true);
    await select(textNode('Run the tests', '.msg-user'));
    expect(labels()).toEqual(['Search in Google', 'Copy', 'Quote']);
    expect(slot().classList.contains('btn-msg-search-idle')).toBe(false);
  });

  it('copies the text selected in the message, else the whole message', async () => {
    const writeText = clipboard();
    const { app } = setup('idle');
    await mount(app);
    await act(async () => agentButton(COPY)!.click());
    expect(writeText).toHaveBeenLastCalledWith('All tests pass.');
    await select(textNode('All tests pass.'), 4, 9);
    await act(async () => agentButton(COPY)!.click());
    expect(writeText).toHaveBeenLastCalledWith('tests');
    vi.unstubAllGlobals();
  });

  it('offers Search only in the message that holds the selection, with the search picked in Settings', async () => {
    const open = vi.fn();
    vi.stubGlobal('open', open);
    const { app } = setup('idle');
    await mount(app);
    expect($(SEARCH)).toBeNull();
    await select(textNode('All tests pass.'), 4, 9);
    expect(document.querySelectorAll(SEARCH).length).toBe(1);
    expect(agentButton(SEARCH)!.title).toBe('Search in Google');
    await act(async () => agentButton(SEARCH)!.click());
    expect(open).toHaveBeenCalledWith('https://www.google.com/search?q=tests', '_blank', 'noopener');
    // Text outside the feed is not a message's.
    await select(textNode('test 12', '.chat-head'));
    expect($(SEARCH)).toBeNull();
    await act(() => app.setWebSearch(WEB_SEARCHES.find((w) => w.id === 'yandex')!));
    await select(textNode('All tests pass.'));
    expect(agentButton(SEARCH)!.title).toBe('Search in Yandex');
    await act(async () => agentButton(SEARCH)!.click());
    expect(open).toHaveBeenLastCalledWith('https://yandex.ru/search/?text=All%20tests%20pass.', '_blank', 'noopener');
    // Kept on this device: a reload starts with it.
    expect(storedWebSearch().id).toBe('yandex');
    act(() => app.setWebSearch(WEB_SEARCHES[0]));
    vi.unstubAllGlobals();
  });

  it('keeps a press that clears the selection (a phone\'s tap): the buttons stay and act on the text held', async () => {
    try {
      const open = vi.fn();
      vi.stubGlobal('open', open);
      // The message's buttons are a phone's row where the screen is narrow (`useIsPhone`).
      vi.stubGlobal('matchMedia', (media: string) => ({ matches: true, media, addEventListener: () => {}, removeEventListener: () => {} }));
      // Without the open request of HISTORY: on a phone it would take the composer's place.
      const { app } = setup('idle', { history: HISTORY.slice(0, 3) });
      await mount(app, true);
      const row = $('.msg-agent .msg-buttons')!;
      // A phone's usage line holds them, the time line none.
      expect($('.msg-agent .msg-time .msg-buttons')).toBeNull();
      expect($('.msg-agent > .meta .msg-buttons')).not.toBeNull();
      await select(textNode('All tests pass.'), 4, 9);
      const search = row.querySelector<HTMLButtonElement>(SEARCH)!;
      const quote = row.querySelector<HTMLButtonElement>('.btn-msg-quote')!;
      /** A pointer press on `el`: whether it was cancelled, which keeps a mouse's selection. */
      const press = async (el: Element) => {
        let cancelled = false;
        await act(() => {
          cancelled = !el.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, cancelable: true }));
        });
        return cancelled;
      };
      expect(await press(quote), 'a mouse press keeps the selection and the focus').toBe(true);
      await select(null);
      expect(row.querySelector(SEARCH), 'still there, though the selection is gone').toBe(search);
      await act(() => quote.click());
      expect(area().value).toBe('> tests\n\n');
      expect(row.querySelector(SEARCH), 'gone with the selection after the click').toBeNull();
      await select(textNode('All tests pass.'), 4, 9);
      await press(row.querySelector(SEARCH)!);
      await select(null);
      await act(() => row.querySelector<HTMLButtonElement>(SEARCH)!.click());
      expect(open).toHaveBeenCalledWith('https://www.google.com/search?q=tests', '_blank', 'noopener');
      // A press elsewhere lets go of the held text.
      await select(textNode('All tests pass.'), 0, 3);
      await press(quote);
      await select(null);
      await act(() => void $('.chat-head')!.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true })));
      expect(row.querySelector(SEARCH)).toBeNull();
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it('has no Quote without a composer, but Search, and no pill or client menu anywhere', async () => {
    const locked = setup('locked_vscode');
    await mount(locked.app);
    await select(textNode('All tests pass.'));
    expect(agentButton(COPY)).not.toBeNull();
    expect(agentButton('.btn-msg-quote')).toBeNull();
    // Search needs no input.
    expect(agentButton(SEARCH)).not.toBeNull();
    expect($('.btn-quote')).toBeNull();
  });

  it('leaves the browser\'s menu to a right click, on a selection too', async () => {
    const { app } = setup('idle');
    await mount(app);
    await select(textNode('All tests pass.'));
    let kept = false;
    await act(() => {
      kept = textNode('All tests pass.').parentElement!.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }));
    });
    expect(kept, 'not prevented').toBe(true);
    expect($('.ctx-menu')).toBeNull();
  });

  it('quotes every line, a blank one as a bare «>», after a blank line', () => {
    expect(withQuote('', 'one')).toBe('> one\n\n');
    expect(withQuote('  \n', 'one')).toBe('> one\n\n');
    expect(withQuote('draft\n\n', 'one\n\n  two\r\nthree')).toBe('draft\n\n> one\n>\n>   two\n> three\n\n');
  });
});

describe('picked files', () => {
  it('a HEIC still being decoded holds the message back, and shows when it is read', async () => {
    const { app } = setup('idle');
    await mount(app);
    // Brave on a computer: the browser does not decode HEIC, the worker answers when told to.
    let answer: (() => void) | null = null;
    vi.stubGlobal(
      'Worker',
      class {
        onmessage: ((e: { data: Blob }) => void) | null = null;
        onerror: (() => void) | null = null;
        postMessage() {
          answer = () => this.onmessage?.({ data: new Blob(['jpeg'], { type: 'image/jpeg' }) });
        }
        terminate() {}
      },
    );
    const decode = vi.spyOn(HTMLImageElement.prototype, 'decode').mockRejectedValue(new DOMException('', 'EncodingError'));
    try {
      // A text typed first: Send would be on but for the file being read.
      const area = $('.composer textarea') as HTMLTextAreaElement;
      await act(async () => {
        area.value = 'look';
        area.dispatchEvent(new Event('input', { bubbles: true }));
      });
      const input = $('input[type=file]') as HTMLInputElement;
      const heic = new File([new Uint8Array([0, 0, 0, 24, 102, 116, 121, 112])], 'HQC.heic', { type: 'image/heic' });
      Object.defineProperty(input, 'files', { value: [heic], configurable: true });
      await act(async () => void input.dispatchEvent(new Event('change', { bubbles: true })));
      await vi.waitFor(() => expect($('.composer-files .attachment-chip')?.textContent).toBe('reading 1 file…'));
      expect(($('button[type=submit]') as HTMLButtonElement).disabled).toBe(true);
      // The file is read before it goes to the worker.
      await vi.waitFor(() => expect(answer).not.toBeNull());
      expect(($('button[type=submit]') as HTMLButtonElement).disabled).toBe(true);
      await act(async () => answer!());
      await vi.waitFor(() => expect($('.composer-files .attachment-thumb')?.getAttribute('title')).toBe('HQC.jpg'));
      expect(root.querySelector('.composer-files')!.textContent).not.toContain('reading');
      expect(($('button[type=submit]') as HTMLButtonElement).disabled).toBe(false);
    } finally {
      decode.mockRestore();
      vi.unstubAllGlobals();
    }
  });
});
