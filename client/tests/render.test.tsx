// @vitest-environment happy-dom
// Rendering of chat cards from examples/wire (SPEC §4, §10.1).

import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { parseEvent, parseServerFrame, type HqcEvent } from '../src/proto';
import { Timeline } from '../src/timeline';
import { ActivityLine, BusyIndicator, ChatItems, onlyRequests, type ChatActions } from '../src/ui/cards';
import { CANONICAL_LABELS, POLICY_HINT_LABELS } from '../src/ui/labels';

const WIRE = join(import.meta.dirname, '../../crates/hqc-proto/examples/wire');
const raw = (name: string): any => JSON.parse(readFileSync(join(WIRE, `${name}.json`), 'utf8'));
const ev = (name: string, patch: (e: any) => void = () => {}): HqcEvent => {
  const e = raw(name);
  patch(e);
  return parseEvent(e);
};

let root: HTMLElement;
afterEach(() => {
  if (root) render(null, root);
  document.body.innerHTML = '';
});

function show(timeline: Timeline, over: Partial<ChatActions> = {}) {
  const actions: ChatActions = {
    timeline,
    approve: vi.fn(async () => {}),
    answer: vi.fn(async () => {}),
    resend: vi.fn(),
    discard: vi.fn(),
    openArtifact: vi.fn(),
    now: () => Date.parse('2026-10-22T10:30:00Z'),
    ...over,
  };
  root = document.createElement('div');
  document.body.appendChild(root);
  const draw = () => act(() => render(<ChatItems items={timeline.items()} actions={actions} />, root));
  draw();
  return { actions, draw };
}

const withSeq = (seq: number) => (e: any) => {
  e.seq = seq;
};

describe('every example event renders', () => {
  const names = readdirSync(WIRE)
    .filter((f) => f.startsWith('event.') && f !== 'event.agent.delta.json')
    .map((f) => f.replace(/\.json$/, ''));
  for (const [i, name] of names.entries()) {
    it(name, () => {
      const t = new Timeline();
      t.apply(ev(name, withSeq(i + 1)));
      show(t);
      // `status` is drawn only for an error with a reason (T13).
      expect(root.children.length).toBe(name === 'event.status' ? 0 : 1);
    });
  }

  it('agent.delta streams into a buffer and is not stored', () => {
    const t = new Timeline();
    t.apply(ev('event.agent.delta'));
    t.apply(ev('event.agent.delta', (e) => (e.payload.text = ' tests')));
    expect(t.streaming).toBe('Running tests');
    expect(t.items()).toEqual([]);
    t.apply(ev('event.agent.message'));
    expect(t.streaming).toBe('');
  });
});

describe('unknown and invalid events', () => {
  it('renders an unknown type as a collapsed card', () => {
    const t = new Timeline();
    t.apply(ev('event.unknown'));
    show(t);
    const card = root.querySelector('details.card-unknown')!;
    expect(card).not.toBeNull();
    expect(card.hasAttribute('open')).toBe(false);
    expect(card.querySelector('summary')!.textContent).toContain('Неизвестное событие');
    expect(card.querySelector('summary')!.textContent).toContain('plan.update');
  });

  it('renders a known type with a broken payload as an invalid card', () => {
    const t = new Timeline();
    const bad = raw('event.tool.call');
    delete bad.payload.tool;
    t.addInvalid(bad, 'tool: expected a string');
    show(t);
    expect(root.querySelector('.card-invalid summary')!.textContent).toContain('tool.call');
  });
});

