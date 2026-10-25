# T39: a message cannot raise permissions — decisions and questions

SPEC 0.43 (§2.1, §3, §5.3, §5.4, §6, §7, §7.1, §8.1, §10.1, §16, §17.2). The rule is the
owner's decision of 2026-10-25 and is kept exact: a turn woken by a peer message (`hqc_ask`,
`hqc_send` with `wake_on_send`, the prompt of `hqc_spawn`) or by a run of an agent's schedule
runs in "Auto" only if the target and every session up the chain run in "Auto"; otherwise in
"Ask". A turn started by the user's own message runs in the session's mode. It answers T38's
open question 1. One live `claude` run of the two allowed (`live_cap`, `claude-haiku-5-5`,
`--max-turns 2`, Claude Code 2.1.308).

## 1. Decisions

- **One source of truth: the chain.** The cap is a field of the chain (`permission_cap`,
  additive), set by the hub where it already sets the chain (`peers::outgoing_chain`, used by
  `hqc_send`, `hqc_ask`, `hqc_spawn` and `hqc_schedule`). It is stored with the
  `peer.message`, with an agent's schedule and with its runs (`user.message` with a chain), so
  `session.input.permission_cap` is read from the input's own chain and nothing else is kept.
- **"Every session up the chain"** is the lower of the calling session's
  `effective_permission_mode` and the cap of the input that started the calling turn; a
  person's input has no cap. The target's own mode is not part of the cap: a target in Ask runs
  in Ask anyway, and the worker never raises. The calling session's mode is the reported one
  (T38's `effective_permission_mode`), `ask` while nothing is reported: what the hub does not
  know cannot lift a cap.
- **`capped_by`** (with an `ask` cap) is the address of the earliest session of the chain found
  in Ask, kept down the chain: in A (Auto) → B (Ask) → C (Auto) → D, D's turn says "asked by
  B", the session whose mode is the reason, not C, which only passed it on. In the common
  case (A asks B) both are the sender.
- **The worker applies a cap only downwards, before the runtime gets the input.** A capped
  input for a process asked to run Auto: the worker sends `set_permission_mode` (`default`)
  and keeps the input until the runtime confirms Ask; only then is it written to stdin. Without
  a confirmation in 10 s (`MODE_SWITCH_TIMEOUT`), or with a runtime that cannot switch, the
  process is killed and resumed in Ask (the input goes back to the head of the queue). A
  process started for a capped input starts with the Ask flag and needs no switch. Writing the
  control request and the input back to back would most likely work too (the CLI reads stdin in
  order), but waiting for the confirmation does not depend on that.
- **Every confirmation is reported.** The engine used to report a mode only when it changed;
  a capped input waits for the answer to its own request, so the engine now reports every
  success answer to the adapter's `set_permission_mode` (the CLI's own `system/status` and
  `system/init` still only on change). Otherwise a switch back to Auto refused by the CLI
  would leave the next capped input waiting for its timeout.
- **After the turn** (a final answer, also after `stop`): the session's own mode is asked for
  again, unless the next queued input is capped too (no pointless Auto → Ask round trip). An
  uncapped input always runs in the session's own mode, so a person's input queued behind a
  capped turn runs in Auto. A turn lost with its process (crash, kill, worker restart) leaves
  nothing to undo: the next process starts in the mode of the input it runs.
- **A switch during a capped turn** changes the session's mode at once (`spec`), but reaches
  the runtime when the capped turn ends; a switch to Auto never raises a capped turn.
- **The header.** The worker says which input's cap holds an Auto session down
  (`session.state.capped_input`, absent = none, unlike `title`), and the hub turns it into the
  session's `permission_capped_by` (migration `0022`), cleared by a report without it and by a
  worker restart. The client shows "Ask (asked by <address>) · Auto after this turn": both
  the turn's mode and the session's own. The worker reports it only when the session's own mode
  is Auto, so a session in Ask shows plain "Ask".
