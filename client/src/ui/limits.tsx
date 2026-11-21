// The usage limits of a session's worker account, in the chat's "More" menu under the model
// (SPEC §6): the 5-hour and the 7-day window, as the worker last reported them; and above the
// input a bar while a window is at the hub's share (`Me.limit_notice_percent`, 70 % by default)
// or the account has hit a limit.

import { useEffect, useState } from 'preact/hooks';

import type { LimitWindow, WorkerLimits } from '../api';

/** When a window resets: the time today or tomorrow, else the weekday with the time. */
export function resetsLabel(at: number, now = Date.now()): string {
  const d = new Date(at * 1000);
  const time = d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  if (at * 1000 - now < 24 * 3600 * 1000) return time;
  return `${d.toLocaleDateString([], { weekday: 'short' })} ${time}`;
}

function Row({ name, w, now }: { name: string; w: LimitWindow; now: number }) {
  const pct = Math.round(Math.min(1, Math.max(0, w.utilization)) * 100);
  return (
    <div class="limit-row">
      <span class="limit-name">{name}</span>
      <span class="limit-bar" aria-hidden="true">
        <span class={`limit-fill ${pct >= 90 ? 'limit-high' : ''}`} style={{ width: `${pct}%` }} />
      </span>
      <span class="limit-pct">{pct}%</span>
      <span class="limit-reset">resets {resetsLabel(w.resets_at, now)}</span>
    </div>
  );
}

/** The limits by `rate_limit_type`, as the VS Code tab names them. */
const LIMIT_NAMES: Record<string, string> = {
  five_hour: '5-hour limit',
  seven_day: 'weekly limit',
  seven_day_opus: 'weekly Opus limit',
  seven_day_sonnet: 'weekly Sonnet limit',
  seven_day_overage_included: 'Fable limit',
  overage: 'usage credit limit',
};

/** The bar's text, as the VS Code tab words it: "You've hit your 5-hour limit · resets 18:00"
 *  once the account is `rejected`, else "You've used 82% of your weekly limit · resets …" for
 *  the fuller window at `percent` (the hub's `limit_notice_percent`) or more; nothing below, and
 *  nothing for a limit or a window that has reset since it was reported (one without a reset
 *  time counts as current). */
export function limitNotice(l: WorkerLimits, now = Date.now(), percent = 70): string | undefined {
  const current = (resetsAt?: number | null) => !resetsAt || resetsAt * 1000 > now;
  if (l.status === 'rejected' && current(l.resets_at)) {
    const name = (l.rate_limit_type && LIMIT_NAMES[l.rate_limit_type]) || 'usage limit';
    return `You've hit your ${name}${l.resets_at ? ` · resets ${resetsLabel(l.resets_at, now)}` : ''}`;
  }
  const windows = (['five_hour', 'seven_day'] as const).flatMap((k) => {
    const w = l[k];
    return w && w.utilization >= percent / 100 && current(w.resets_at) ? [{ k, w }] : [];
  });
  const top = windows.sort((a, b) => b.w.utilization - a.w.utilization)[0];
  if (!top) return undefined;
  const pct = Math.floor(Math.min(1, top.w.utilization) * 100);
  return `You've used ${pct}% of your ${LIMIT_NAMES[top.k]} · resets ${resetsLabel(top.w.resets_at, now)}`;
}

/** Above the input while [`limitNotice`] has something to say: the limits load when shown and
 *  again whenever `refresh` changes (the session's state: a turn that starts or ends). */
export function LimitBar({
  load,
  refresh,
  percent,
  now = Date.now,
}: {
  load: () => Promise<WorkerLimits | undefined>;
  refresh: unknown;
  percent: number;
  now?: () => number;
}) {
  const [limits, setLimits] = useState<WorkerLimits | undefined>();
  useEffect(() => {
    let live = true;
    load().then(
      (l) => live && setLimits(l),
      () => {},
    );
    return () => {
      live = false;
    };
  }, [refresh]);
  const text = limits && limitNotice(limits, now(), percent);
  if (!text) return null;
  return (
    <div class={`limit-notice ${limits.status === 'rejected' ? 'limit-notice-hit' : ''}`} role="status">
      {text}
    </div>
  );
}

/** Loads the limits when shown (the menu opens); nothing while there are none. */
export function Limits({ load, now = Date.now }: { load: () => Promise<WorkerLimits | undefined>; now?: () => number }) {
  const [limits, setLimits] = useState<WorkerLimits | undefined>();
  useEffect(() => {
    let live = true;
    load().then(
      (l) => live && setLimits(l),
      () => {},
    );
    return () => {
      live = false;
    };
  }, []);
  if (!limits || (!limits.five_hour && !limits.seven_day)) return null;
  const t = now();
  return (
    <div class="limits" aria-label="Usage limits">
      <span class="field-label">Limits</span>
      {limits.five_hour && <Row name="Session" w={limits.five_hour} now={t} />}
      {limits.seven_day && <Row name="Week" w={limits.seven_day} now={t} />}
    </div>
  );
}