describe('approval.request', () => {
  it('has exactly one button per option, captioned by canonical', () => {
    const t = new Timeline();
    t.apply(ev('event.approval.request'));
    show(t);
    const buttons = [...root.querySelectorAll('.card-approval button')];
    const options = raw('event.approval.request').payload.options;
    expect(buttons.map((b) => b.getAttribute('data-option'))).toEqual(options.map((o: any) => o.id));
    expect(buttons.map((b) => b.textContent)).toEqual(
      options.map((o: any) => CANONICAL_LABELS[o.canonical as keyof typeof CANONICAL_LABELS]),
    );
  });

  it('captions the card with policy_hint, and nothing without it', () => {
    const t = new Timeline();
    t.apply(ev('event.approval.request', withSeq(1)));
    t.apply(
      ev('event.approval.request', (e) => {
        e.seq = 2;
        e.event_id = '0199f3a2-7c10-7000-8000-0000000001a1';
        e.payload.request_id = 'req-2';
        e.payload.policy_hint = 'deny';
      }),
    );
    t.apply(
      ev('event.approval.request', (e) => {
        e.seq = 3;
        e.event_id = '0199f3a2-7c10-7000-8000-0000000001a2';
        e.payload.request_id = 'req-3';
        delete e.payload.policy_hint;
      }),
    );
    show(t);
    const titles = [...root.querySelectorAll('.card-approval .card-title')].map((c) => c.textContent);
    expect(titles).toEqual([
      `Разрешение · Bash · ${POLICY_HINT_LABELS.allow}`,
      `Разрешение · Bash · ${POLICY_HINT_LABELS.deny}`,
      'Разрешение · Bash',
    ]);
  });

  it("uses the agent's label for an unknown canonical and keeps the agent's option set", () => {
    const t = new Timeline();
    t.apply(
      ev('event.approval.request', (e) => {
        e.payload.options = [
          { id: 'proceed', label: 'Proceed with edits', canonical: 'allow_edits' },
          { id: 'reject', label: 'Reject', canonical: 'deny' },
        ];
      }),
    );
    show(t);
    const buttons = [...root.querySelectorAll('.card-approval button')].map((b) => b.textContent);
    expect(buttons).toEqual(['Proceed with edits', CANONICAL_LABELS.deny]);
  });

  it('sends the option id on click, then shows the answer instead of buttons', async () => {
    const t = new Timeline();
    t.apply(ev('event.approval.request', withSeq(1)));
    const { actions, draw } = show(t);
    const btn = root.querySelector<HTMLButtonElement>('button[data-option="allow_session"]')!;
    await act(async () => btn.click());
    expect(actions.approve).toHaveBeenCalledWith('req-1', 'allow_session');

    t.apply(ev('event.approval.response', (e) => ((e.seq = 2), (e.payload.option_id = 'allow_session'))));
    draw();
    expect(root.querySelectorAll('.card-approval button').length).toBe(0);
    expect(root.querySelector('.card-approval .outcome')!.textContent).toContain(CANONICAL_LABELS.session);
    expect(root.querySelector('.line-approval')!.textContent).toContain('Bash');
  });

  it('removes the buttons on approval.cancel', () => {
    const t = new Timeline();
    t.apply(ev('event.approval.request', withSeq(1)));
    t.apply(ev('event.approval.cancel', withSeq(2)));
    show(t);
    expect(root.querySelectorAll('.card-approval button').length).toBe(0);
    expect(root.querySelector('.card-approval .outcome')!.textContent).toContain('воркер перезапущен');
  });

  it('shows the error if the hub refuses the answer', async () => {
    const t = new Timeline();
    t.apply(ev('event.approval.request'));
    show(t, { approve: async () => Promise.reject(new Error('request is not open')) });
    await act(async () => root.querySelector<HTMLButtonElement>('.card-approval button')!.click());
    await act(async () => {});
    expect(root.querySelector('.card-approval .error-text')!.textContent).toContain('request is not open');
  });
});

