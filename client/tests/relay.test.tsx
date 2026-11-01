// @vitest-environment happy-dom
// T51: messages between sessions in the feed, from the events of a real exchange
// (`fixtures/t51-*.json`, the shapes of the T41 and T42 live runs): "Question from" and
// "Reply to" in the answering session, "Sent to" and "Reply from" in the asking one, a send
// that went with the reply inside the reply's block, the other session as a link or, deleted,
// as text.
import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { parseEvent, type HqcEvent } from '../src/proto';
import { Timeline } from '../src/timeline';
import { ChatItems, type ChatActions } from '../src/ui/cards';
import { formatTime } from '../src/ui/labels';

interface Fixture {
  asker_id: string;
  helper_id: string;
  asker: unknown[];
  helper: unknown[];
}

const load = (name: string): Fixture => JSON.parse(readFileSync(join(import.meta.dirname, 'fixtures', name), 'utf8'));
const exchange = load('t51-exchange.json');
const replySend = load('t51-reply-send.json');

let root: HTMLElement | undefined;
afterEach(() => {
  if (root) render(null, root);
  root = undefined;
  document.body.innerHTML = '';
});

/** The sessions the user has: the fixture's two by address and id. */
function sessions(f: Fixture, asker: string, helper: string, gone: string[] = []) {
  const all = [
    { id: f.asker_id, address: asker },
    { id: f.helper_id, address: helper },
  ].filter((s) => !gone.includes(s.address));
  return (ref: string) => all.find((s) => s.id === ref || s.address === ref);
}

async function feed(raw: unknown[], over: Partial<ChatActions> = {}) {
  const t = new Timeline();
  t.applyAll(raw.map(parseEvent));
  const actions: ChatActions = {
    timeline: t,
    approve: async () => {},
    answer: async () => {},
    resend: () => {},
    discard: () => {},
    openArtifact: vi.fn(),
    ...over,
  };
  root = document.createElement('div');
  document.body.appendChild(root);
  await act(async () => render(<ChatItems items={t.items()} actions={actions} />, root!));
  // The other session's events are asked for after the first render.
  await act(async () => {});
  return { el: root, t, actions };
}

const blocks = (el: Element) => [...el.querySelectorAll<HTMLElement>('.relay')];
const head = (b: Element) =>
  [b.querySelector('.relay-label'), b.querySelector('.relay-peer'), b.querySelector('.relay-when')].map((x) => x?.textContent ?? '');
const ev = (raw: unknown[], seq: number) => parseEvent(raw.find((e) => (e as { seq: number }).seq === seq));

describe('the answering session (esp/session-5)', () => {
  it('"↙ Question from" with its file and "↗ Reply to · delivered" with the turn\'s attachment and tokens', async () => {
    const { el } = await feed(exchange.helper, { findSession: sessions(exchange, 'esp/session-4', 'esp/session-5') });
    const [q, r, ...rest] = blocks(el);
    expect(rest).toEqual([]);
    expect(q.classList.contains('relay-in')).toBe(true);
    expect(head(q)).toEqual(['Question from', 'esp/session-4', formatTime('2026-10-31T23:11:02Z')]);
    expect(q.querySelector('a.relay-peer')!.getAttribute('href')).toBe(`#/s/${exchange.asker_id}`);
    expect(q.querySelector('.text')!.textContent).toContain('This is esp/session-4');
    expect(q.querySelector('.attachment')).not.toBeNull();

    expect(r.classList.contains('relay-out')).toBe(true);
    expect(head(r)).toEqual(['Reply to', 'esp/session-4', `delivered ${formatTime('2026-10-31T23:11:19Z')}`]);
    // The reply's text, its table, the file attached in the turn with its caption, the tokens.
    expect(r.querySelector('table')!.textContent).toContain('sha1 before');
    expect(r.querySelector('.attachment-name')!.textContent).toBe('hqc-xfer-test.txt');
    expect(r.querySelector('.attachment-caption')!.textContent).toBe('Edited copy: line replaced, checked timestamp appended');
    expect(r.lastElementChild!.textContent).toContain('cache 209k');
    // The attachment is not drawn a second time, the commands stay a group.
    expect(el.querySelectorAll('.attachment').length).toBe(2);
    expect(el.querySelectorAll('.tool-group').length).toBe(1);
    expect(el.querySelector('.msg-final')).toBeNull();
  });

  it('no delivery time before the asking worker acknowledged the reply, and the time once the hub sends it', async () => {
    const raw = exchange.helper.map((e) => {
      const { reply_delivered_at: _, ...rest } = e as Record<string, unknown>;
      return rest;
    });
    const { el, t } = await feed(raw, { findSession: sessions(exchange, 'esp/session-4', 'esp/session-5') });
    expect(head(blocks(el)[1])[2]).toBe('');
    // The hub sends the reply again with the time (SPEC §6); a later copy without it keeps it.
    t.apply(ev(exchange.helper, 9));
    t.apply(parseEvent(raw[8]));
    const reply = t.events().find((e) => e.seq === 9) as HqcEvent;
    expect(reply.reply_delivered_at).toBe('2026-10-31T23:11:19Z');
  });

  it('a send that went with the reply shows after its separator inside the reply (T42)', async () => {
    const asker = { session_id: replySend.asker_id, seq: 3 };
    const peerMessage = vi.fn(async (sid: string, seq: number) =>
      sid === asker.session_id && seq === asker.seq ? ev(replySend.asker, 3) : undefined,
    );
    const { el } = await feed(replySend.helper, { findSession: sessions(replySend, 'live/asker', 'live/helper'), peerMessage });
    const [q, r, ...rest] = blocks(el);
    expect(rest).toEqual([]);
    expect(head(q)[0]).toBe('Question from');
    expect(head(r)).toEqual(['Reply to', 'live/asker', `delivered ${formatTime('2026-10-31T23:20:17Z')}`]);
    const parts = [...r.querySelectorAll('.text, .relay-sep')].map((x) => x.textContent);
    expect(parts).toEqual(['done', 'sent with hqc_send while answering', 'fixed']);
    // Its file as it arrived in the asker.
    expect(peerMessage).toHaveBeenCalledWith(replySend.asker_id, 3);
    expect(r.querySelectorAll('.attachment').length).toBe(1);
    expect(el.querySelector('.tool-group')).toBeNull();
  });
});

