// The session's scheduled messages (SPEC §16, T35): agents create them with `hqc_schedule`;
// here the owner sees them, pauses, resumes and deletes them.

import { useEffect, useState } from 'preact/hooks';
import type { Api } from '../api';
import type { Schedule } from '../proto';
import { formatDay } from './labels';

/** "`0 9 * * *` (Europe/Berlin)" or "once, today at 09:00". */
export function scheduleWhen(s: Schedule, now: number): string {
  if (s.cron) return `${s.cron} (${s.timezone})`;
  return s.at ? `once, ${formatDay(s.at, now)}` : 'once';
}

function nextLabel(s: Schedule, now: number): string {
  if (!s.enabled) return 'paused';
  if (s.next_run) return `next ${formatDay(s.next_run, now)}`;
  return s.last_run ? `sent ${formatDay(s.last_run, now)}` : 'done';
}

export function SchedulesPanel({ api, sessionId, onClose }: { api: Api; sessionId: string; onClose: () => void }) {
  const [list, setList] = useState<Schedule[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    api.schedules(sessionId).then(setList, (e: Error) => setError(e.message));
  }, [api, sessionId]);

  const run = async (fn: () => Promise<unknown>) => {
    setError(null);
    try {
      await fn();
      setList(await api.schedules(sessionId));
    } catch (e) {
      setError((e as Error).message);
    }
  };
  const now = Date.now();
  return (
    <div class="schedules" role="region" aria-label="Schedules">
      <div class="schedules-head">
        <b>Scheduled messages</b>
        <button type="button" class="btn btn-small btn-outline" onClick={onClose}>
          Close
        </button>
      </div>
      {error && <div class="error-text">{error}</div>}
      {list && list.length === 0 && (
        <p class="muted">None. An agent schedules messages into a session with hqc_schedule.</p>
      )}
      {list && list.length > 0 && (
        <ul class="plain-list">
          {list.map((s) => (
            <li key={s.id} class={`schedule ${s.enabled ? '' : 'schedule-paused'}`}>
              <div class="schedule-main">
                <div class="schedule-when">
                  <span class="mono">{scheduleWhen(s, now)}</span> <span class="muted">· {nextLabel(s, now)}</span>
                </div>
                <div class="schedule-text">{s.message}</div>
              </div>
              <div class="schedule-actions">
                {s.next_run && (
                  <button
                    type="button"
                    class="btn btn-small btn-outline btn-pause"
                    onClick={() => run(() => api.patchSchedule(s.id, { enabled: !s.enabled }))}
                  >
                    {s.enabled ? 'Pause' : 'Resume'}
                  </button>
                )}
                <button
                  type="button"
                  class="btn btn-small btn-outline btn-delete-schedule"
                  onClick={() => confirm('Delete this scheduled message?') && run(() => api.deleteSchedule(s.id))}
                >
                  Delete
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