describe('clarify.request', () => {
  it('renders questions with choices and submits answers', async () => {
    const t = new Timeline();
    t.apply(ev('event.clarify.request'));
    const { actions } = show(t);
    const qs = root.querySelectorAll('.card-clarify fieldset');
    expect(qs.length).toBe(3);
    expect(qs[0].querySelectorAll('input[type=radio]').length).toBe(2);
    expect(qs[1].querySelectorAll('input[type=checkbox]').length).toBe(3);
    expect(qs[2].querySelectorAll('input[type=text]').length).toBe(1);

    const click = (sel: string) => act(() => root.querySelector<HTMLInputElement>(sel)!.click());
    await click('input[value="Worker"]');
    await click('input[value="clippy"]');
    await click('input[value="test"]');
    const text = qs[2].querySelector<HTMLInputElement>('input[type=text]')!;
    await act(() => {
      text.value = 'нет';
      text.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await act(async () => root.querySelector<HTMLButtonElement>('.card-clarify button[type=submit]')!.click());
    expect(actions.answer).toHaveBeenCalledWith('req-2', { q1: 'Worker', q2: ['clippy', 'test'], q3: 'нет' });
  });

  it('closes after clarify.response', () => {
    const t = new Timeline();
    t.apply(ev('event.clarify.request', withSeq(1)));
    t.apply(ev('event.clarify.response', withSeq(2)));
    show(t);
    expect(root.querySelector('.card-clarify button')).toBeNull();
    expect(root.querySelector<HTMLInputElement>('input[value="Hub"]')!.checked).toBe(true);
  });
});

describe('tool cards', () => {
  it('folds the result into the call card, collapsed, with a preview', () => {
    const t = new Timeline();
    t.apply(ev('event.tool.call', withSeq(1)));
    show(t);
    let card = root.querySelector('details.card-tool')!;
    expect(card.classList.contains('tool-running')).toBe(true);

    t.apply(ev('event.tool.result', withSeq(2)));
    show(t);
    const cards = root.querySelectorAll('details.card-tool');
    expect(cards.length).toBe(1);
    card = cards[0];
    expect(card.hasAttribute('open')).toBe(false);
    expect(card.classList.contains('tool-error')).toBe(true);
    expect(card.querySelector('summary')!.textContent).toContain('Bash');
    expect(card.querySelector('summary')!.textContent).toContain('cargo test');
    expect(card.textContent).toContain('1 test failed');
  });

  it('shows a result without its call as its own card', () => {
    const t = new Timeline();
    t.apply(ev('event.tool.result'));
    show(t);
    expect(root.querySelectorAll('details.card-tool').length).toBe(1);
  });
});

describe('messages', () => {
  it('shows an optimistic message and replaces it with the echo', () => {
    const t = new Timeline();
    t.addPending('op-1', 'Run the tests');
    show(t);
    expect(root.querySelector('.bubble-pending')!.textContent).toContain('Run the tests');
    t.apply(ev('event.user.message'));
    show(t);
    expect(root.querySelector('.bubble-pending')).toBeNull();
    expect(root.querySelectorAll('.bubble-user').length).toBe(1);
  });

  it('offers resend for a message the hub did not accept', async () => {
    const t = new Timeline();
    t.addPending('op-9', 'hi');
    t.failPending('op-9', 'HTTP 502');
    const { actions } = show(t);
    expect(root.querySelector('.bubble-failed')!.textContent).toContain('HTTP 502');
    await act(async () => root.querySelector<HTMLButtonElement>('.bubble-failed button')!.click());
    expect(actions.resend).toHaveBeenCalledWith('op-9');
  });

  it('marks a failed delivery', () => {
    const t = new Timeline();
    t.apply(ev('event.user.message', (e) => (e.delivery = 'failed')));
    show(t);
    expect(root.querySelector('.delivery-failed')).not.toBeNull();
    expect(t.hasFailedDelivery()).toBe(true);
  });

  it('updates delivery from a delivery frame, not from the same event again', () => {
    const t = new Timeline();
    t.apply(ev('event.user.message', (e) => ((e.delivery = 'pending'), delete e.delivered_at)));
    // The same seq again (REST page and backlog overlap) changes nothing.
    t.apply(ev('event.user.message'));
    expect(t.events()).toHaveLength(1);
    expect(t.events()[0].delivery).toBe('pending');
    const d = parseServerFrame(JSON.parse(readFileSync(join(WIRE, 'client.delivery.json'), 'utf8')));
    if (d.type !== 'delivery') throw new Error('not a delivery frame');
    t.applyDelivery(d);
    expect(t.events()[0]).toMatchObject({ delivery: 'delivered', delivered_at: d.delivered_at });
    // A change for another event of the same seq is ignored.
    t.applyDelivery({ ...d, event_id: 'other', delivery: 'failed' });
    expect(t.events()[0].delivery).toBe('delivered');
  });

  it('shows the POST /input reply without moving lastSeq', () => {
    const t = new Timeline();
    t.apply(ev('event.system', withSeq(1)));
    t.addPending('op-1', 'Run the tests');
    t.applyReply(ev('event.user.message', withSeq(5)));
    expect(t.items().some((i) => i.kind === 'pending')).toBe(false);
    expect(t.lastSeq).toBe(1);
    t.apply(ev('event.user.message', withSeq(5)));
    expect(t.lastSeq).toBe(5);
    expect(t.events()).toHaveLength(2);
  });

  it('renders system as a line, status only for an error with a reason', () => {
    const t = new Timeline();
    const status = (seq: number, state: string, detail?: string) =>
      ev('event.status', (e) => {
        e.seq = seq;
        e.payload = detail === undefined ? { state } : { state, detail };
      });
    t.apply(status(1, 'waiting', 'approval requested'));
    t.apply(ev('event.system', withSeq(2)));
    t.apply(status(3, 'idle'));
    t.apply(status(4, 'running'));
    t.apply(status(5, 'error'));
    t.apply(status(6, 'error', 'claude exited with code 1'));
    show(t);
    const lines = [...root.querySelectorAll('.line')].map((l) => l.textContent);
    expect(lines).toEqual(['Session created', 'ошибка · claude exited with code 1']);
    expect(t.events()).toHaveLength(6);
  });

  it('marks a call denied by the worker policy', () => {
    const t = new Timeline();
    t.apply(
      ev('event.system', (e) => {
        e.seq = 1;
        e.payload = {
          code: 'policy_denied',
          text: "Denied by the worker's permission policy: Permission to use Bash with command git push has been denied.",
        };
      }),
    );
    show(t);
    const line = root.querySelector('.line-system-policy_denied')!;
    expect(line.textContent).toContain('Запрещено политикой воркера');
    expect(line.textContent).toContain('git push');
  });
});

describe('turn indicator (T13): from the session state, no events of its own', () => {
  const call = (seq: number, call_id: string, tool: string) =>
    ev('event.tool.call', (e) => {
      e.seq = seq;
      e.payload = { ...e.payload, call_id, tool };
    });
  const result = (seq: number, call_id: string) =>
    ev('event.tool.result', (e) => {
      e.seq = seq;
      e.payload = { ...e.payload, call_id };
    });

  function draw(state: any, timeline = new Timeline()) {
    root = document.createElement('div');
    document.body.appendChild(root);
    act(() =>
      render(
        <>
          <header>
            <BusyIndicator state={state} />
          </header>
          <ActivityLine state={state} timeline={timeline} />
        </>,
        root,
      ),
    );
    return {
      busy: root.querySelector('header .busy'),
      line: root.querySelector('.activity')?.textContent ?? null,
    };
  }

  it('running: spinner in the header and "агент работает"', () => {
    const r = draw('running');
    expect(r.busy).not.toBeNull();
    expect(r.line).toBe('агент работает');
  });

  it('running with the last tool.call unanswered: "выполняет <tool>"', () => {
    const t = new Timeline();
    t.apply(call(1, 'a', 'Read'));
    t.apply(result(2, 'a'));
    t.apply(call(3, 'b', 'Bash'));
    expect(draw('running', t).line).toBe('выполняет Bash');
    t.apply(result(4, 'b'));
    render(null, root);
    expect(draw('running', t).line).toBe('агент работает');
  });

  it('queued and waiting: their line, no spinner in the header', () => {
    for (const [state, text] of [
      ['queued', 'ввод в очереди'],
      ['waiting', 'ждёт подтверждения'],
    ]) {
      const t = new Timeline();
      t.apply(call(1, 'a', 'Bash'));
      const r = draw(state, t);
      expect(r.busy).toBeNull();
      expect(r.line).toBe(text);
      render(null, root);
    }
  });

  it('nothing in other states', () => {
    for (const state of ['idle', 'error', 'archived', 'worker_offline', 'locked_vscode', 'unknown']) {
      const r = draw(state);
      expect(r.busy).toBeNull();
      expect(r.line).toBeNull();
      render(null, root);
    }
  });
});

describe("worker's service session (T17, SPEC §10.2)", () => {
  const log = (seq: number, level: string, text: string) =>
    ev('event.system', (e) => {
      e.seq = seq;
      e.ts = '2026-10-22T10:00:07Z';
      e.payload = { text, code: 'worker_log', level };
    });
  const workerRequest = (seq: number) =>
    ev('event.approval.request', (e) => {
      const r = raw('worker.worker.approval.request').payload;
      e.seq = seq;
      e.author = { kind: 'system' };
      e.payload = {
        request_id: r.request_id,
        tool: r.kind,
        description: r.description,
        options: [
          { id: 'once', label: 'Allow', canonical: 'once' },
          { id: 'deny', label: 'Deny', canonical: 'deny' },
        ],
        expires_at: '2026-10-22T11:00:00Z',
      };
    });

  it('renders a log line with its time, level and text as is', () => {
    const t = new Timeline();
    t.apply(log(1, 'warn', 'hqc_worker::connection: hub connection lost: closed_by_hub *x*'));
    t.apply(log(2, 'error', 'outbox write failed'));
    show(t);
    const lines = [...root.querySelectorAll('.line-worker-log')];
    expect(lines).toHaveLength(2);
    expect(lines[0].classList.contains('level-warn')).toBe(true);
    expect(lines[0].querySelector('.log-level')!.textContent).toBe('предупреждение');
    expect(lines[0].querySelector('.log-time')!.getAttribute('datetime')).toBe('2026-10-22T10:00:07Z');
    expect(lines[0].querySelector('.log-time')!.textContent).toMatch(/^\d\d:\d\d:\d\d$/);
    // Not Markdown: underscores and stars stay.
    expect(lines[0].querySelector('.log-text')!.textContent).toBe(
      'hqc_worker::connection: hub connection lost: closed_by_hub *x*',
    );
    expect(lines[1].classList.contains('level-error')).toBe(true);
    expect(lines[1].querySelector('.log-level')!.textContent).toBe('ошибка');
  });

  it('shows the confirmation of a new folder with once / deny and sends the answer', async () => {
    const t = new Timeline();
    t.apply(workerRequest(1));
    const { actions } = show(t);
    expect(root.querySelector('.card-approval .card-title')!.textContent).toContain('сессии в новой папке');
    const buttons = [...root.querySelectorAll<HTMLButtonElement>('.card-approval button')];
    expect(buttons.map((b) => b.textContent)).toEqual([CANONICAL_LABELS.once, CANONICAL_LABELS.deny]);
    await act(async () => buttons[0].click());
    expect(actions.approve).toHaveBeenCalledWith(raw('worker.worker.approval.request').payload.request_id, 'once');
  });

  it('«только запросы» keeps the request cards only', () => {
    const t = new Timeline();
    t.apply(log(1, 'warn', 'a'));
    t.apply(workerRequest(2));
    t.apply(log(3, 'error', 'b'));
    const items = onlyRequests(t.items());
    expect(items).toHaveLength(1);
    expect(items[0].kind === 'event' && items[0].event.type).toBe('approval.request');
  });
});

describe('fork of a transcript (T20, SPEC §9.3)', () => {
  it('draws session_forked as a warning line with an icon', () => {
    const forked = raw('worker.mirror.event.forked').payload.event;
    const t = new Timeline();
    t.apply(
      ev('event.system', (e) => {
        e.seq = 1;
        e.payload = forked.payload;
      }),
    );
    show(t);
    const line = root.querySelector('.line-system-session_forked')!;
    expect(line.classList.contains('line-warning')).toBe(true);
    expect(line.querySelector('.warn-icon')!.getAttribute('aria-label')).toBe('Внимание');
    expect(line.querySelector('b')!.textContent).toBe('Развилка');
    expect(line.textContent).toContain('1 ход из hqc не попал в его контекст');
    // Other system lines stay plain.
    render(null, root);
    const t2 = new Timeline();
    t2.apply(ev('event.system', withSeq(1)));
    show(t2);
    expect(root.querySelector('.line-warning')).toBeNull();
  });
});
