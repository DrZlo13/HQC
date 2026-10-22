// @vitest-environment happy-dom
// Rendering of chat cards from examples/wire (SPEC §4, §10.1).

import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { parseEvent, type HqcEvent } from '../src/proto';
import { Timeline } from '../src/timeline';
import { ChatItems, type ChatActions } from '../src/ui/cards';
import { CANONICAL_LABELS } from '../src/ui/labels';

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
      expect(root.children.length).toBeGreaterThan(0);
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

  it('updates delivery when the same event comes again', () => {
    const t = new Timeline();
    t.apply(ev('event.user.message', (e) => (e.delivery = 'pending')));
    t.apply(ev('event.user.message'));
    expect(t.events()).toHaveLength(1);
    expect(t.events()[0].delivery).toBe('delivered');
  });

  it('renders status and system as lines', () => {
    const t = new Timeline();
    t.apply(ev('event.status', withSeq(1)));
    t.apply(ev('event.system', withSeq(2)));
    show(t);
    const lines = [...root.querySelectorAll('.line')].map((l) => l.textContent);
    expect(lines).toEqual(['ждёт ответа · approval requested', 'Session created']);
  });
});
