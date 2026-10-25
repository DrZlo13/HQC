# T35: questions and decisions

## 1. Crates: `croner` and `chrono-tz`

- **`croner` 4** (MIT, hexagon/croner-rust; the Rust port of the widely used JS `croner`,
  4.0.2 is current) parses 5-field patterns natively (`Seconds::Disallowed`,
  `Year::Disallowed`), works on `chrono::DateTime<Tz>`, and documents DST handling that follows
  Vixie cron / OCPS, with tests for gaps and overlaps in the crate. The alternatives: `cron`
  (zslayton) needs 6–7 fields with seconds and has no documented DST rules; `saffron` has no
  time zones and is unmaintained. Hand-written cron plus DST is exactly the kind of code the
  task asked not to write.
- **`chrono-tz` 0.10** is the IANA database for `chrono`, which the workspace already uses
  everywhere; `croner` names it as its time zone companion. `jiff` would bring its own database
  but a second date-time library into a `chrono` code base.

What `croner` does on DST (unit tests `schedules::tests`, Europe/Berlin, 2026):
- `0 9 * * *` stays 09:00 local: 08:00Z before 2026-03-29, 07:00Z after; back to 08:00Z after
  2026-10-25.
- `30 2 * * *` on 2026-03-29 (02:30 does not exist): 03:00 CEST, right after the gap.
- `30 2 * * *` on 2026-10-25 (02:30 happens twice): once, the first time (00:30Z, CEST).
- Interval patterns (`*/30 * * * *`) run in both passes of the repeated hour; that is the Vixie
  behaviour and fine for messages at least 5 minutes apart.

## 2. The clock

`AppState::clock` (`schedules::Clock`) is the system time unless a test sets it. Everything
in the cron reads it (creation, resume, the passes); tests set it and call
`schedules::run_due` themselves. The background loop would race with that, so once the clock is
set the loop stands aside: it skips its pass, and `fire` checks again after locking a row (a
test sets the clock before it creates schedules, so any row the loop could lock is already the
test's). Production never sets it.

## 3. Missed runs and `catch_up`

"Missed" needs a threshold, because every run is a little late (the loop ticks every 10 s). A
run found more than **2 minutes** after its time was missed (the hub was down); within that it
is on time and is sent whatever `catch_up` says. 2 minutes is below the 5-minute minimum
interval, so an on-time run never jumps over another one. After a missed run (sent or skipped)
the next run is the first one after now: several missed runs of a `cron` give at most one
message. `catch_up` defaults to false (the spec's "otherwise skipped"); the tool and the API
take it.

An `at` that ran or was skipped keeps its row with `next_run` null, so the client can show
"sent at …"; it does not count toward the per-owner limit. The rows of finished schedules stay
until the user deletes them; a cleanup can come later if they pile up.

## 4. Sending once

A run locks its row (`SELECT … FOR UPDATE`) while it stores the message and moves `next_run`,
in one transaction. The message's `event_id` is `once_id("schedule.run", schedule id, run time)`,
so a run repeated after a crash between the two (the event stored, `next_run` not moved) finds
the event already there and only moves `next_run`. A `PATCH`/`DELETE` during a run waits for the
row and then applies; "paused and deleted schedules never fire" holds from the moment the
request returns.

## 5. Targets that take no input

The run goes the normal way (`pending`, then `session.input`), so a busy session queues it and
an offline worker gets it on `hello`. A session that is archived or driven from VS Code refuses
user input in the API (`409 archived`, `409 locked_vscode`, "no silent queue"), so its runs are
skipped, with a warn in the hub's log, not queued; the schedule keeps running and resumes when
the session takes input again. A deleted session deletes its schedules (`ON DELETE CASCADE`).
`hqc_schedule` accepts a session driven from VS Code (it may be taken over before the run); the
API refuses to create one for an archived session, the tool cannot see archived sessions at all
(§7.1 target rules).

## 6. The marker and the author

The run is a plain `user.message` authored by the owner, so the agent sees ordinary input and
the turn, chain and unread rules apply unchanged (a `user.message` starts no chain: a scheduled
turn is a human's turn by proxy, so its agent may start new asks). The payload gets
`schedule_id` (an optional field in `hqc_proto::event::EventKind::UserMessage`); the client shows
"· scheduled" next to the time. Nothing else in the hub reads it.

## 7. Time zone of the user

`users.timezone` existed but nothing set it, so every user was UTC and "cron in the user's
timezone" meant UTC. `PATCH /api/me {timezone}` sets it, and the client sends the browser's zone
(`Intl.DateTimeFormat().resolvedOptions().timeZone`) at start when it differs. A schedule
stores its own `timezone` at creation, so a user who travels does not move existing schedules.
The tool and the API also take an explicit `timezone` (the agent may be told "at 9 Berlin time").

## 8. What the agent can do

`hqc_schedule` creates; listing and deleting from the agent were left out: they would be two more
tools in every session's `tools/list` for something the owner manages in the client, and an
agent that schedules into its own session can say in its message what the schedule is for. If
agents need it, `hqc_list_schedules`/`hqc_unschedule` are cheap on top of the same rows.

## 9. Limits (SPEC §20)

- 100 schedules per owner that may still run (paused ones count, finished ones do not):
  `limit_exceeded` / `409 schedule_limit`. T36 reviews the caps of the MCP surface together.
- `cron` runs at least 5 minutes apart, checked over its next 100 runs (catches `*/4`, and also
  `0,1 9 * * *`, whose close pair comes once a day); a pattern that never runs (`0 0 30 2 *`) is
  refused.
- `at` in the future; `message` non-blank and at most 16 KiB.

## 10. Live run

`crates/hqc-hub/tests/live_schedule.rs`, 2026-10-25, home-mac, Claude Code 2.1.308,
`claude-haiku-5-5`, `--max-turns 2`, one run (13.5 s): session `live/planner` called
`mcp__hqc__hqc_schedule {"session": "live/target", "cron": "0 9 * * *", "timezone":
"Europe/Berlin", "message": "Reply with exactly one word: mango"}`; the result had `next_run`
`2026-10-25T08:00:00Z` (09:00 CET: Berlin left summer time that night). The test then set the
hub's clock one second past it and ran the cron: a `user.message` with `schedule_id` landed in
`live/target`, and its agent answered `mango` (final, `in_reply_to` = that message).