- **An agent's schedule keeps the cap it was made with**, as it keeps its chain (T36). A
  person's schedule has no chain and no cap.

## 2. Tests

| what | where |
|---|---|
| the cap: human turn, Auto/Ask sender, down a chain, the earliest Ask names it, an old chain | hub `peers::tests::the_cap_is_the_lowest_mode_up_the_chain` |
| the four combinations (Auto/Ask sender × target), the header field set and cleared | hub `tests/t39.rs::the_cap_is_the_senders_mode_in_all_four_combinations` |
| A (Auto) → B (Ask) → C (Auto) → D, and a spawn under the cap | `…::ask_in_the_middle_of_a_chain_caps_everything_below_it` |
| the reported mode counts, not the asked one; nothing reported is Ask | `…::the_reported_mode_counts` |
| an agent's schedule keeps its cap, a person's has none | `…::an_agents_schedule_keeps_its_cap` |
| a worker restart clears the header field | `…::a_worker_restart_clears_the_cap` |
| the switch down, the wait for the confirmation, back to Auto before a queued user input | worker `tests/worker.rs::a_capped_turn_runs_in_ask_and_the_mode_comes_back` |
| a user's switch during a capped turn waits for its end | `…::a_switch_during_a_capped_turn_waits_for_its_end` |
| the cap never raises | `…::the_cap_never_raises` |
| a runtime that cannot switch is restarted in Ask; later starts in the own mode | `…::a_runtime_that_cannot_switch_is_restarted_in_ask` |
| a stopped agent starts in Ask; a lost turn leaves nothing behind | `…::a_stopped_agent_starts_in_ask_for_a_capped_input` |
| every confirmation of ours is reported | `adapter::claude_code::tests::permission_mode_switch_in_the_stream` |
| the header mark and the summary that ends it | `client/tests/chat.test.tsx` "permission mode" |
| the wire examples (chain, input, state, session, summary) | `hqc-proto` round trips, `client/tests/proto.test.ts` |
| live: A in Ask asks B in Auto; B's `touch` is asked about | `crates/hqc-hub/tests/live_cap.rs` |

The live run: B's process started with `--permission-mode auto` (the hub starts B before its
input arrives), got the capped question, was switched to `default` and confirmed it 3 ms later;
only then did it get the question. Its `touch t39-capped` came as an `approval.request` while
B showed `permission_capped_by: live/asker` and `effective_permission_mode: ask`; the test
denied it, B replied to A ("the command was denied"), and B was back in Auto with no cap. The
file was not created.

## 3. Not verified / open questions

1. **Buffered sends.** A `send` into the send buffer does not start a turn, so the rule does not
   apply: its text reaches the agent in `peer_prefix` of the next input, and if that is the
   person's own message the turn runs in the session's mode. A session in Ask can thus put text
   in front of an Auto session's next human turn. Capping such a turn too would follow the
   spirit of the rule but not its letter ("a turn started by the user's own message runs in the
   session's mode"); left for the owner.
2. **A schedule outliving its creator's mode.** The cap is fixed when an agent's schedule is
   made; if that session later switches from Auto to Ask, the schedule's runs still run in Auto
   (the other direction is harmless). Recomputing at each run would need the creating session,
   which a schedule does not store.
3. **The restart fallback** (no confirmation in 10 s, or a runtime that cannot switch) is tested
   with the fake adapter only; Claude Code 2.1.308 always confirmed.
4. **Messages from before 0.43** still pending when the hub is upgraded carry no cap and run in
   the target's mode.
5. **The confirmation is matched by order**, not by the request's id: the worker takes the next
   reported Ask after asking for Ask. The CLI answers control requests in order and reports its
   own mode changes only after making them, so a stale Ask cannot come in between; matching ids
   would need a separate adapter event.
6. **The hub is trusted with the cap**, like with the answers to requests (§2.1): a compromised
   hub can leave it out. The worker cannot check the chain.
