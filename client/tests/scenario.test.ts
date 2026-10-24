// Acceptance scenario on the mock hub (T5): open a session, write, see deltas and tool
// cards, get approval.request, press a button, see approval.response and the final answer.
// Runs the client's own transport and state code against mock/server.mjs over real HTTP
// and WebSocket, and renders the chat to check what the user would see.

import { afterAll, beforeAll, describe, expect, it } from 'vitest';
// @ts-expect-error plain JS module without types
import { startMock } from '../mock/server.mjs';
import { Api } from '../src/api';
import { ChatSession } from '../src/session';
import { HubSocket } from '../src/ws';
import type { EventType, HqcEvent } from '../src/proto';

let mock: { url: string; dropClients(): void; close(): Promise<void> };
let api: Api;
let socket: HubSocket;

beforeAll(async () => {
  mock = await startMock({ speed: 0.02 });
  api = new Api(mock.url);
  socket = new HubSocket({ url: mock.url.replace('http', 'ws') + '/api/ws', minDelayMs: 20 });
  socket.start();
});

afterAll(async () => {
  socket.stop();
  await mock.close();
});

function waitFor<T>(chat: ChatSession, pick: () => T | undefined, ms = 5000): Promise<T> {
  return new Promise((resolve, reject) => {
    const check = () => {
      const v = pick();
      if (v !== undefined && v !== false) {
        off();
        clearTimeout(timer);
        resolve(v);
      }
    };
    const off = chat.timeline.onChange(check);
    const timer = setTimeout(() => {
      off();
      reject(new Error('timeout; have: ' + chat.timeline.events().map((e) => e.type).join(',')));
    }, ms);
    check();
  });
}

const find = (chat: ChatSession, type: EventType, after = 0): HqcEvent | undefined =>
  chat.timeline.events().find((e) => e.type === type && e.seq! > after);

