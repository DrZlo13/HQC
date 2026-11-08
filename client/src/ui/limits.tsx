// The usage limits of a session's worker account, in the chat's "More" menu under the model
// (SPEC §6): the 5-hour and the 7-day window, as the worker last reported them.

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
