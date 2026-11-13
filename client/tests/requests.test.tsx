// @vitest-environment happy-dom
// T37: requests where the user is looking — another session's request as a card in the
// session waiting on it (under "Waiting for a reply"), the "Requests" list of every open
// request, answering from both into the request's own session, and the card leaving as
// soon as the request is answered anywhere else.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { ApiError, type Api } from '../src/api';
import { AppState } from '../src/app';
import { parseEvent, parseRequestList, parseServerFrame, type OpenRequest, type SessionInfo } from '../src/proto';
import { HubSocket } from '../src/ws';
import { Chat } from '../src/ui/Chat';
import { PhoneHome } from '../src/ui/PhoneHome';
import { RequestsScreen } from '../src/ui/requests';
import { SessionList } from '../src/ui/SessionList';

const WIRE = join(import.meta.dirname, '../../crates/hqc-proto/examples/wire');
const load = (f: string): unknown => JSON.parse(readFileSync(join(WIRE, f), 'utf8'));

// From api.requests.json: B's approval that A (…01) and the planner (…02) wait on, and a
// worker's own request nobody waits on.
const A = '0199f3a2-7c10-7000-8000-000000000001';
const B = '0199f3a2-7c10-7000-8000-000000000003';
const SYS = '0199f3a2-7c10-7000-8000-0000000000f1';
const REQUESTS: OpenRequest[] = parseRequestList(load('api.requests.json')).map((r) => {
  // Still open whenever the test runs.
  if (r.event.type === 'approval.request') r.event.payload.expires_at = '2099-01-01T00:00:00Z';
  return r;
});

let root: HTMLElement;
afterEach(() => {
  if (root) render(null, root);
  document.body.innerHTML = '';
});

function info(id: string, title: string, more: Partial<SessionInfo> = {}): SessionInfo {
  return {
    id,
    project_id: 'p1',
    workspace_id: 'w1',
    worker_id: 'wk1',
    cwd: '.',
    title,
    slug: id,
    state: 'running',
    queue_len: 0,
    unread: 0,
    last_seq: 2,
    last_read_seq: 2,
    archived: false,
    updated_at: '2026-10-25T10:00:00Z',
    system: false,
    ...more,
  };
}

// A's feed: its turn called hqc_ask a minute ago and waits for the reply.
const HISTORY = [
  parseEvent({ ...(load('event.user.message.json') as object), session_id: A, seq: 1 }),
  parseEvent({
    ...(load('event.tool.call.json') as object),
    session_id: A,
    seq: 2,
    ts: new Date(Date.now() - 60_000).toISOString(),
    payload: { call_id: 'ask-1', tool: 'mcp__hqc__hqc_ask', args_preview: '{"session":"hqc/helper","text":"build?"}' },
  }),
];

function setup(requests: OpenRequest[] = REQUESTS) {
  const api = {
    session: vi.fn(async () => info(A, 'hqc: run the tests')),
    events: vi.fn(async (_id: string, after: number) => {
      const events = HISTORY.filter((e) => e.seq! > after);
      return { events, invalid: [], maxSeq: Math.max(after, ...events.map((e) => e.seq!)), count: events.length };
    }),
    eventsBefore: vi.fn(async (_id: string, before = Number.MAX_SAFE_INTEGER, limit = 200) => {
      const found = HISTORY.filter((e) => e.seq! < before).slice(-limit);
      const seqs = found.map((e) => e.seq!);
      return { events: found, invalid: [], maxSeq: Math.max(0, ...seqs), minSeq: seqs[0], count: found.length };
    }),
    readCursor: vi.fn(async () => {}),
    approve: vi.fn(async () => {}),
    clarify: vi.fn(async () => {}),
    workspaces: vi.fn(async () => []),
  };
  const socket = { onState: () => () => {}, subscribe: () => () => {} } as unknown as HubSocket;
  const app = new AppState(api as unknown as Api, socket);
  app.conn = 'open';
  app.sessions.set(A, info(A, 'hqc: run the tests'));
  app.sessions.set(B, info(B, 'Check the build on llm-server', { worker_id: 'wk2', state: 'waiting' }));
  app.setRequests(requests);
  return { api, app };
}

async function mount(node: preact.ComponentChild) {
  root = document.createElement('div');
  document.body.appendChild(root);
  await act(async () => {
    render(node, root);
    await new Promise((r) => setTimeout(r, 50));
  });
}