describe('the asking session (esp/session-4)', () => {
  it('"↗ Sent to · delivered" with the file it handed over and "↙ Reply from" with Open / Download', async () => {
    const peerMessage = vi.fn(async (sid: string, seq: number) =>
      sid === exchange.helper_id && seq === 2 ? ev(exchange.helper, 2) : undefined,
    );
    const { el, actions } = await feed(exchange.asker, {
      findSession: sessions(exchange, 'esp/session-4', 'esp/session-5'),
      peerMessage,
    });
    const [s, r, ...rest] = blocks(el);
    expect(rest).toEqual([]);
    expect(s.classList.contains('relay-out')).toBe(true);
    expect(head(s)).toEqual(['Sent to', 'esp/session-5', `delivered ${formatTime('2026-10-31T23:11:03Z')}`]);
    expect(s.querySelector('a.relay-peer')!.getAttribute('href')).toBe(`#/s/${exchange.helper_id}`);
    expect(s.querySelector('.text')!.textContent).toContain('I attach hqc-xfer-test.txt');
    expect(s.querySelectorAll('.attachment').length).toBe(1);

    expect(r.classList.contains('relay-in')).toBe(true);
    expect(head(r)).toEqual(['Reply from', 'esp/session-5', formatTime('2026-10-31T23:11:19Z')]);
    expect(r.querySelector('table')!.textContent).toContain('sha1 after');
    expect(r.querySelector('.attachment-caption')!.textContent).toMatch(/\.hqc\/inbox\/.+\/hqc-xfer-test\.txt$/);
    const buttons = [...r.querySelectorAll('button')].filter((b) => /Open|Download/.test(b.textContent!));
    expect(buttons.map((b) => b.textContent)).toEqual(['Open', 'Download']);
    await act(async () => buttons[1].click());
    expect(actions.openArtifact).toHaveBeenCalledWith('0199f3a2-51a0-7000-8000-0000000000f2', true);
    // The Bash call stays in its group; the ask is not a row of it.
    expect(el.querySelectorAll('.tool-group').length).toBe(1);
  });

  it('before the reply: the file names it gave and "waiting for a reply"', async () => {
    const { el } = await feed(exchange.asker.slice(0, 6), { findSession: sessions(exchange, 'esp/session-4', 'esp/session-5') });
    const [s, ...rest] = blocks(el);
    expect(rest).toEqual([]);
    expect(head(s)).toEqual(['Sent to', 'esp/session-5', 'waiting for a reply']);
    expect(s.querySelector('.relay-file')!.textContent).toBe('hqc-xfer-test.txt');
  });

  it('a send that went with the reply shows after its separator, not as a message of its own (T42)', async () => {
    const { el } = await feed(replySend.asker, { findSession: sessions(replySend, 'live/asker', 'live/helper') });
    const [s, r, ...rest] = blocks(el);
    expect(rest).toEqual([]);
    expect(head(s)[0]).toBe('Sent to');
    expect(head(r)[0]).toBe('Reply from');
    const parts = [...r.querySelectorAll('.text, .relay-sep')].map((x) => x.textContent);
    expect(parts).toEqual(['done', 'sent with hqc_send while answering', 'fixed']);
    expect(r.querySelector('.attachment-name')!.textContent).toBe('report.txt');
  });

  it('a deleted session is plain text, and nothing is asked of it', async () => {
    const peerMessage = vi.fn(async () => undefined);
    const { el } = await feed(exchange.asker, {
      findSession: sessions(exchange, 'esp/session-4', 'esp/session-5', ['esp/session-5']),
      peerMessage,
    });
    for (const b of blocks(el)) {
      expect(b.querySelector('a')).toBeNull();
      expect(b.querySelector('.relay-peer')!.textContent).toBe('esp/session-5');
    }
    expect(peerMessage).not.toHaveBeenCalled();
    // "Sent to" knows no delivery time then, the file names stay.
    expect(head(blocks(el)[0])[2]).toBe('');
    expect(blocks(el)[0].querySelector('.relay-file')!.textContent).toBe('hqc-xfer-test.txt');
  });
});
