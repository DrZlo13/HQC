// @vitest-environment happy-dom
// T26: the chat screen — the header (state pill, worker, path, model and cost of the
// session, "Stop", "More"), the composer in one row, the VS Code strip in its place, the
// request's hotkeys through the chat, and the phone's bottom sheet with the text answer.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import type { Api, SessionPatch } from '../src/api';
import { AppState } from '../src/app';
import {
  parseEvent,
  type HeldBy,
  type HqcEvent,
  type PermissionMode,
  type EventOf,
  type SessionInfo,
  type SessionState,
  type SpawnedBy,
} from '../src/proto';
import { AUTO_CONFIRM, modelName, shownModel } from '../src/ui/labels';
import type { HubSocket, SessionSubscriber } from '../src/ws';
import { Chat, withQuote } from '../src/ui/Chat';

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
    spawned_by?: SpawnedBy;
    permission_mode?: PermissionMode;
    effective_permission_mode?: PermissionMode;
    permission_capped_by?: string;
    subagents?: number;
    background_tasks?: number;
    model?: string;
  } = {},
) {
  const history = opts.history ?? HISTORY;
  const api = {
    // GET /api/sessions/{id}: the session object with the commands the hub has by now.
    session: vi.fn(async () => ({ ...info, commands: opts.fetched })),
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
    approve: vi.fn(async () => {}),
    takeover: vi.fn(async () => ({})),
    // PATCH /api/sessions/{id}: the hub stores the mode, the worker confirms it at once; the
    // model is stored as sent (`null`: the runtime's default).
    patchSession: vi.fn(async (_id: string, patch: SessionPatch) => ({
      ...info,
      permission_mode: patch.permission_mode,
      effective_permission_mode: patch.permission_mode,
      model: patch.model === undefined ? info.model : (patch.model ?? undefined),
    })),
    // POST /api/sessions/{id}/fork: the new session.
    forkSession: vi.fn(async () => ({ ...info, id: 's2', title: 'test 12 (fork)', last_seq: 4, last_read_seq: 4 })),
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
    model: opts.model,
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

// T38: the session's permission mode in the header, and the switch in "More".
describe('permission mode', () => {
  const mark = () => ({ auto: $('.perm-auto')?.textContent, ask: $('.perm-ask')?.textContent });
  const menu = async () => {
    await act(async () => $('.btn-more')!.click());
    return [...root.querySelectorAll<HTMLButtonElement>('.menu-item')];
  };

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
      expect(items.map((b) => b.textContent)).toContain('Switch to Auto');
      await act(async () => items.find((b) => b.textContent === 'Switch to Auto')!.click());
      expect(confirm).toHaveBeenCalledWith(AUTO_CONFIRM);
      expect(AUTO_CONFIRM).toContain("deny list is the only limit");
      expect(AUTO_CONFIRM.includes('\n')).toBe(false);
      expect(api.patchSession).not.toHaveBeenCalled();

      confirm.mockReturnValue(true);
      items = await menu();
      await act(async () => {
        items.find((b) => b.textContent === 'Switch to Auto')!.click();
        await new Promise((r) => setTimeout(r, 10));
      });
      expect(api.patchSession).toHaveBeenCalledWith('s1', { permission_mode: 'auto' });
      expect(mark().auto).toBe('Auto');

      confirm.mockClear();
      items = await menu();
      expect(items.map((b) => b.textContent)).toContain('Switch to Ask');
      await act(async () => {
        items.find((b) => b.textContent === 'Switch to Ask')!.click();
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
});

// The session's model in "More": a choice PATCHes at once, a typed name when it is done; `null`
// goes back to the runtime's default.
describe('model', () => {
  const field = <E extends HTMLElement = HTMLSelectElement>(name: string) =>
    root.querySelector<E>(`.menu-settings [name=${name}]`);
  const set = (name: string, value: string) =>
    act(async () => {
      const el = field<HTMLInputElement>(name)!;
      el.value = value;
      el.dispatchEvent(new Event('input', { bubbles: true }));
      el.dispatchEvent(new Event('change', { bubbles: true }));
      await new Promise((r) => setTimeout(r, 10));
    });
  const open = () => act(async () => $('.btn-more')!.click());

  it('switches the model: an alias at once, another name once typed, the default as null', async () => {
    const { app, api } = setup('running');
    await mount(app);
    await open();
    expect(field('model')!.value).toBe('');
    expect([...field('model')!.options].map((o) => o.textContent)).toEqual([
      "Claude Code's default",
      'Opus',
      'Sonnet',
      'Haiku',
      'Fable',
      'Another model…',
    ]);
    await set('model', 'sonnet');
    expect(api.patchSession).toHaveBeenLastCalledWith('s1', { model: 'sonnet' });
    expect(app.sessions.get('s1')!.model).toBe('sonnet');
    expect(field('model')!.value).toBe('sonnet');

    await set('model', 'custom');
    expect(api.patchSession).toHaveBeenCalledTimes(1);
    await set('model_name', ' claude-opus-5-5 ');
    expect(api.patchSession).toHaveBeenLastCalledWith('s1', { model: 'claude-opus-5-5' });

    await set('model', '');
    expect(api.patchSession).toHaveBeenLastCalledWith('s1', { model: null });
    expect(field('model_name')).toBeNull();
  });

  it('the header names a model switched to since the last turn at once', async () => {
    const { app } = setup('running', { model: 'fable' });
    await mount(app);
    expect($('.chat-model')!.textContent).toBe('Fable');
    expect(shownModel('opus', 'claude-opus-5-5')).toBe('claude-opus-5-5');
    expect(shownModel('claude-fable-5-1', 'claude-fable-5-1')).toBe('claude-fable-5-1');
    expect(shownModel(undefined, 'claude-opus-5-5')).toBe('claude-opus-5-5');
    expect(shownModel('claude-fable-5-1', 'claude-opus-5-5')).toBe('claude-fable-5-1');
    expect(shownModel('sonnet', undefined)).toBe('sonnet');
    expect(modelName('sonnet')).toBe('Sonnet');
    expect(modelName('qwen3.8-27b:q4_K')).toBe('qwen3.8-27b:q4_K');
  });

  it('shows the usage limits of the session\'s worker under the model', async () => {
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
    const rows = [...root.querySelectorAll('.menu-settings .limit-row')].map((r) => [
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
  });

  it('shows no limits while the worker has reported none', async () => {
    const { app, api } = setup('idle');
    (api as any).workers = vi.fn(async () => [{ id: 'wk1', name: 'drzlo-mac' }]);
    await mount(app);
    await open();
    await act(async () => new Promise((r) => setTimeout(r, 10)));
    expect(root.querySelector('.limits')).toBeNull();
  });

  it('a name that is no alias shows as typed', async () => {
    const { app } = setup('idle', { model: 'claude-sonnet-5-5' });
    await mount(app);
    await open();
    expect(field('model')!.value).toBe('custom');
    expect(field<HTMLInputElement>('model_name')!.value).toBe('claude-sonnet-5-5');
  });

  it('shows what the hub refused', async () => {
    const { app, api } = setup('idle');
    api.patchSession.mockRejectedValueOnce(new Error('model must be 1..64 characters of A-Z a-z 0-9 . _ : -'));
    await mount(app);
    await open();
    await set('model', 'custom');
    await set('model_name', 'a b');
    expect(api.patchSession).toHaveBeenLastCalledWith('s1', { model: 'a b' });
    expect($('.notice')!.textContent).toContain('Not changed: model must be');
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
    expect(button('Stop')).toBeUndefined();
    render(null, root);

    ({ app } = setup('idle', { subagents: 0 }));
    await mount(app);
    expect($('.pill')!.textContent).toBe('Idle');
    expect($('.pill .pulse')).toBeNull();
  });

  it('says Running while a background task runs after the turn ended, Idle once it ends', async () => {
    const { app } = setup('idle', { background_tasks: 1 });
    await mount(app);
    expect($('.pill')!.textContent).toBe('Running');
    expect($('.chat-agents')).toBeNull();
    const s = app.sessions.get('s1')!;
    await act(async () =>
      app.mergeSummary([
        { session_id: 's1', project_id: 'p1', title: s.title, state: 'idle', queue_len: 0, unread: 0, last_seq: 7, last_read_seq: 7 },
      ]),
    );
    expect($('.pill')!.textContent).toBe('Idle');
  });

  it('shows the state, worker, path, model and cost of the session, "Stop" and "More"', async () => {
    const { app } = setup('waiting');
    await mount(app);
    expect($('.chat-title')!.textContent).toBe('test 12');
    expect($('.pill')!.textContent).toBe('Waiting for permission');
    expect($('.pill .pulse')).toBeNull();
    const meta = $('.chat-meta')!.textContent!;
    expect(meta).toContain('drzlo-mac');
    expect($('.chat-path .mono')!.textContent).toBe('~/work/hqc');
    expect($('.chat-path button')!.getAttribute('aria-label')).toBe('Copy path');
    // The model of the last usage that names one, the sum of cost_usd.
    expect($('.chat-model')!.textContent).toBe('Opus 5.5');
    expect($('.chat-cost')!.textContent).toBe('$0.07 this session');
    expect(button('Stop')).toBeDefined();
    await act(async () => $('.btn-more')!.click());
    // T50: archiving is on the list's rows, not in the header.
    expect([...root.querySelectorAll('.menu-item')].map((b) => b.textContent)).toEqual([
      'Schedules',
      'Move to project…',
      'Switch to Auto',
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
    expect([...root.querySelectorAll('.menu-item')].map((b) => b.textContent)).toEqual(['Schedules']);
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
});

// The subagents running now, one line each, pinned above the input (SPEC §6, since 0.101).
describe('live subagents', () => {
  const live = (patch: (e: any) => void = () => {}) =>
    ev('event.subagents.live', 0, (e) => {
      delete e.seq;
      patch(e);
    });
  const rows = () =>
    [...root.querySelectorAll('.live-agents-row')].map((r) => [
      r.querySelector('.live-agents-what')!.textContent,
      r.querySelector('.live-agents-stats')!.textContent,
    ]);

  it('shows a row per subagent above the input, outside the feed, none with an empty list', async () => {
    const { app } = setup('running', { subagents: 2 });
    await mount(app);
    expect($('.live-agents')).toBeNull();
    await act(async () => subscriber!.onEvent(live()));
    expect(rows()).toEqual([
      [
        'Fix schematic power nets in import: Running Verify the final sample runs are byte-identical',
        '155.3k tokens · 188 tools · 4m 13s',
      ],
      ['Reply with number', '1s'],
    ]);
    // Pinned: outside the feed's scroller, right above the input.
    expect($('.timeline .live-agents')).toBeNull();
    expect($('.live-agents + .composer-wrap')).not.toBeNull();
    await act(async () => subscriber!.onEvent(live((e) => (e.payload.agents = []))));
    expect($('.live-agents')).toBeNull();
  });
});

describe('the request card through the chat', () => {
  it('folds the tool call into the card, explains, and takes Enter', async () => {
    const { api, app } = setup('waiting');
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

  it('on a phone too the machine is named once, in the pill: the line beside it has only the folder', async () => {
    const { app } = setup('locked_vscode', { held_by: { entrypoint: 'claude-vscode', status: 'idle' }, history: HISTORY.slice(0, 3) });
    await mount(app, true);
    expect($('.pill')!.textContent).toBe('In VS Code on drzlo-mac');
    expect($('.chat-meta')!.textContent!.split('drzlo-mac')).toHaveLength(2);
    expect($('.chat-where')!.textContent).not.toContain('drzlo-mac');
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
    // "More" on a phone also has the path, the model and the cost.
    await act(async () => $('.btn-more')!.click());
    expect($('.menu-info')!.textContent).toContain('~/work/hqc');
    expect($('.menu-info')!.textContent).toContain('Opus 5.5');
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
  afterEach(() => vi.restoreAllMocks());

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

  it('one that came during an earlier text stays above it', async () => {
    const answer = ev('event.agent.message', 3, (e) => (e.payload = { text: 'the earlier answer', final: true }));
    const idle = ev('event.status', 4, (e) => (e.payload = { state: 'idle' }));
    await writing([first, meanwhile(2), answer, idle]);
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
  const select = (node: Node | null) =>
    act(async () => {
      const sel = document.getSelection()!;
      sel.removeAllRanges();
      if (node) {
        const r = document.createRange();
        r.selectNodeContents(node);
        sel.addRange(r);
      }
      document.dispatchEvent(new Event('selectionchange'));
    });

  /** A right click on `node` (a text node's element): whether the browser's menu was kept. */
  const rightClick = async (node: Node) => {
    let kept = true;
    await act(() => {
      kept = (node.parentElement ?? (node as Element)).dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 300, clientY: 200 }));
    });
    return kept;
  };

  it('on a computer quotes the selected text from the client\'s menu on a right click, after the draft, and focuses the field', async () => {
    const { app } = setup('idle');
    await mount(app);
    await act(() => {
      area().value = 'Look at this';
      area().dispatchEvent(new Event('input', { bubbles: true }));
    });
    await select(textNode('All tests pass.'));
    // No floating button on a computer: the menu has it.
    expect($('.btn-quote')).toBeNull();
    expect(await rightClick(textNode('All tests pass.')), 'the browser\'s menu is not shown').toBe(false);
    expect([...document.querySelectorAll('.ctx-menu [role="menuitem"]')].map((b) => b.textContent!.trim())).toEqual(['Quote', 'Copy']);
    // Its own class: the floating button's style (fixed, shifted) is not the item's.
    expect($('.ctx-menu .btn-quote')).toBeNull();
    await act(() => $('.ctx-menu .menu-quote')!.click());
    expect(area().value).toBe('Look at this\n\n> All tests pass.\n\n');
    expect(document.activeElement).toBe(area());
    expect(area().selectionStart).toBe(area().value.length);
    expect(document.getSelection()!.rangeCount).toBe(0);
    expect($('.ctx-menu')).toBeNull();
    // The next quote goes after the first.
    await select(textNode('All tests pass.'));
    await rightClick(textNode('All tests pass.'));
    await act(() => $('.ctx-menu .menu-quote')!.click());
    expect(area().value).toBe('Look at this\n\n> All tests pass.\n\n> All tests pass.\n\n');
  });

  it('copies the selected text from the same menu', async () => {
    const writeText = vi.fn(async (_: string) => {});
    vi.stubGlobal('isSecureContext', true);
    Object.defineProperty(window.navigator, 'clipboard', { value: { writeText }, configurable: true });
    const { app } = setup('idle');
    await mount(app);
    await select(textNode('All tests pass.'));
    await rightClick(textNode('All tests pass.'));
    await act(async () => $('.ctx-menu .btn-copy-selection')!.click());
    expect(writeText).toHaveBeenCalledWith('All tests pass.');
    vi.unstubAllGlobals();
  });

  it('leaves the browser\'s menu to a right click without a selection, outside the feed, or without a composer', async () => {
    const { app } = setup('idle');
    await mount(app);
    expect(await rightClick(textNode('All tests pass.')), 'nothing selected').toBe(true);
    await select(textNode('test 12', '.chat-head'));
    expect(await rightClick(textNode('test 12', '.chat-head'))).toBe(true);
    expect($('.ctx-menu')).toBeNull();
    render(null, root);

    const locked = setup('locked_vscode');
    await mount(locked.app);
    await select(textNode('All tests pass.'));
    expect(await rightClick(textNode('All tests pass.'))).toBe(true);
    expect($('.ctx-menu')).toBeNull();
  });

  it('on a phone offers the button only for text in the feed, while selected, and keeps the browser\'s menu', async () => {
    const { app } = setup('idle', { history: HISTORY.slice(0, 3) });
    await mount(app, true);
    await select(textNode('test 12', '.chat-head'));
    expect($('.btn-quote')).toBeNull();
    await select(textNode('All tests pass.'));
    expect($('.btn-quote')!.textContent).toBe('Quote');
    expect(await rightClick(textNode('All tests pass.')), 'no client menu on a phone').toBe(true);
    expect($('.ctx-menu')).toBeNull();
    await select(null);
    expect($('.btn-quote'), 'gone with the selection').toBeNull();
  });

  it('on a phone a tap that clears the selection still quotes it', async () => {
    // Without the open request of HISTORY: on a phone it would take the composer's place.
    const { app } = setup('idle', { history: HISTORY.slice(0, 3) });
    await mount(app, true);
    await select(textNode('All tests pass.'));
    const btn = $('.btn-quote')!;
    await act(() => void btn.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, cancelable: true })));
    await select(null);
    expect($('.btn-quote')).toBe(btn);
    await act(() => btn.click());
    expect(area().value).toBe('> All tests pass.\n\n');
    expect($('.btn-quote')).toBeNull();
  });

  it('quotes every line, a blank one as a bare «>», after a blank line', () => {
    expect(withQuote('', 'one')).toBe('> one\n\n');
    expect(withQuote('  \n', 'one')).toBe('> one\n\n');
    expect(withQuote('draft\n\n', 'one\n\n  two\r\nthree')).toBe('draft\n\n> one\n>\n>   two\n> three\n\n');
  });
});