const $ = (sel: string) => root.querySelector<HTMLElement>(sel);
const $$ = (sel: string) => [...root.querySelectorAll<HTMLElement>(sel)];
const button = (scope: HTMLElement, text: string) =>
  [...scope.querySelectorAll<HTMLButtonElement>('button')].find((b) => b.textContent?.trim().startsWith(text));

describe('the request of a waited-on session in the asking session', () => {
  it('shows under "Waiting for a reply" as B\'s card, named and set apart from A\'s own', async () => {
    const { app } = setup();
    await mount(<Chat app={app} sessionId={A} />);
    expect($('.activity-ask')!.textContent).toContain('Waiting for a reply');
    const card = $('.waited-requests .foreign-request')!;
    expect(card).not.toBeNull();
    // Under the activity line, inside the feed.
    expect($('.activity-ask')!.compareDocumentPosition(card) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    // It names B, its address and its worker, and links to B.
    const head = card.querySelector('.req-head')!.textContent!;
    expect(head).toContain('Check the build on llm-server');
    expect(head).toContain('hqc/helper');
    expect(head).toContain('on llm-server');
    expect(card.querySelector<HTMLAnchorElement>('.req-session')!.getAttribute('href')).toBe(`#/s/${B}`);
    // The other waiting session is named; A itself is not.
    expect(card.querySelector('.req-waiting')!.textContent).toBe('Also waiting: Plan the release');
    // B's own card, inside the frame; the worker's request nobody waits on is not here.
    expect(card.querySelector('.perm .perm-what')!.textContent).toBe('$ cargo build --release');
    expect($$('.foreign-request')).toHaveLength(1);
    // The keys never answer another session's request, so the card shows no key hints.
    expect(card.querySelector('.kbd')).toBeNull();
    expect(card.getAttribute('aria-label')).toBe('Request of Check the build on llm-server');
  });

  it('answers into B, and disappears once answered', async () => {
    const { api, app } = setup();
    await mount(<Chat app={app} sessionId={A} />);
    const card = $('.foreign-request')!;
    // Enter does not answer another session's request.
    await act(async () => void document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })));
    expect(api.approve).not.toHaveBeenCalled();
    await act(async () => {
      button(card, 'Allow')!.click();
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(api.approve.mock.calls[0]).toEqual([B, 'req-7', 'allow', undefined]);
    expect($('.foreign-request')).toBeNull();
    expect(app.requests.map((r) => r.request_id)).toEqual(['ws-1']);
  });

  it('a text reply goes with B\'s deny option', async () => {
    const { api, app } = setup();
    await mount(<Chat app={app} sessionId={A} />);
    await act(async () => button($('.foreign-request')!, 'Reply with text')!.click());
    const field = $('.foreign-request textarea') as HTMLTextAreaElement;
    await act(async () => {
      field.value = 'Not now';
      field.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await act(async () => {
      button($('.foreign-request')!, 'Deny and send')!.click();
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(api.approve.mock.calls[0]).toEqual([B, 'req-7', 'deny', 'Not now']);
  });

  it('disappears when answered elsewhere: the hub\'s next list leaves it out', async () => {
    const { app } = setup();
    await mount(<Chat app={app} sessionId={A} />);
    expect($('.foreign-request')).not.toBeNull();
    await act(async () => app.setRequests(REQUESTS.filter((r) => r.request_id !== 'req-7')));
    expect($('.foreign-request')).toBeNull();
  });

  it('an answer that comes second (409 request_closed) closes it as well', async () => {
    const { api, app } = setup();
    api.approve.mockRejectedValueOnce(new ApiError(409, 'the request is already closed', 'request_closed'));
    await mount(<Chat app={app} sessionId={A} />);
    await act(async () => {
      button($('.foreign-request')!, 'Allow')!.click();
      await new Promise((r) => setTimeout(r, 0));
    });
    expect($('.foreign-request')).toBeNull();
  });

  it('a clarify of the waited-on session is answered the same way', async () => {
    const clarify = parseRequestList({
      requests: [
        {
          ...(load('api.requests.json') as { requests: object[] }).requests[0],
          request_id: 'req-2',
          kind: 'clarify',
          event: { ...(load('event.clarify.request.json') as object), session_id: B },
        },
      ],
    });
    const { api, app } = setup(clarify);
    await mount(<Chat app={app} sessionId={A} />);
    const card = $('.foreign-request')!;
    expect(card.querySelector('.card-clarify')).not.toBeNull();
    // Answer waits for every question: an option, a checked label, a typed text.
    await act(async () => card.querySelector<HTMLInputElement>('input[type=radio]')!.click());
    await act(async () => card.querySelector<HTMLInputElement>('input[type=checkbox]')!.click());
    await act(async () => {
      const texts = card.querySelectorAll<HTMLInputElement>('input[type=text]');
      const text = texts[texts.length - 1];
      text.value = 'no';
      text.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await act(async () => {
      button(card, 'Answer')!.click();
      await new Promise((r) => setTimeout(r, 0));
    });
    const [sid, rid] = api.clarify.mock.calls[0] as unknown as [string, string];
    expect([sid, rid]).toEqual([B, 'req-2']);
    expect($('.foreign-request')).toBeNull();
  });
});

describe('the "Requests" list', () => {
  it('lists every open request, the worker\'s too, each with its session', async () => {
    const { app } = setup();
    await mount(<RequestsScreen app={app} />);
    expect($('.chat-meta')!.textContent).toBe('2 open');
    const items = $$('.foreign-request');
    expect(items.map((x) => x.dataset.request)).toEqual(['req-7', 'ws-1']);
    expect(items[0].querySelector('.req-waiting')!.textContent).toBe('Waiting on it: Plan the release, hqc: run the tests');
    expect(items[1].querySelector('.req-head')!.textContent).toContain('Worker');
    expect(items[1].querySelector<HTMLAnchorElement>('.req-session')!.getAttribute('href')).toBe(`#/s/${SYS}`);
    expect(items[1].querySelector('.perm-title')!.textContent).toBe('The worker asks for permission');
  });

  it('answers in place into the request\'s session; empty when nothing is open', async () => {
    const { api, app } = setup();
    await mount(<RequestsScreen app={app} />);
    const worker = $$('.foreign-request')[1];
    await act(async () => {
      button(worker, 'Allow')!.click();
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(api.approve.mock.calls[0]).toEqual([SYS, 'ws-1', 'once', undefined]);
    expect($$('.foreign-request').map((x) => x.dataset.request)).toEqual(['req-7']);
    await act(async () => app.setRequests([]));
    expect($('.empty')!.textContent).toBe('No open requests');
  });

  it("has an entry with the count in the left column, a button in the header of a phone's first screen", async () => {
    const { app } = setup();
    app.me = { id: 'u1', email: 'me@example.com', display_name: '', personal_project_id: 'p1' } as never;
    app.currentProjectId = 'p1';
    await mount(<SessionList app={app} />);
    const link = $('.requests-link')!;
    expect(link.getAttribute('href')).toBe('#/requests');
    expect(link.querySelector('.badge')!.textContent).toBe('2');
    await act(async () => app.setRequests([]));
    expect($('.requests-link')).toBeNull();
    await act(async () => app.setRequests(REQUESTS));
    render(null, root);
    await mount(<PhoneHome app={app} />);
    expect($('.side-head .requests-link-phone')!.getAttribute('aria-label')).toBe('Requests: 2');
    // Not on a project's screen.
    render(null, root);
    await mount(<SessionList app={app} phone />);
    expect($('.requests-link')).toBeNull();
  });
});

describe('the requests frame', () => {
  it('comes with the summary subscription and replaces the list whole', async () => {
    const sockets: FakeWs[] = [];
    class FakeWs {
      readyState = 0;
      sent: string[] = [];
      onopen?: () => void;
      onmessage?: (m: { data: string }) => void;
      onclose?: () => void;
      onerror?: () => void;
      constructor() {
        sockets.push(this);
      }
      send(s: string) {
        this.sent.push(s);
      }
      close() {}
    }
    const socket = new HubSocket({ url: 'ws://x/api/ws', WebSocketImpl: FakeWs as never });
    const got: OpenRequest[][] = [];
    socket.onRequests((r) => got.push(r));
    socket.onSummary(() => {});
    socket.start();
    const ws = sockets[0];
    ws.readyState = 1;
    ws.onopen!();
    expect(ws.sent.map((s) => JSON.parse(s).type)).toEqual(['subscribe_summary']);
    ws.onmessage!({ data: readFileSync(join(WIRE, 'client.requests.json'), 'utf8') });
    ws.onmessage!({ data: JSON.stringify({ type: 'requests', requests: [] }) });
    expect(got.map((l) => l.map((r) => r.request_id))).toEqual([['req-7'], []]);
    const f = parseServerFrame(load('client.requests.json'));
    expect(f.type === 'requests' && f.requests[0].waiting.map((w) => w.session_id)).toEqual([A]);
    socket.stop();
  });
});
