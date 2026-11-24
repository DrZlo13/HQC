// @vitest-environment happy-dom
// T35: scheduled messages (SPEC §16): the wire shapes, the "scheduled" mark on a
// `user.message` the hub cron sent, and the session's list with pause, resume and delete.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import type { Api } from '../src/api';
import { parseEvent, parseMe, parseSchedule, parseScheduleList, type Schedule } from '../src/proto';
import { ChatItems, type ChatActions } from '../src/ui/cards';
import { SchedulesList, scheduleWhen } from '../src/ui/Schedules';
import { Timeline } from '../src/timeline';

const WIRE = join(import.meta.dirname, '../../crates/hqc-proto/examples/wire');
const load = (f: string): unknown => JSON.parse(readFileSync(join(WIRE, f), 'utf8'));

let root: HTMLElement;
afterEach(() => {
  if (root) render(null, root);
  document.body.innerHTML = '';
  vi.unstubAllGlobals();
});

function mountRoot() {
  root = document.createElement('div');
  document.body.appendChild(root);
}

const button = (label: string) => root.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`);

describe('wire', () => {
  it('parses schedules', () => {
    const s = parseSchedule(load('api.schedule.json'));
    expect(s).toMatchObject({ cron: '0 9 * * 1-5', timezone: 'Europe/Berlin', enabled: true, catch_up: false });
    expect(s.at).toBeUndefined();
    const [at] = parseScheduleList(load('api.schedules.json'));
    expect(at.at).toBe('2026-10-26T15:30:00Z');
    expect(at.last_run).toBeUndefined();
    expect(scheduleWhen(s, Date.now())).toBe('0 9 * * 1-5 (Europe/Berlin)');
    expect(scheduleWhen(at, Date.now())).toMatch(/^once, /);
  });

  it('reads the user timezone and the schedule mark', () => {
    expect(parseMe({ id: 'u', email: 'a@b', personal_project_id: 'p', timezone: 'Asia/Tokyo' }).timezone).toBe(
      'Asia/Tokyo',
    );
    const ev = parseEvent(load('event.user.message.scheduled.json'));
    expect(ev.type === 'user.message' && ev.payload.schedule_id).toBe('019a1c2e-5b00-7000-8000-00000000c001');
    const plain = parseEvent(load('event.user.message.json'));
    expect(plain.type === 'user.message' && plain.payload.schedule_id).toBeUndefined();
  });
});

it('marks a message the hub cron sent', () => {
  mountRoot();
  const t = new Timeline();
  t.apply(parseEvent(load('event.user.message.scheduled.json')));
  const actions = { timeline: t } as unknown as ChatActions;
  render(<ChatItems items={t.items()} actions={actions} />, root);
  expect(root.querySelector('.msg-user .scheduled')?.textContent).toContain('scheduled');
});

it('lists the session schedules, pauses, resumes and deletes them', async () => {
  vi.stubGlobal('confirm', () => true);
  const base = parseSchedule(load('api.schedule.json'));
  let list: Schedule[] = [base];
  const schedules = vi.fn(async () => list);
  const patchSchedule = vi.fn(async (_id: string, p: { enabled: boolean }) => {
    list = [{ ...base, enabled: p.enabled }];
    return list[0];
  });
  const deleteSchedule = vi.fn(async () => {
    list = [];
  });
  const api = { schedules, patchSchedule, deleteSchedule } as unknown as Api;
  mountRoot();
  await act(async () => render(<SchedulesList api={api} sessionId="s1" refresh={1} />, root));
  await act(async () => {});
  expect(schedules).toHaveBeenCalledWith('s1');
  expect(root.querySelector('ul.live-schedules')?.getAttribute('aria-label')).toBe('Scheduled messages');
  expect(root.querySelector('.live-schedule-text')?.textContent).toBe(base.message);
  expect(root.querySelector('.live-schedule-when')?.textContent).toContain('next ');
  // No header and no Close: the list belongs to the block above the input.
  expect(root.querySelector('.schedules-head')).toBeNull();

  await act(async () => button('Pause this scheduled message')!.click());
  await act(async () => {});
  expect(patchSchedule).toHaveBeenCalledWith(base.id, { enabled: false });
  expect(root.querySelector('.live-schedule-paused .live-schedule-when')?.textContent).toContain('paused');

  await act(async () => button('Resume this scheduled message')!.click());
  await act(async () => {});
  expect(patchSchedule).toHaveBeenLastCalledWith(base.id, { enabled: true });
  expect(root.querySelector('.live-schedule-paused')).toBeNull();

  await act(async () => button('Delete this scheduled message')!.click());
  await act(async () => {});
  expect(deleteSchedule).toHaveBeenCalledWith(base.id);
  expect(root.querySelector('.live-agents-note')?.textContent).toContain('None.');
});

it('reads the list again when the hub sends a new one (refresh changes)', async () => {
  const schedules = vi.fn(async () => [] as Schedule[]);
  const api = { schedules } as unknown as Api;
  mountRoot();
  await act(async () => render(<SchedulesList api={api} sessionId="s1" refresh={1} />, root));
  expect(schedules).toHaveBeenCalledTimes(1);
  await act(async () => render(<SchedulesList api={api} sessionId="s1" refresh={1} />, root));
  expect(schedules).toHaveBeenCalledTimes(1);
  await act(async () => render(<SchedulesList api={api} sessionId="s1" refresh={2} />, root));
  expect(schedules).toHaveBeenCalledTimes(2);
});

describe('schedules.live (SPEC 0.169)', () => {
  it('is kept by the timeline with its time, and gone when the socket drops', () => {
    const t = new Timeline();
    t.apply(parseEvent(load('event.schedules.live.json')));
    expect(t.liveSchedules.map((s) => s.message)).toEqual([
      'Check the nightly build and report failures',
      "Summarize yesterday's commits",
    ]);
    expect(t.liveSchedulesAt).toBeGreaterThan(0);
    // Not a stored event: it takes no place in the feed.
    expect(t.events()).toEqual([]);
    t.reset();
    expect(t.liveSchedules).toEqual([]);
  });
});