describe('mock scenario', () => {
  it('runs a turn with an approval from open to final answer', async () => {
    const me = await api.me();
    const list = await api.sessions(me.personal_project_id);
    const target = list.find((s) => s.title === 'Пустая сессия')!;
    expect(target.state).toBe('idle');

    const chat = new ChatSession(api, socket, target.id);
    await chat.open();
    await waitFor(chat, () => find(chat, 'system'));
    const start = chat.timeline.lastSeq;

    // Write: shown at once, then reconciled with the hub echo by client_op_id.
    const deltas: string[] = [];
    chat.timeline.onChange(() => {
      if (chat.timeline.streaming) deltas.push(chat.timeline.streaming);
    });
    const op = chat.send('Запусти тесты');
    expect(chat.timeline.items().some((i) => i.kind === 'pending' && i.pending.client_op_id === op)).toBe(true);
    const echo = await waitFor(chat, () => find(chat, 'user.message', start));
    expect(echo.client_op_id).toBe(op);
    expect(chat.timeline.items().some((i) => i.kind === 'pending')).toBe(false);

    // Deltas, then tool cards, then the approval request.
    const req = await waitFor(chat, () => find(chat, 'approval.request', start));
    expect(deltas.length).toBeGreaterThan(1);
    expect(find(chat, 'tool.call', start)).toBeDefined();
    expect(find(chat, 'tool.result', start)).toBeDefined();
    if (req.type !== 'approval.request') throw new Error('unreachable');
    expect(req.payload.options.map((o) => o.canonical)).toEqual(['once', 'session', 'always', 'deny', 'deny_always']);

    // Press "allow once" (the same call the button makes) and see the response and answer.
    const once = req.payload.options.find((o) => o.canonical === 'once')!;
    await chat.approve(req.payload.request_id, once.id);
    const resp = await waitFor(chat, () => find(chat, 'approval.response', start));
    expect(resp.type === 'approval.response' && resp.payload.option_id).toBe(once.id);
    expect(chat.timeline.requestFate(req.payload.request_id).response).toBeDefined();
    // First answer wins: a second one is refused and writes nothing (SPEC §10.1).
    await expect(chat.approve(req.payload.request_id, once.id)).rejects.toMatchObject({
      status: 409,
      code: 'request_closed',
    });

    const final = await waitFor(chat, () =>
      chat.timeline.events().find((e) => e.type === 'agent.message' && e.payload.final && e.seq! > start),
    );
    expect(final.type === 'agent.message' && final.payload.text).toMatch(/42/);
    await waitFor(chat, () =>
      chat.timeline.events().find((e) => e.type === 'status' && e.payload.state === 'idle' && e.seq! > start),
    );
    expect(chat.timeline.streaming).toBe('');
    expect(chat.timeline.events().find((e) => e.event_id === echo.event_id)?.delivery).toBe('delivered');

    // What the user sees: render the timeline and look for the cards.
    const { renderChatToElement } = await import('./render-helpers');
    const el = renderChatToElement(chat);
    // The Read call in a group captioned by the interim message; the Bash call is the
    // approval card, folded after the answer.
    const group = el.querySelector('.tool-group')!;
    expect(group.querySelector('.tool-caption')!.textContent).toBe('Сейчас посмотрю на проект и запущу тесты.');
    expect(group.querySelector('.tool-stats')!.textContent).toMatch(/^· 1 команда/);
    expect(el.querySelector('.perm-done-row')!.textContent).toContain('$ cargo test');
    expect(el.querySelector('.perm-done-row')!.textContent).toContain('· разрешено');
    expect(el.textContent).toContain('Все 42 теста прошли.');
    expect(el.querySelectorAll('.perm button[data-option]').length).toBe(0);
    chat.close();
  });

  it('answers a request with text instead: the denial carries it, the agent reads it (T26)', async () => {
    const me = await api.me();
    const s = await api.createSession(me.personal_project_id, 'text answer');
    const chat = new ChatSession(api, socket, s.id);
    await chat.open();
    await waitFor(chat, () => find(chat, 'system'));
    const start = chat.timeline.lastSeq;
    chat.send('Запусти тесты');
    const req = await waitFor(chat, () => find(chat, 'approval.request', start));
    if (req.type !== 'approval.request') throw new Error('unreachable');
    const deny = req.payload.options.find((o) => o.canonical === 'deny')!;
    const once = req.payload.options.find((o) => o.canonical === 'once')!;
    // Text only goes with a deny option.
    await expect(chat.approve(req.payload.request_id, once.id, 'нет')).rejects.toMatchObject({ status: 400 });
    await chat.approve(req.payload.request_id, deny.id, 'Сначала собери проект');
    const resp = await waitFor(chat, () => find(chat, 'approval.response', start));
    expect(resp.type === 'approval.response' && resp.payload).toMatchObject({ option_id: deny.id, message: 'Сначала собери проект' });
    const final = await waitFor(chat, () =>
      chat.timeline.events().find((e) => e.type === 'agent.message' && e.payload.final && e.seq! > start),
    );
    expect(final.type === 'agent.message' && final.payload.text).toContain('Сначала собери проект');

    const { renderChatToElement } = await import('./render-helpers');
    const el = renderChatToElement(chat);
    expect(el.querySelector('.msg-answer .msg-bubble')!.textContent).toBe('Сначала собери проект');
    expect(el.querySelector('.perm-done-row')!.textContent).toContain('отклонено, ответ текстом');
    chat.close();
  });

  it('resubscribes from the last seq after the socket drops', async () => {
    const me = await api.me();
    const s = await api.createSession(me.personal_project_id, 'reconnect');
    const chat = new ChatSession(api, socket, s.id);
    await chat.open();
    await waitFor(chat, () => find(chat, 'system'));
    const before = chat.timeline.events().length;

    mock.dropClients();
    await api.input(s.id, 'пока сокет лежит', 'op-offline');
    await new Promise<void>((r) => {
      const off = socket.onState((st) => st === 'open' && (off(), r()));
    });
    const echo = await waitFor(chat, () =>
      chat.timeline.events().find((e) => e.type === 'user.message' && e.client_op_id === 'op-offline'),
    );
    expect(echo).toBeDefined();
    const seqs = chat.timeline.events().map((e) => e.seq);
    expect(new Set(seqs).size).toBe(seqs.length);
    expect(chat.timeline.events().length).toBeGreaterThan(before);
    await chat.stop();
    chat.close();
  });

  it('resends a message with the same client_op_id and gets one echo', async () => {
    const me = await api.me();
    const s = await api.createSession(me.personal_project_id, 'resend');
    const chat = new ChatSession(api, socket, s.id);
    await chat.open();
    // The first POST was stored, but the client saw it fail (reply lost).
    await api.input(s.id, 'один раз', 'op-dup');
    chat.timeline.addPending('op-dup', 'один раз');
    chat.timeline.failPending('op-dup', 'network');
    chat.resend('op-dup');
    await waitFor(chat, () => find(chat, 'approval.request'));
    const echoes = chat.timeline.events().filter((e) => e.type === 'user.message');
    expect(echoes).toHaveLength(1);
    expect(chat.timeline.items().some((i) => i.kind === 'pending')).toBe(false);
    await chat.stop();
    chat.close();
  });

  it('cancels an open approval on stop and removes the buttons', async () => {
    const me = await api.me();
    const s = await api.createSession(me.personal_project_id, 'stop');
    const chat = new ChatSession(api, socket, s.id);
    await chat.open();
    chat.send('запусти');
    const req = await waitFor(chat, () => find(chat, 'approval.request'));
    await chat.stop();
    const cancel = await waitFor(chat, () => find(chat, 'approval.cancel'));
    expect(cancel.type === 'approval.cancel' && cancel.payload.reason).toBe('stopped');
    if (req.type !== 'approval.request') throw new Error('unreachable');
    expect(chat.timeline.requestFate(req.payload.request_id).cancel).toBeDefined();
    chat.close();
  });

  it('answers a clarify request', async () => {
    const me = await api.me();
    const s = await api.createSession(me.personal_project_id, 'clarify');
    const chat = new ChatSession(api, socket, s.id);
    await chat.open();
    chat.send('есть вопрос');
    const req = await waitFor(chat, () => find(chat, 'clarify.request'));
    if (req.type !== 'clarify.request') throw new Error('unreachable');
    await chat.answer(req.payload.request_id, { q1: 'Hub', q2: ['test'], q3: 'нет' });
    await waitFor(chat, () => find(chat, 'clarify.response'));
    const final = await waitFor(chat, () =>
      chat.timeline.events().find((e) => e.type === 'agent.message' && e.payload.final),
    );
    expect(final.type === 'agent.message' && final.payload.text).toContain('q1 = Hub');
    chat.close();
  });

  it('retries a failed delivery', async () => {
    const me = await api.me();
    const target = (await api.sessions(me.personal_project_id)).find((x) => x.state === 'error')!;
    const chat = new ChatSession(api, socket, target.id);
    await chat.open();
    expect(chat.timeline.hasFailedDelivery()).toBe(true);
    await chat.retry();
    await waitFor(chat, () => !chat.timeline.hasFailedDelivery() || undefined);
    await waitFor(chat, () => chat.timeline.events().find((e) => e.type === 'approval.request'));
    await chat.stop();
    chat.close();
  });
});
