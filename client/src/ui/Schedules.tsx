// The session's scheduled messages (SPEC §16, T35): agents create them with `hqc_schedule`;
// the owner sees them in the block above the input, pauses, resumes and deletes them.

import { useEffect, useState } from 'preact/hooks';
import type { Api } from '../api';
import type { Schedule } from '../proto';
import { IconPause, IconPlay, IconTrash } from './icons';
import { formatDay } from './labels';
import { Loading } from './Loading';

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

/** The list under the "Scheduled:" rows of the block above the input (LiveSubagents), one item
 *  for each message with its pause and delete buttons; `refresh` changes when the hub sends a new
 *  `schedules.live`, which reads the list again. */
export function SchedulesList({ api, sessionId, refresh }: { api: Api; sessionId: string; refresh: number }) {
  const [list, setList] = useState<Schedule[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    // Only the latest read lands, whichever answers first; a good answer clears an old error.
    let alive = true;
    api.schedules(sessionId).then(
      (l) => {
        if (!alive) return;
        setList(l);
        setError(null);
      },
      (e: Error) => {
        if (alive) setError(e.message);
      },
    );
    return () => {
      alive = false;
    };
  }, [api, sessionId, refresh]);

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
    <>
      {!list && !error && (
        <div class="live-agents-note live-schedules-note">
          <Loading />
        </div>
      )}
      {error && <div class="live-agents-note live-schedules-note">{error}</div>}
      {list && list.length === 0 && (
        <div class="live-agents-note live-schedules-note">
          None. An agent schedules messages into a session with hqc_schedule.
        </div>
      )}
      {list && list.length > 0 && (
        <ul class="live-schedules" aria-label="Scheduled messages">
          {list.map((s) => (
            <li key={s.id} class={s.enabled ? 'live-schedule' : 'live-schedule live-schedule-paused'}>
              <div class="live-agents-row">
                <span class="live-agents-what live-schedule-when">{`${scheduleWhen(s, now)} · ${nextLabel(s, now)}`}</span>
                {s.next_run && (
                  <button
                    type="button"
                    class="icon-btn live-agents-stop btn-pause-schedule"
                    aria-label={s.enabled ? 'Pause this scheduled message' : 'Resume this scheduled message'}
                    title={s.enabled ? 'Pause this scheduled message' : 'Resume this scheduled message'}
                    onClick={() => run(() => api.patchSchedule(s.id, { enabled: !s.enabled }))}
                  >
                    {s.enabled ? <IconPause size={12} /> : <IconPlay size={12} />}
                  </button>
                )}
                <button
                  type="button"
                  class="icon-btn live-agents-stop btn-delete-schedule"
                  aria-label="Delete this scheduled message"
                  title="Delete this scheduled message"
                  onClick={() => confirm('Delete this scheduled message?') && run(() => api.deleteSchedule(s.id))}
                >
                  <IconTrash size={12} />
                </button>
              </div>
              <p class="live-schedule-text">{s.message}</p>
            </li>
          ))}
        </ul>
      )}
    </>
  );
}
