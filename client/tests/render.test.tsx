// @vitest-environment happy-dom
// Rendering of the chat feed from examples/wire (SPEC §4, §10.1), T26 markup: tool groups,
// the permission card with its hotkeys and text answer, messages, service lines.

import { afterEach, describe, expect, it, vi } from "vitest";
import { render } from "preact";
import { act } from "preact/test-utils";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { parseEvent, parseServerFrame, type Activity, type EventOf, type HqcEvent } from "../src/proto";
import { Timeline } from "../src/timeline";
import { PermissionBlock } from "../src/ui/permission";
import {
  ActivityLine,
  ChatItems,
  StreamingMessage,
  onlyRequests,
  type ChatActions,
} from "../src/ui/cards";
import { CANONICAL_LABELS, formatTime } from "../src/ui/labels";
import { argsText, buildFeed, cutJson, relativePaths, subagentState } from "../src/ui/feed";

const WIRE = join(import.meta.dirname, "../../crates/hqc-proto/examples/wire");
const raw = (name: string): any =>
  JSON.parse(readFileSync(join(WIRE, `${name}.json`), "utf8"));
const ev = (name: string, patch: (e: any) => void = () => {}): HqcEvent => {
  const e = raw(name);
  patch(e);
  return parseEvent(e);
};

let root: HTMLElement;
afterEach(() => {
  if (root) render(null, root);
  document.body.innerHTML = "";
});

const NOW = Date.parse("2026-10-22T10:30:00Z");

function show(timeline: Timeline, over: Partial<ChatActions> = {}) {
  const actions: ChatActions = {
    timeline,
    approve: vi.fn(async () => {}),
    answer: vi.fn(async () => {}),
    resend: vi.fn(),
    discard: vi.fn(),
    openArtifact: vi.fn(),
    now: () => NOW,
    ...over,
  };
  root = document.createElement("div");
  document.body.appendChild(root);
  const draw = () =>
    act(() =>
      render(<ChatItems items={timeline.items()} actions={actions} />, root),
    );
  draw();
  return { actions, draw };
}

const withSeq = (seq: number) => (e: any) => {
  e.seq = seq;
};

const optionButtons = () => [
  ...root.querySelectorAll<HTMLButtonElement>(".perm button[data-option]"),
];
const buttonByText = (text: string) =>
  [...root.querySelectorAll<HTMLButtonElement>("button")].find(
    (b) => b.textContent?.trim() === text,
  );
const key = (k: string) =>
  act(
    () =>
      void document.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: k,
          bubbles: true,
          cancelable: true,
        }),
      ),
  );

const call = (seq: number, call_id: string, tool: string, args: object) =>
  ev("event.tool.call", (e) => {
    e.seq = seq;
    e.ts = new Date(
      Date.parse("2026-10-22T10:00:00Z") + seq * 1000,
    ).toISOString();
    e.payload = { call_id, tool, args_preview: JSON.stringify(args) };
  });
const result = (seq: number, call_id: string, error = false) =>
  ev("event.tool.result", (e) => {
    e.seq = seq;
    e.ts = new Date(
      Date.parse("2026-10-22T10:00:00Z") + seq * 1000,
    ).toISOString();
    e.payload = {
      ...e.payload,
      call_id,
      error,
      result_preview: `out ${call_id}`,
      duration_ms: 1000,
    };
    if (!error) delete e.payload.error_text;
  });
const agentText = (seq: number, text: string, final: boolean) =>
  ev("event.agent.message", (e) => {
    e.seq = seq;
    e.payload = { text, final };
  });

describe("every example event renders", () => {
  // Transport only: the stream text, the subagents and the schedules above the input
  // (chat.test.tsx), and the token rate, which reaches the client as the session's
  // `output_per_s`; a retraction is stored but not drawn.
  const names = readdirSync(WIRE)
    .filter(
      (f) =>
        f.startsWith("event.") &&
        f !== "event.agent.delta.json" &&
        f !== "event.agent.delta.reset.json" &&
        f !== "event.subagents.live.json" &&
        f !== "event.subagents.live.retry.json" &&
        f !== "event.retraction.json" &&
        f !== "event.tokens.rate.json" &&
        f !== "event.schedules.live.json",
    )
    .map((f) => f.replace(/\.json$/, ""));
  for (const [i, name] of names.entries()) {
    it(name, () => {
      const t = new Timeline();
      t.apply(ev(name, withSeq(i + 1)));
      show(t);
      // `status` is drawn only for an error with a reason (T13).
      expect(root.children.length).toBe(name === "event.status" ? 0 : 1);
    });
  }

  it("agent.delta streams into a buffer and is not stored", () => {
    const t = new Timeline();
    t.apply(ev("event.agent.delta"));
    t.apply(ev("event.agent.delta", (e) => (e.payload.text = " tests")));
    expect(t.streaming).toBe("Running tests");
    expect(t.items()).toEqual([]);
    t.apply(ev("event.agent.message"));
    expect(t.streaming).toBe("");
  });

  it("agent.delta with reset drops the text of an abandoned attempt (SPEC 0.161)", () => {
    const t = new Timeline();
    t.apply(ev("event.agent.delta"));
    t.apply(ev("event.agent.delta.reset"));
    expect(t.streaming).toBe("");
    t.apply(ev("event.agent.delta", (e) => (e.payload.text = "Retried")));
    expect(t.streaming).toBe("Retried");
  });

  it("a retraction ends the text being written, which no message ends then (SPEC 0.179)", () => {
    const t = new Timeline();
    t.apply(ev("event.agent.delta", (e) => (e.payload.text = "I can't help")));
    t.apply(ev("event.retraction"));
    expect(t.streaming).toBe("");
    t.apply(ev("event.agent.delta", (e) => (e.payload.text = "Here it is.")));
    expect(t.streaming).toBe("Here it is.");
  });

  it("an older page loaded behind the live tail ends no stream", () => {
    const t = new Timeline();
    t.apply(ev("event.agent.message", withSeq(50)));
    t.apply(ev("event.agent.delta"));
    t.applyAll([ev("event.agent.message", withSeq(10)), ev("event.status", withSeq(11))]);
    expect(t.streaming).toBe("Running");
  });
});

describe("the thinking being written (SPEC §6)", () => {
  const thinking = (text: string) =>
    ev("event.agent.delta", (e) => (e.payload = { text, thinking: true }));

  it("is drawn as the thinking it will be stored as, in its place", () => {
    const t = new Timeline();
    t.apply(ev("event.user.message", withSeq(1)));
    t.apply(thinking("Checking"));
    const { actions } = show(t);
    // As the chat draws its feed: the answer being written under the blocks.
    const draw = () =>
      act(() =>
        render(
          <>
            <ChatItems items={t.items()} actions={actions} />
            {t.streaming && <StreamingMessage text={t.streaming} />}
          </>,
          root,
        ),
      );
    // The thinking line of each block of the feed.
    const blocks = () =>
      [...root.querySelectorAll(".feed-block")].map(
        (b) => b.querySelector(".thinking summary")?.textContent,
      );
    draw();
    expect(blocks()).toEqual([undefined, "Thinking · Checking"]);
    t.apply(thinking(" the tests first"));
    draw();
    expect(blocks()).toEqual([undefined, "Thinking · Checking the tests first"]);
    expect(root.querySelector(".thinking-text")!.textContent).toBe("Checking the tests first");
    expect(t.streaming).toBe("");
    // The user opens it to read the thinking as it is written.
    const live = root.querySelector<HTMLDetailsElement>("details.thinking")!;
    act(() => {
      live.setAttribute("open", "");
      live.dispatchEvent(new Event("toggle"));
    });

    // The stored text, told apart from the live one by its full stop.
    t.apply(
      ev("event.agent.thinking", (e) => {
        e.seq = 2;
        e.payload.text = "Checking the tests first.";
      }),
    );
    draw();
    expect(t.thinking).toBeNull();
    expect(blocks()).toEqual([undefined, "Thinking · Checking the tests first."]);
    // It stays open when the thinking is stored.
    const stored = root.querySelector<HTMLDetailsElement>("details.thinking")!;
    expect(stored.hasAttribute("open")).toBe(true);
    expect(stored.querySelector(".thinking-text")!.textContent).toBe("Checking the tests first.");

    t.apply(ev("event.agent.delta", (e) => (e.payload.text = "Yes.")));
    draw();
    expect(blocks()).toEqual([undefined, "Thinking · Checking the tests first."]);
    expect(root.querySelector(".msg-streaming")!.textContent).toBe("Yes.");
  });

  it("after a tool call stands under its folded group, not in it, and joins it when stored", () => {
    const t = new Timeline();
    t.applyAll([call(1, "c1", "Bash", { command: "ls" }), result(2, "c1")]);
    t.apply(thinking("Checking"));
    const { draw } = show(t);
    expect(buildFeed(t.items(), t).map((b) => b.kind)).toEqual(["tools", "item"]);
    // Seen without opening the group.
    const live = root.querySelector<HTMLDetailsElement>("details.thinking")!;
    expect(live.closest(".tool-group")).toBeNull();
    expect(live.querySelector("summary")!.textContent).toBe("Thinking · Checking");

    t.apply(
      ev("event.agent.thinking", (e) => {
        e.seq = 3;
        e.payload.text = "Checking.";
      }),
    );
    draw();
    // Stored, a row of the group of the calls before it, as any stored thinking.
    expect(buildFeed(t.items(), t).map((b) => b.kind)).toEqual(["tools"]);
    expect(root.querySelector("details.thinking")).toBeNull();
    act(() => root.querySelector<HTMLButtonElement>(".tool-group-head")!.click());
    expect(root.querySelector(".tool-group-body details.thinking .thinking-text")!.textContent).toBe("Checking.");
  });

  const ends: [string, (t: Timeline) => void][] = [
    [
      "the turn's end",
      (t) =>
        t.apply(
          ev("event.status", (e) => {
            e.seq = 2;
            e.payload = { state: "idle" };
          }),
        ),
    ],
    ["an answer stored", (t) => t.apply(ev("event.agent.message", withSeq(2)))],
    ["a dropped socket", (t) => t.reset()],
    // The runtime abandoned the attempt that wrote it (SPEC §4).
    ["a reset", (t) => t.apply(ev("event.agent.delta.reset"))],
    // The runtime took back what it was writing: no stored thinking ends it then.
    ["a retraction", (t) => t.apply(ev("event.retraction"))],
  ];
  for (const [what, end] of ends) {
    it(`ends with ${what}`, () => {
      const t = new Timeline();
      t.apply(ev("event.user.message", withSeq(1)));
      t.apply(thinking("Checking"));
      end(t);
      expect(t.thinking).toBeNull();
      expect(t.items().some((i) => i.kind === "event" && i.event.type === "agent.delta")).toBe(false);
    });
  }
});

describe("delivery after a dropped socket", () => {
  const input = (
    delivery: "pending" | "delivered",
    delivered_at?: string,
  ): HqcEvent =>
    ({
      event_id: "01a174c0-5a5d-7453-8440-44f0017983f2",
      session_id: "01a174be-822b-7589-b823-c8ed23a221a8",
      seq: 7,
      ts: "2026-10-25T20:28:45Z",
      author: { kind: "user", id: "u" },
      type: "user.message",
      payload: { text: "run the tests" },
      delivery,
      delivered_at,
    }) as HqcEvent;
  const status = (t: Timeline) => t.events().find((e) => e.seq === 7)?.delivery;

  it("a copy the hub sends again updates the delivery the reply left pending", () => {
    const t = new Timeline();
    t.applyReply(input("pending"));
    // The reply does not move lastSeq: the resubscription replays seq 7 anyway.
    expect(t.resumeSeq()).toBe(0);
    t.apply(input("delivered", "2026-10-25T20:28:45.3Z"));
    expect(status(t)).toBe("delivered");
    expect(t.resumeSeq()).toBe(7);
  });

  it("a late reply to POST /input does not undo a newer delivery", () => {
    const t = new Timeline();
    t.apply(input("delivered", "2026-10-25T20:28:45.3Z"));
    t.applyReply(input("pending"));
    expect(status(t)).toBe("delivered");
  });

  it("resubscribes from before an input still pending, even when later events arrived", () => {
    const t = new Timeline();
    t.apply(input("pending"));
    t.apply({
      ...input("pending"),
      event_id: "x",
      seq: 9,
      type: "status",
      payload: { state: "idle" },
      delivery: undefined,
    } as HqcEvent);
    expect(t.lastSeq).toBe(9);
    expect(t.resumeSeq()).toBe(6);
  });
});

describe("an input that came while the agent's text was being written (SPEC §6)", () => {
  const input = (seq: number, during: boolean) =>
    ev("event.user.message", (e) => {
      e.seq = seq;
      e.event_id = `0199f3a2-7c10-7000-8000-00000000020${seq}`;
      if (during) e.payload.during_text = true;
    });
  const answer = (seq: number, inReplyTo?: string) =>
    ev("event.agent.message", (e) => {
      e.seq = seq;
      e.payload = { text: "Done", final: true };
      if (inReplyTo) e.payload.in_reply_to = inReplyTo;
    });
  const order = (t: Timeline) => t.items().map((i) => i.key);
  const thought = (seq: number) => ev("event.agent.thinking", withSeq(seq));

  it("stands under the answer stored after it", () => {
    const t = new Timeline();
    t.applyAll([input(1, true), answer(2)]);
    expect(order(t)).toEqual(["e2", "e1"]);
  });

  it("only the marked inputs right before the answer go under it", () => {
    const t = new Timeline();
    t.applyAll([input(1, false), input(2, true), answer(3)]);
    expect(order(t)).toEqual(["e1", "e3", "e2"]);
  });

  it("the answer never goes above the input it answers", () => {
    const t = new Timeline();
    const u = input(1, true);
    t.applyAll([u, answer(2, u.event_id)]);
    expect(order(t)).toEqual(["e1", "e2"]);
  });

  it("an answer goes only above the inputs after the answer before it", () => {
    const t = new Timeline();
    t.applyAll([answer(1), input(2, true), answer(3), answer(4)]);
    expect(order(t)).toEqual(["e1", "e3", "e2", "e4"]);
  });

  it("stands under the thinking stored after it, and the answer after that stays under it", () => {
    const t = new Timeline();
    // Taken into the turn: one that waits for a turn of its own stands after the turn instead.
    const into = input(2, true);
    (into.payload as { joined?: boolean }).joined = true;
    t.applyAll([input(1, false), into, thought(3), answer(4)]);
    expect(order(t)).toEqual(["e1", "e3", "e2", "e4"]);
  });

  it("stands under the thinking being written, before and after it is stored", () => {
    const t = new Timeline();
    t.apply(input(1, false));
    t.apply(ev("event.agent.delta", (e) => (e.payload = { text: "Checking", thinking: true })));
    t.apply(input(2, true));
    expect(order(t)).toEqual(["e1", "t1", "e2"]);
    t.apply(thought(3));
    expect(order(t)).toEqual(["e1", "t1", "e2"]);
    const [, stored] = t.items();
    expect(stored.kind === "event" && stored.event.seq).toBe(3);
  });

  it("a final answer keeps its usage line in its block", () => {
    const t = new Timeline();
    t.applyAll([input(1, true), answer(2), ev("event.usage", withSeq(3))]);
    const blocks = buildFeed(t.items(), t);
    expect(blocks.map((b) => b.kind)).toEqual(["agent", "item"]);
    const [first] = blocks;
    expect(first.kind === "agent" ? first.usage?.seq : undefined).toBe(3);
  });

  it("a final answer takes the line that says how its turn ended along", () => {
    const t = new Timeline();
    const end = (code: string, seq = 4) =>
      ev("event.system", (e) => {
        e.seq = seq;
        e.payload = { code, text: code === "turn_interrupted" ? "Interrupted" : "Turn ended early: max_turns" };
      });
    for (const code of ["turn_interrupted", "turn_ended_early"]) {
      const t = new Timeline();
      t.applyAll([input(1, true), answer(2), ev("event.usage", withSeq(3)), end(code)]);
      expect(order(t)).toEqual(["e2", "e3", "e4", "e1"]);
      // A turn that ends on an API error or an interrupt mid-reply has no `usage` line.
      const bare = new Timeline();
      bare.applyAll([input(1, true), answer(2), end(code, 3)]);
      expect(order(bare)).toEqual(["e2", "e3", "e1"]);
    }
    t.applyAll([input(1, true), answer(2), ev("event.usage", withSeq(3)), end("worker_log")]);
    expect(order(t)).toEqual(["e2", "e3", "e1", "e4"]);
  });
});

describe("an input that waits for a turn of its own (SPEC §6, §17.2)", () => {
  const at = (seq: number) =>
    new Date(Date.parse("2026-10-22T10:00:00Z") + seq * 1000).toISOString();
  const id = (seq: number) =>
    `0199f3a2-7c10-7000-8000-${String(seq).padStart(12, "0")}`;
  /** The user's message stored at `seq`: delivered at `seq` `delivered` (the worker's ack),
   *  else pending. */
  const input = (seq: number, delivered?: number, patch: (e: any) => void = () => {}) =>
    ev("event.user.message", (e) => {
      e.seq = seq;
      e.event_id = id(seq);
      e.ts = at(seq);
      e.payload = { text: `input ${seq}`, artifact_ids: [] };
      if (delivered === undefined) {
        e.delivery = "pending";
        delete e.delivered_at;
      } else e.delivered_at = at(delivered);
      patch(e);
    });
  const final = (seq: number, inReplyTo: number) =>
    ev("event.agent.message", (e) => {
      e.seq = seq;
      e.ts = at(seq);
      e.payload = { text: `answer to ${inReplyTo}`, final: true, in_reply_to: id(inReplyTo) };
    });
  const idle = (seq: number, inReplyTo: number) =>
    ev("event.status", (e) => {
      e.seq = seq;
      e.ts = at(seq);
      e.payload = { state: "idle", in_reply_to: id(inReplyTo) };
    });
  const order = (t: Timeline) => t.items().map((i) => i.key);
  const time = (seq: number) =>
    [...root.querySelectorAll(".msg-user")]
      .find((m) => m.textContent?.includes(`input ${seq}`))!
      .querySelector(".msg-time")!.textContent;

  it("stands at the bottom while the turn runs, queued", () => {
    const t = new Timeline();
    t.applyAll([
      input(1, 1.2),
      call(2, "a", "Bash", { command: "make" }),
      input(3),
      result(4, "a"),
      call(5, "b", "Bash", { command: "make test" }),
    ]);
    expect(order(t)).toEqual(["e1", "e2", "e4", "e5", "e3"]);
    show(t);
    expect(time(3)).toBe(`${formatTime(at(3))} · queued`);
  });

  it("delivered, stands after the turn it waited through and before the work of its own", () => {
    const t = new Timeline();
    t.applyAll([
      input(1, 1.2),
      call(2, "a", "Bash", { command: "make" }),
      input(3),
      result(4, "a"),
      final(5, 1),
      ev("event.usage", withSeq(6)),
      idle(7, 1),
    ]);
    expect(order(t)).toEqual(["e1", "e2", "e4", "e5", "e6", "e7", "e3"]);
    t.applyDelivery({
      session_id: "s",
      event_id: id(3),
      seq: 3,
      delivery: "delivered",
      delivered_at: at(7.1),
    });
    t.applyAll([call(8, "b", "Read", { file_path: "a.rs" }), result(9, "b"), final(10, 3)]);
    expect(order(t)).toEqual(["e1", "e2", "e4", "e5", "e6", "e7", "e3", "e8", "e9", "e10"]);
    show(t);
    expect(time(3)).toBe(formatTime(at(3)));
    // The answer it waited through keeps its usage line.
    expect(root.querySelectorAll(".msg-final > .meta .usage")).toHaveLength(1);
  });

  it("one whose turn has no work (a slash command) stands before the end of its own turn", () => {
    const t = new Timeline();
    t.applyAll([
      input(1, 1.2),
      call(2, "a", "Bash", { command: "make" }),
      input(3, 6.1, (e) => (e.payload.text = "/cost")),
      result(4, "a"),
      final(5, 1),
      idle(6, 1),
      idle(7, 3),
      input(8, 8.1),
    ]);
    expect(order(t)).toEqual(["e1", "e2", "e4", "e5", "e6", "e3", "e7", "e8"]);
  });

  it("inputs waiting one behind another take the turns in order", () => {
    const t = new Timeline();
    t.applyAll([
      input(1, 1.2),
      call(2, "a", "Bash", { command: "make" }),
      input(3, 6.1),
      input(4),
      result(5, "a"),
      final(6, 1),
      call(7, "b", "Bash", { command: "ls" }),
    ]);
    expect(order(t)).toEqual(["e1", "e2", "e5", "e6", "e3", "e7", "e4"]);
    const queued = t.items().filter((i) => i.kind === "event" && i.queued);
    expect(queued.map((i) => i.key)).toEqual(["e3", "e4"]);
  });

  it("a joined input behind a waiting one stays behind it: two inputs carried past a final answer", () => {
    // Both written during the thinking of a turn with no calls (DrZlo13/hqc#42): the CLI runs
    // the first as the next turn, the second joins that turn.
    const t = new Timeline();
    t.applyAll([
      input(1, 1.2),
      ev("event.agent.thinking", (e) => ((e.seq = 2), (e.ts = at(2)))),
      input(3, 6.1),
      input(4, 7.1, (e) => (e.payload.joined = true)),
      final(5, 1),
      idle(6, 1),
      ev("event.agent.thinking", (e) => ((e.seq = 7), (e.ts = at(7)))),
      final(8, 4),
      idle(9, 4),
    ]);
    expect(order(t)).toEqual(["e1", "e2", "e5", "e6", "e3", "e4", "e7", "e8", "e9"]);
  });

  it("a joined input that went past a waiting one stands at its own place", () => {
    // X waited for a turn of its own and was delivered at 7.1; P1 joined the running turn at 4.5.
    const t = new Timeline();
    t.applyAll([
      input(1, 1.2),
      ev("event.agent.thinking", (e) => ((e.seq = 2), (e.ts = at(2)))),
      input(3, 7.1),
      input(4, 4.5, (e) => (e.payload.joined = true)),
      final(5, 1),
      idle(6, 1),
      ev("event.agent.thinking", (e) => ((e.seq = 7), (e.ts = at(7)))),
      final(8, 3),
    ]);
    expect(order(t)).toEqual(["e1", "e2", "e4", "e5", "e6", "e3", "e7", "e8"]);
  });

  it("a joined input stands behind the last waiting input delivered before it, not at the end of them", () => {
    // K waits (delivered 7.1), then X (delivered 12.1); e joined at 8.5, in K's turn, past X.
    const t = new Timeline();
    t.applyAll([
      input(1, 1.2),
      ev("event.agent.thinking", (e) => ((e.seq = 2), (e.ts = at(2)))),
      input(3, 7.1),
      input(4, 12.1),
      input(5, 8.5, (e) => (e.payload.joined = true)),
      final(6, 1),
      idle(7, 1),
      ev("event.agent.thinking", (e) => ((e.seq = 8), (e.ts = at(8)))),
      final(9, 3),
      idle(10, 3),
      ev("event.agent.thinking", (e) => ((e.seq = 11), (e.ts = at(11)))),
      final(12, 4),
    ]);
    expect(order(t)).toEqual(["e1", "e2", "e6", "e7", "e3", "e5", "e8", "e9", "e10", "e4", "e11", "e12"]);
  });

  it("a send of another session that joined the turn stands behind the waiting input like a joined message", () => {
    const t = new Timeline();
    t.applyAll([
      input(1, 1.2),
      ev("event.agent.thinking", (e) => ((e.seq = 2), (e.ts = at(2)))),
      input(3, 6.1),
      ev("event.peer.message", (e) => {
        e.seq = 4;
        e.event_id = id(4);
        e.ts = at(4);
        e.delivery = "delivered";
        e.delivered_at = at(7.1);
        e.payload = { from_session_id: id(99), text: "FYI", mode: "send", joined: true } as any;
      }),
      final(5, 1),
      idle(6, 1),
      ev("event.agent.thinking", (e) => ((e.seq = 7), (e.ts = at(7)))),
      final(8, 3),
      idle(9, 3),
    ]);
    expect(order(t)).toEqual(["e1", "e2", "e5", "e6", "e3", "e4", "e7", "e8", "e9"]);
  });

  it("a joined input behind a waiting one stays behind it: inputs during a turn another session's question started", () => {
    const t = new Timeline();
    t.applyAll([
      ev("event.peer.message", (e) => {
        e.seq = 1;
        e.event_id = id(1);
        e.ts = at(1);
        e.delivery = "delivered";
        e.delivered_at = at(1.2);
      }),
      call(2, "a", "Bash", { command: "make" }),
      input(3, 8.1),
      input(4, 10.1, (e) => (e.payload.joined = true)),
      result(5, "a"),
      final(6, 1),
      ev("event.usage", withSeq(7)),
      idle(8, 1),
      call(9, "b", "Bash", { command: "ls" }),
      result(10, "b"),
      final(11, 3),
      idle(12, 3),
    ]);
    expect(order(t)).toEqual(["e1", "e2", "e5", "e6", "e7", "e8", "e3", "e4", "e9", "e10", "e11", "e12"]);
  });

  it("a /compact typed during a turn stands above the compaction it started", () => {
    const t = new Timeline();
    t.applyAll([
      input(1, 1.2),
      call(2, "a", "Bash", { command: "make" }),
      input(3, 7.1, (e) => (e.payload.text = "/compact")),
      result(4, "a"),
      final(5, 1),
      ev("event.usage", withSeq(6)),
      idle(7, 1),
      ev("event.system", (e) => {
        e.seq = 8;
        e.payload = { code: "context_compacted", text: "Summary of the conversation" };
      }),
      ev("event.usage", (e) => ((e.seq = 9), (e.payload = { input_tokens: 0, output_tokens: 0, cost_usd: 0.0014 }))),
      idle(10, 3),
    ]);
    expect(order(t)).toEqual(["e1", "e2", "e4", "e5", "e6", "e7", "e3", "e8", "e9", "e10"]);
  });

  it("a turn lost right after it was taken ends the waiting input's own turn", () => {
    const t = new Timeline();
    t.applyAll([
      input(1, 1.2),
      call(2, "a", "Bash", { command: "make" }),
      input(3, 6.1),
      result(4, "a"),
      final(5, 1),
      idle(6, 1),
      ev("event.system", (e) => {
        e.seq = 7;
        e.payload = { code: "turn_lost", text: "The agent process stopped during a turn; the turn was lost" };
      }),
      input(8, 8.1),
      call(9, "b", "Bash", { command: "ls" }),
      final(10, 8),
    ]);
    expect(order(t)).toEqual(["e1", "e2", "e4", "e5", "e6", "e3", "e7", "e8", "e9", "e10"]);
  });

  it("one written during the text waits behind the one queued before it, live and once the text is stored", () => {
    const t = new Timeline();
    t.applyAll([
      input(1, 1.2),
      call(2, "a", "Bash", { command: "make" }),
      input(3),
      result(4, "a"),
      input(5, undefined, (e) => (e.payload.during_text = true)),
    ]);
    expect(order(t)).toEqual(["e1", "e2", "e4", "e3", "e5"]);
    t.applyAll([final(6, 1), ev("event.usage", withSeq(7))]);
    expect(order(t)).toEqual(["e1", "e2", "e4", "e6", "e7", "e3", "e5"]);
  });

  it("one written during the agent's thinking or text and taken after that turn stands after it", () => {
    // A real session's events: 180 came while the thinking of 179's turn streamed, the agent
    // took it five seconds after that turn ended and answered it in a turn of its own.
    const thinking = (seq: number) => ev("event.agent.thinking", (e) => ((e.seq = seq), (e.ts = at(seq))));
    const t = new Timeline();
    t.applyAll([
      input(179, 179.5),
      input(180, undefined, (e) => (e.payload.during_text = true)),
      thinking(181),
      final(182, 179),
      ev("event.usage", withSeq(183)),
      idle(184, 179),
    ]);
    expect(order(t)).toEqual(["e179", "e181", "e182", "e183", "e184", "e180"]);
    show(t);
    expect(time(180)).toBe(`${formatTime(at(180))} · queued`);
    t.applyDelivery({
      session_id: "s",
      event_id: id(180),
      seq: 180,
      delivery: "delivered",
      delivered_at: at(184.5),
    });
    t.applyAll([thinking(185), final(186, 180), ev("event.usage", withSeq(187)), idle(188, 180)]);
    expect(order(t)).toEqual(["e179", "e181", "e182", "e183", "e184", "e180", "e185", "e186", "e187", "e188"]);
  });

  it("the text that starts the own turn of one written during the text stays under it", () => {
    const t = new Timeline();
    t.applyAll([
      input(1, 1.2),
      call(2, "a", "Bash", { command: "make" }),
      result(3, "a"),
      input(4, 6.1, (e) => (e.payload.during_text = true)),
      final(5, 1),
      idle(6, 1),
      ev("event.agent.message", (e) => {
        e.seq = 7;
        e.ts = at(7);
        e.payload = { text: "Let me look.", final: false };
      }),
      call(8, "b", "Read", { file_path: "a.rs" }),
      result(9, "b"),
      final(10, 4),
      idle(11, 4),
    ]);
    expect(order(t)).toEqual(["e1", "e2", "e3", "e5", "e6", "e4", "e7", "e8", "e9", "e10", "e11"]);
  });

  it("a question from another session waits too, and says so", () => {
    const t = new Timeline();
    t.applyAll([
      input(1, 1.2),
      call(2, "a", "Bash", { command: "make" }),
      ev("event.peer.message", withSeq(3)),
    ]);
    expect(order(t)).toEqual(["e1", "e2", "e3"]);
    t.apply(result(4, "a"));
    expect(order(t)).toEqual(["e1", "e2", "e4", "e3"]);
    show(t);
    expect(root.querySelector(".relay-when")!.textContent).toBe(
      `${formatTime("2026-10-22T10:00:00Z")} · queued`,
    );
  });

  it("stands under the thinking being written, as do one written during it and a message not stored yet", () => {
    const t = new Timeline();
    t.applyAll([input(1, 1.2), call(2, "a", "Bash", { command: "make" }), input(3), result(4, "a")]);
    t.apply(ev("event.agent.delta", (e) => (e.payload = { text: "Checking", thinking: true })));
    t.apply(input(5, undefined, (e) => (e.payload.during_text = true)));
    // The inputs above carry the example's `op-1`.
    t.addPending("op-2", "typed while it thinks");
    expect(order(t)).toEqual(["e1", "e2", "e4", "t1", "e3", "e5", "p-op-2"]);
  });

  it("keeps its place: sent while no turn ran, joined to the turn, or mirrored", () => {
    const t = new Timeline();
    t.applyAll([input(1, 1.2), final(2, 1), input(3)]);
    expect(order(t)).toEqual(["e1", "e2", "e3"]);
    show(t);
    expect(time(3)).toBe(`${formatTime(at(3))} · awaiting delivery`);
    const joined = new Timeline();
    joined.applyAll([
      input(1, 1.2),
      call(2, "a", "Bash", { command: "make" }),
      input(3, 4.1, (e) => (e.payload.joined = true)),
      result(4, "a"),
      final(5, 1),
    ]);
    expect(order(joined)).toEqual(["e1", "e2", "e3", "e4", "e5"]);
    // A message typed in VS Code during a turn, mirrored where the CLI took it (SPEC §9.1).
    const mirrored = new Timeline();
    mirrored.applyAll([
      input(1, 1),
      call(2, "a", "Bash", { command: "make" }),
      result(3, "a"),
      input(4, 4),
      final(5, 1),
    ]);
    expect(order(mirrored)).toEqual(["e1", "e2", "e3", "e4", "e5"]);
  });
});

describe("unknown and invalid events", () => {
  it("renders an unknown type as a collapsed card", () => {
    const t = new Timeline();
    t.apply(ev("event.unknown"));
    show(t);
    const card = root.querySelector("details.card-unknown")!;
    expect(card).not.toBeNull();
    expect(card.hasAttribute("open")).toBe(false);
    expect(card.querySelector("summary")!.textContent).toContain(
      "Unknown event",
    );
    expect(card.querySelector("summary")!.textContent).toContain("plan.update");
  });

  it("renders a known type with a broken payload as an invalid card", () => {
    const t = new Timeline();
    const bad = raw("event.tool.call");
    delete bad.payload.tool;
    t.addInvalid(bad, "tool: expected a string");
    show(t);
    expect(root.querySelector(".card-invalid summary")!.textContent).toContain(
      "tool.call",
    );
  });
});

describe("approval.request (T26 card)", () => {
  it("has exactly one button per option, captioned by canonical", () => {
    const t = new Timeline();
    t.apply(ev("event.approval.request"));
    show(t);
    const options = raw("event.approval.request").payload.options;
    const buttons = optionButtons();
    expect(buttons.map((b) => b.getAttribute("data-option")).sort()).toEqual(
      options.map((o: any) => o.id).sort(),
    );
    for (const o of options) {
      const b = buttons.find((x) => x.getAttribute("data-option") === o.id)!;
      expect(b.textContent).toContain(
        CANONICAL_LABELS[o.canonical as keyof typeof CANONICAL_LABELS],
      );
    }
    // "Allow" first; the key hints only on the card the keys answer.
    expect(buttons[0].getAttribute("data-canonical")).toBe("once");
    expect(root.querySelector(".kbd")).toBeNull();
  });

  it("shows ⏎ on Allow and Esc on Deny when the keys answer this card", () => {
    const t = new Timeline();
    t.apply(ev("event.approval.request"));
    show(t, { hotkeyRequest: raw("event.approval.request").payload.request_id });
    expect(optionButtons()[0].querySelector(".kbd")!.textContent).toBe("⏎");
    expect(root.querySelector('[data-option="deny"] .kbd')!.textContent).toBe(
      "Esc",
    );
  });

  it("says what is asked, for how long, and shows the command", () => {
    const t = new Timeline();
    t.apply(ev("event.approval.request"));
    show(t);
    expect(root.querySelector(".perm-title")!.textContent).toBe(
      "The agent wants to run a command",
    );
    expect(root.querySelector(".perm-sub")!.textContent).toBe("Bash");
    expect(root.querySelector(".perm-what")!.textContent).toBe("$ cargo test");
    // ts 10:00:00, now 10:30:00.
    expect(root.querySelector(".perm-timer")!.textContent).toBe(
      "waiting 30:00",
    );
  });

  it("names the subagent that asks", () => {
    const t = new Timeline();
    t.apply(ev("event.approval.request.subagent"));
    show(t);
    expect(root.querySelector(".perm-title")!.textContent).toBe(
      "Subagent “Fix schematic power nets in import” wants to run a command",
    );
  });

  it("names the subagent on a phone's sheet too", () => {
    const sheet = (e: HqcEvent) => {
      const t = new Timeline();
      t.apply(e);
      root = document.createElement("div");
      document.body.appendChild(root);
      const actions: ChatActions = {
        timeline: t,
        approve: vi.fn(async () => {}),
        answer: vi.fn(async () => {}),
        resend: vi.fn(),
        discard: vi.fn(),
        openArtifact: vi.fn(),
      };
      act(() => render(<PermissionBlock event={e as EventOf<"approval.request">} actions={actions} sheet />, root));
      return root.querySelector(".perm-sheet .perm-title")!.textContent;
    };
    expect(sheet(ev("event.approval.request.subagent"))).toBe(
      "Subagent “Fix schematic power nets in import”: run the command?",
    );
    render(null, root);
    expect(sheet(ev("event.approval.request"))).toBe("Run the command?");
  });

  it("explains policy_hint allow and a path outside the workspace, nothing otherwise", () => {
    const req = (seq: number, id: string, description: string, hint?: string) =>
      ev("event.approval.request", (e) => {
        e.seq = seq;
        e.event_id = `0199f3a2-7c10-7000-8000-0000000001b${seq}`;
        e.payload.request_id = id;
        e.payload.description = description;
        if (hint) e.payload.policy_hint = hint;
        else delete e.payload.policy_hint;
      });
    const t = new Timeline();
    t.apply(req(1, "r1", "List root directory\nls /", "allow"));
    t.apply(req(2, "r2", "cargo test", "allow"));
    t.apply(req(3, "r3", "Show a file\ncat ~/notes.txt"));
    t.apply(req(4, "r4", "ls src"));
    show(t, { workspaceRoot: "/Users/me/work/hqc" });
    const cards = [...root.querySelectorAll(".perm")];
    expect(cards[0].querySelector(".perm-sub")!.textContent).toBe(
      "Bash · List root directory",
    );
    expect(cards[0].querySelector(".perm-what")!.textContent).toBe("$ ls /");
    expect(cards[0].querySelector(".perm-note")!.textContent).toBe(
      "A worker rule allows Bash, but Claude Code asked anyway: the path / is outside the working folder ~/work/hqc.",
    );
    expect(cards[1].querySelector(".perm-note")!.textContent).toBe(
      "A worker rule allows Bash, but Claude Code asked anyway.",
    );
    expect(cards[2].querySelector(".perm-note")!.textContent).toBe(
      "The path ~/notes.txt is outside the working folder ~/work/hqc.",
    );
    expect(cards[3].querySelector(".perm-note")).toBeNull();
  });

  it("uses the agent's label for an unknown canonical and keeps the agent's option set", () => {
    const t = new Timeline();
    t.apply(
      ev("event.approval.request", (e) => {
        e.payload.options = [
          {
            id: "proceed",
            label: "Proceed with edits",
            canonical: "allow_edits",
          },
          { id: "reject", label: "Reject", canonical: "deny" },
        ];
      }),
    );
    show(t);
    const buttons = optionButtons().map((b) => b.childNodes[0].textContent);
    expect(buttons).toEqual(["Proceed with edits", CANONICAL_LABELS.deny]);
  });

  it("sends the option id on click, then folds into a row that can show the request again", async () => {
    const t = new Timeline();
    t.apply(ev("event.approval.request", withSeq(1)));
    const { actions, draw } = show(t);
    const btn = root.querySelector<HTMLButtonElement>(
      'button[data-option="allow_session"]',
    )!;
    await act(async () => btn.click());
    expect(actions.approve).toHaveBeenCalledWith("req-1", "allow_session");

    t.apply(
      ev(
        "event.approval.response",
        (e) => ((e.seq = 2), (e.payload.option_id = "allow_session")),
      ),
    );
    draw();
    expect(optionButtons()).toHaveLength(0);
    expect(root.querySelector(".perm-card")).toBeNull();
    const row = root.querySelector(".perm-done-row")!;
    expect(row.textContent).toContain("$ cargo test");
    expect(row.textContent).toContain("· allowed for the rest of the session");
    await act(async () => buttonByText("Show the request again")!.click());
    expect(root.querySelector(".perm-card .outcome")!.textContent).toBe(
      "Answer: allowed for the rest of the session",
    );
    expect(optionButtons()).toHaveLength(0);
  });

  it("an answered request's row names its tool and argument as the call's row does; a card names an MCP tool by name and server", () => {
    const request = (seq: number, tool: string) =>
      ev("event.approval.request", (e) => {
        e.seq = seq;
        e.payload = { ...e.payload, request_id: `req-${seq}`, tool, description: tool, rule_suggestions: [] };
      });
    const allowed = (seq: number, request_id: string) =>
      ev("event.approval.response", (e) => ((e.seq = seq), (e.payload = { ...e.payload, request_id, option_id: "allow" })));
    const t = new Timeline();
    t.apply(call(1, "a", "mcp__probe-bench__move", { x: 90.7, y: 80.9 }));
    t.apply(request(2, "mcp__probe-bench__move"));
    t.apply(allowed(3, "req-2"));
    t.apply(call(4, "b", "Read", { file_path: "/w/hqc/src/a.rs" }));
    t.apply(request(5, "Read"));
    t.apply(allowed(6, "req-5"));
    t.apply(call(7, "c", "mcp__probe-bench__home", { axis: "z" }));
    t.apply(request(8, "mcp__probe-bench__home"));
    show(t, { workspaceRoot: "/w/hqc" });
    expect([...root.querySelectorAll(".perm-done-what")].map((s) => s.textContent)).toEqual([
      "move (probe-bench) x: 90.7, y: 80.9",
      "Read src/a.rs",
    ]);
    expect(root.querySelector(".perm-card .perm-title")!.textContent).toBe("The agent wants to use home (probe-bench)");
    expect(root.querySelector(".perm-card .perm-what pre")!.textContent).toBe('axis: "z"');
  });

  it("removes the buttons on approval.cancel", () => {
    const t = new Timeline();
    t.apply(ev("event.approval.request", withSeq(1)));
    t.apply(ev("event.approval.cancel", withSeq(2)));
    show(t);
    expect(optionButtons()).toHaveLength(0);
    expect(
      root.querySelector(".perm-done-row .outcome")!.textContent,
    ).toContain("worker restarted");
  });

  it("shows the error if the hub refuses the answer", async () => {
    const t = new Timeline();
    t.apply(ev("event.approval.request"));
    show(t, {
      approve: async () => Promise.reject(new Error("request is not open")),
    });
    await act(async () => optionButtons()[0].click());
    await act(async () => {});
    expect(root.querySelector(".perm .error-text")!.textContent).toContain(
      "request is not open",
    );
  });

  it("Enter allows once and Esc denies while the card has the keys and no field has the focus", async () => {
    const t = new Timeline();
    t.apply(ev("event.approval.request", withSeq(1)));
    const { actions } = show(t, { hotkeyRequest: "req-1" });
    await key("Enter");
    expect(actions.approve).toHaveBeenLastCalledWith("req-1", "allow");
    await act(async () => {});
    await key("Escape");
    expect(actions.approve).toHaveBeenLastCalledWith("req-1", "deny");
    await act(async () => {});
    expect(actions.approve).toHaveBeenCalledTimes(2);

    // Typing somewhere: the keys belong to the field.
    const input = document.createElement("textarea");
    document.body.appendChild(input);
    input.focus();
    await key("Enter");
    await key("Escape");
    expect(actions.approve).toHaveBeenCalledTimes(2);
    input.remove();
  });

  it("a card without the keys ignores Enter and Esc", async () => {
    const t = new Timeline();
    t.apply(ev("event.approval.request", withSeq(1)));
    const { actions } = show(t);
    await key("Enter");
    await key("Escape");
    expect(actions.approve).not.toHaveBeenCalled();
  });

  it("answers with text instead: a denial with message, then shown as the user's message", async () => {
    const t = new Timeline();
    t.apply(ev("event.approval.request", withSeq(1)));
    const { actions, draw } = show(t, { hotkeyRequest: "req-1" });
    await act(async () => buttonByText("Reply with text instead")!.click());
    const area = root.querySelector<HTMLTextAreaElement>(
      ".perm-reply textarea",
    )!;
    expect(root.querySelector(".perm-reply-hint")!.textContent).toBe(
      "The reply denies cargo test and goes to the agent",
    );
    // The keys of the card do not fire while typing.
    area.focus();
    await key("Enter");
    expect(actions.approve).not.toHaveBeenCalled();
    await act(() => {
      area.value = "Build first, tests later";
      area.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await act(async () => buttonByText("Deny and send")!.click());
    expect(actions.approve).toHaveBeenCalledWith(
      "req-1",
      "deny",
      "Build first, tests later",
    );

    t.apply(
      ev(
        "event.approval.response.message",
        (e) => ((e.seq = 2), (e.payload.message = "Build first, tests later")),
      ),
    );
    draw();
    expect(root.querySelector(".perm-done-row .outcome")!.textContent).toBe(
      "· denied, replied with text",
    );
    const msg = root.querySelector(".msg-user.msg-answer")!;
    expect(msg.querySelector(".msg-bubble .msg-text")!.textContent).toBe(
      "Build first, tests later",
    );
    // A mark before the time, the words its tooltip (feedlook.test.tsx, "joined the turn").
    const mark = msg.querySelector(".msg-bubble > .msg-time > .msg-note")!;
    expect(mark.getAttribute("title")).toBe("Instead of the request");
    expect(mark.textContent).toBe("");
  });

  it('"Back to the request" gives the buttons back', async () => {
    const t = new Timeline();
    t.apply(ev("event.approval.request", withSeq(1)));
    show(t);
    await act(async () => buttonByText("Reply with text instead")!.click());
    expect(optionButtons()).toHaveLength(0);
    await act(async () => buttonByText("Back to the request")!.click());
    expect(optionButtons()).toHaveLength(5);
  });

  it("takes over the tool.call right before it: one card, the command from the call", () => {
    const t = new Timeline();
    t.apply(
      call(1, "c1", "Bash", {
        command: "ls /",
        description: "List root directory",
      }),
    );
    t.apply(
      ev("event.approval.request", (e) => {
        e.seq = 2;
        e.payload.description = "List root directory\nls /";
      }),
    );
    show(t);
    expect(root.querySelector(".tool-group")).toBeNull();
    expect(root.querySelector(".perm-what")!.textContent).toBe("$ ls /");
  });
});

describe("clarify.request", () => {
  it("renders questions with choices and submits answers", async () => {
    const t = new Timeline();
    t.apply(ev("event.clarify.request"));
    const { actions } = show(t);
    const qs = root.querySelectorAll(".card-clarify fieldset");
    expect(qs.length).toBe(3);
    // The options and Other.
    expect(qs[0].querySelectorAll("input[type=radio]").length).toBe(3);
    expect(qs[1].querySelectorAll("input[type=checkbox]").length).toBe(4);
    expect(qs[2].querySelectorAll("input[type=text]").length).toBe(1);

    const click = (sel: string) =>
      act(() => root.querySelector<HTMLInputElement>(sel)!.click());
    await click('input[value="Worker"]');
    await click('input[value="clippy"]');
    await click('input[value="test"]');
    const text = qs[2].querySelector<HTMLInputElement>("input[type=text]")!;
    await act(() => {
      text.value = "no";
      text.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await act(async () =>
      root
        .querySelector<HTMLButtonElement>(".card-clarify button[type=submit]")!
        .click(),
    );
    expect(actions.answer).toHaveBeenCalledWith("req-2", {
      q1: "Worker",
      q2: ["clippy", "test"],
      q3: "no",
    });
  });

  it("Answer waits for an answer to every question", async () => {
    const t = new Timeline();
    t.apply(ev("event.clarify.request"));
    const { actions } = show(t);
    const qs = root.querySelectorAll(".card-clarify fieldset");
    const submit = () =>
      root.querySelector<HTMLButtonElement>(".card-clarify button[type=submit]")!;
    const type = (value: string) =>
      act(() => {
        const text = qs[2].querySelector<HTMLInputElement>("input[type=text]")!;
        text.value = value;
        text.dispatchEvent(new Event("input", { bubbles: true }));
      });
    expect(submit().disabled).toBe(true);
    await act(() => root.querySelector<HTMLInputElement>('input[value="Worker"]')!.click());
    await type("no");
    // The multi question has nothing checked yet.
    expect(submit().disabled).toBe(true);
    await act(async () => {
      root.querySelector("form.card-clarify")!.dispatchEvent(
        new Event("submit", { bubbles: true, cancelable: true }),
      );
    });
    expect(actions.answer).not.toHaveBeenCalled();
    const fmt = () => qs[1].querySelector<HTMLInputElement>('input[value="fmt"]')!;
    await act(() => fmt().click());
    expect(submit().disabled).toBe(false);
    // A label checked and unchecked again leaves the question unanswered.
    await act(() => fmt().click());
    expect(submit().disabled).toBe(true);
    // Other chosen with nothing typed is an answer.
    await act(() =>
      qs[1].querySelector<HTMLInputElement>(".choice-other input[type=checkbox]")!.click(),
    );
    expect(submit().disabled).toBe(false);
    // A text of spaces is not.
    await type("  ");
    expect(submit().disabled).toBe(true);
  });

  it("names the subagent that asks, else the agent", () => {
    const t = new Timeline();
    t.apply(ev("event.clarify.request", withSeq(1)));
    t.apply(
      ev("event.clarify.request", (e) => {
        e.seq = 2;
        e.event_id = "0199f3a2-7c10-7000-8000-0000000001c2";
        e.payload.request_id = "req-9";
        e.payload.subagent = {
          task_id: "a52af2b436ded4cea",
          description: "Fix schematic power nets in import",
        };
      }),
    );
    show(t);
    expect(
      [...root.querySelectorAll(".card-clarify .card-title")].map((c) => c.textContent),
    ).toEqual(["Agent asks", "Subagent “Fix schematic power nets in import” asks"]);
  });

  it("closes after clarify.response", () => {
    const t = new Timeline();
    t.apply(ev("event.clarify.request", withSeq(1)));
    t.apply(ev("event.clarify.response", withSeq(2)));
    show(t);
    expect(root.querySelector(".card-clarify button")).toBeNull();
    expect(
      root.querySelector<HTMLInputElement>('input[value="Hub"]')!.checked,
    ).toBe(true);
  });

  // SPEC 0.161: the CLI no longer waits for the answer, so it goes as the user's message, in the
  // words of the VS Code tab for an earlier question (web:162062–162070).
  const cancelled = (reason: string, only?: number) => {
    const t = new Timeline();
    t.apply(
      ev("event.clarify.request", (e) => {
        e.seq = 1;
        if (only !== undefined) e.payload.questions = [e.payload.questions[only]];
      }),
    );
    t.apply(
      ev("event.clarify.cancel", (e) => {
        e.seq = 2;
        e.payload.reason = reason;
      }),
    );
    return t;
  };
  const submit = () =>
    act(async () =>
      root
        .querySelector<HTMLButtonElement>(".card-clarify button[type=submit]")!
        .click(),
    );
  const SEVERAL =
    'Answering your earlier questions:\n- "Which area should I focus on first?": Worker\n- "Which checks should run?": clippy, test\n- "Anything else?": no';
  const submitButton = () => root.querySelector<HTMLButtonElement>(".card-clarify button[type=submit]");
  const typeOther = (q: number, value: string) =>
    act(() => {
      const text = root.querySelectorAll(".card-clarify fieldset")[q].querySelector<HTMLInputElement>("input[type=text]")!;
      text.value = value;
      text.dispatchEvent(new Event("input", { bubbles: true }));
    });
  const answerAll = async () => {
    await act(() => root.querySelector<HTMLInputElement>('input[value="Worker"]')!.click());
    await act(() => root.querySelector<HTMLInputElement>('input[value="clippy"]')!.click());
    await act(() => root.querySelector<HTMLInputElement>('input[value="test"]')!.click());
    await typeOther(2, "no");
  };
  const outcomes = () => [...root.querySelectorAll(".card-clarify .outcome")].map((o) => o.textContent);
  const checked = (value: string) => root.querySelector<HTMLInputElement>(`input[value="${value}"]`)!.checked;
  // The chat's `send`: the message stands in the feed at once, not stored yet.
  const sending = (t: Timeline) => vi.fn((text: string) => t.addPending("op-1", text));
  const userMessage = (seq: number, text: string) =>
    ev("event.user.message", (e) => {
      e.seq = seq;
      e.payload = { text, artifact_ids: [] };
      delete e.client_op_id;
    });
  for (const reason of ["worker_restart", "stopped"]) {
    it(`a question cancelled with ${reason} is answered by a message`, async () => {
      const t = cancelled(reason);
      const send = sending(t);
      const { actions, draw } = show(t, { send });
      expect(root.querySelector(".card-clarify")!.classList.contains("is-open")).toBe(true);
      // Nothing chosen, nothing to send; as on an open card, Answer waits for every question.
      expect(submitButton()!.disabled).toBe(true);
      await act(() => root.querySelector<HTMLInputElement>('input[value="Worker"]')!.click());
      await act(() => root.querySelector<HTMLInputElement>('input[value="clippy"]')!.click());
      await act(() => root.querySelector<HTMLInputElement>('input[value="test"]')!.click());
      expect(submitButton()!.disabled).toBe(true);
      await act(async () => {
        root.querySelector("form.card-clarify")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
      });
      expect(send).not.toHaveBeenCalled();
      await typeOther(2, "no");
      expect(submitButton()!.disabled).toBe(false);
      await submit();
      expect(send).toHaveBeenCalledWith(SEVERAL);
      expect(actions.answer).not.toHaveBeenCalled();
      draw();
      expect(submitButton()).toBeNull();
      expect(outcomes()).toEqual(["Answers sent"]);
      expect([checked("Worker"), checked("clippy"), checked("test"), checked("Hub")]).toEqual([true, true, true, false]);
    });
  }

  it("a later message of the user closes it", () => {
    const t = cancelled("worker_restart");
    t.apply(userMessage(3, "Never mind, look at the hub."));
    show(t, { send: vi.fn() });
    expect(submitButton()).toBeNull();
    expect(root.querySelector(".card-clarify fieldset")!.hasAttribute("disabled")).toBe(true);
    expect(outcomes()).toEqual(["Cancelled: worker restarted"]);
  });

  it("another session's message after it closes it too", () => {
    const t = cancelled("stopped");
    t.apply(ev("event.peer.message", withSeq(3)));
    show(t, { send: vi.fn() });
    expect(submitButton()).toBeNull();
    expect(outcomes()).toEqual(["Cancelled: turn stopped"]);
  });

  it("opened again after the answer, it shows what was answered", () => {
    const t = cancelled("stopped");
    t.apply(userMessage(3, SEVERAL));
    show(t, { send: vi.fn() });
    expect(submitButton()).toBeNull();
    expect(outcomes()).toEqual(["Answers sent"]);
    expect([checked("Worker"), checked("clippy"), checked("test"), checked("Hub")]).toEqual([true, true, true, false]);
  });

  it("a send that failed and was discarded leaves it to be answered", async () => {
    const t = cancelled("worker_restart");
    const { draw } = show(t, { send: sending(t) });
    await answerAll();
    await submit();
    t.failPending("op-1", "offline");
    draw();
    expect(submitButton()).toBeNull();
    expect(outcomes()).toEqual(["Cancelled: worker restarted"]);
    t.dropPending("op-1");
    draw();
    expect(submitButton()).not.toBeNull();
  });

  it("the answer to one question goes in the tab's words for one", async () => {
    const send = vi.fn();
    show(cancelled("worker_restart", 2), { send });
    await typeOther(0, "no");
    await submit();
    expect(send).toHaveBeenCalledWith('Answering your earlier question "Anything else?": no');
  });

  it("a question that timed out stays closed", () => {
    show(cancelled("timeout"), { send: vi.fn() });
    expect(root.querySelector(".card-clarify button")).toBeNull();
    expect(root.querySelector(".card-clarify fieldset")!.hasAttribute("disabled")).toBe(true);
  });
});

describe("requests of the CLI (SPEC 0.180)", () => {
  it("names the tool as the runtime does and says why it asks", () => {
    const t = new Timeline();
    t.apply(ev("event.approval.request.human_only"));
    show(t);
    expect(root.querySelector(".perm-title")!.textContent).toBe(
      "The agent wants to use fetch (web)",
    );
    expect(root.querySelector(".perm-sub")!.textContent).toBe(
      // The CLI's `display_name` of an MCP tool ("Fetch") drops its server.
      "fetch (web) · https://example.com/report",
    );
    expect(root.querySelector(".perm-reason")!.textContent).toBe(
      "Permission rule 'mcp__web__fetch' from project settings requires approval",
    );
    // Only a person may answer: once and deny.
    expect(optionButtons().map((b) => b.getAttribute("data-option"))).toEqual([
      "once",
      "deny",
    ]);
    // Without the fields: the tool's own name, an MCP one with its server, no reason.
    const plain = new Timeline();
    plain.apply(
      ev("event.approval.request", (e) => {
        e.payload.tool = "mcp__web__fetch";
      }),
    );
    show(plain);
    expect(root.querySelector(".perm-title")!.textContent).toBe(
      "The agent wants to use fetch (web)",
    );
    expect(root.querySelector(".perm-reason")).toBeNull();
  });

  it("says a request the agent withdrew was withdrawn", () => {
    const t = new Timeline();
    t.apply(ev("event.approval.request", withSeq(1)));
    t.apply(
      ev("event.approval.cancel", (e) => {
        e.seq = 2;
        e.payload = { request_id: "req-1", reason: "withdrawn" };
      }),
    );
    t.apply(ev("event.clarify.request", withSeq(3)));
    t.apply(
      ev("event.clarify.cancel", (e) => {
        e.seq = 4;
        e.payload = { request_id: "req-2", reason: "withdrawn" };
      }),
    );
    show(t);
    expect(root.querySelector(".perm-done-outcome")!.textContent).toBe(
      "· cancelled: withdrawn",
    );
    expect(root.querySelector(".card-clarify .outcome")!.textContent).toBe(
      "Cancelled: withdrawn",
    );
  });

  it("asks an MCP server's form: the server, its text, no Other, Decline", async () => {
    const t = new Timeline();
    t.apply(ev("event.clarify.request.elicitation"));
    const { actions } = show(t);
    const card = root.querySelector(".card-clarify")!;
    expect(card.querySelector(".card-title")!.textContent).toBe("deploy asks");
    expect(card.querySelector(".clarify-message")!.textContent).toBe(
      "Where should the release go?",
    );
    expect(card.querySelector('input[aria-label="Other answer"]')).toBeNull();
    const qs = card.querySelectorAll("fieldset");
    expect(qs[2].querySelectorAll("input[type=radio]").length).toBe(2);
    await act(async () => buttonByText("Decline")!.click());
    expect(actions.answer).toHaveBeenLastCalledWith("req-4", {}, "decline");

    const again = show(t);
    await act(() => root.querySelector<HTMLInputElement>('input[value="us-east"]')!.click());
    await act(async () =>
      root.querySelector<HTMLButtonElement>(".card-clarify button[type=submit]")!.click(),
    );
    expect(again.actions.answer).toHaveBeenLastCalledWith("req-4", { "1": "us-east" }, "accept");
  });

  it("Answer of an MCP server's form waits for its required fields and an integer that reads as one", async () => {
    const t = new Timeline();
    t.apply(ev("event.clarify.request.elicitation"));
    const { actions } = show(t);
    const qs = root.querySelectorAll(".card-clarify fieldset");
    expect(qs[0].querySelector(".question-header")!.textContent).toBe("Region · required");
    expect(qs[1].querySelector(".question-header")).toBeNull();
    const submit = () =>
      root.querySelector<HTMLButtonElement>(".card-clarify button[type=submit]")!;
    const replicas = (value: string) =>
      act(() => {
        const text = qs[1].querySelector<HTMLInputElement>("input[type=text]")!;
        text.value = value;
        text.dispatchEvent(new Event("input", { bubbles: true }));
      });
    // The required region has no answer.
    expect(submit().disabled).toBe(true);
    await replicas("3");
    expect(submit().disabled).toBe(true);
    await act(() => root.querySelector<HTMLInputElement>('input[value="us-east"]')!.click());
    expect(submit().disabled).toBe(false);
    // An integer that does not read as one.
    await replicas("three");
    expect(submit().disabled).toBe(true);
    await act(async () => {
      root.querySelector("form.card-clarify")!.dispatchEvent(
        new Event("submit", { bubbles: true, cancelable: true }),
      );
    });
    expect(actions.answer).not.toHaveBeenCalled();
    // Past a safe integer the worker does not read it as one either.
    await replicas("99999999999999999999");
    expect(submit().disabled).toBe(true);
    await replicas("-3");
    expect(submit().disabled).toBe(false);
    // An optional field may stay empty.
    await replicas("");
    expect(submit().disabled).toBe(false);
  });

  it("shows an MCP server's link with Done, and a declined one", async () => {
    const t = new Timeline();
    t.apply(ev("event.clarify.request.elicitation_url", withSeq(1)));
    const { actions } = show(t);
    const a = root.querySelector<HTMLAnchorElement>(".card-clarify .clarify-link a")!;
    expect(a.getAttribute("href")).toBe("https://github.com/login/device");
    expect(root.querySelector(".card-clarify fieldset")).toBeNull();
    await act(async () => buttonByText("Done")!.click());
    expect(actions.answer).toHaveBeenLastCalledWith("req-5", {}, "accept");
    t.apply(
      ev("event.clarify.response", (e) => {
        e.seq = 2;
        e.payload = { request_id: "req-5", answers: {}, action: "accept" };
      }),
    );
    show(t);
    expect(root.querySelector(".card-clarify .outcome")!.textContent).toBe("Done");

    const declined = new Timeline();
    declined.apply(ev("event.clarify.request.elicitation_url", withSeq(1)));
    declined.apply(ev("event.clarify.response.decline", withSeq(2)));
    show(declined);
    expect(root.querySelector(".card-clarify .outcome")!.textContent).toBe("Declined");
    expect(buttonByText("Done")).toBeUndefined();

    // The label is the address the link opens, a look-alike host in its punycode form.
    const lookalike = new Timeline();
    lookalike.apply(
      ev("event.clarify.request.elicitation_url", (e) => {
        e.payload.url = "https://gith\u0430b.com/login";
      }),
    );
    show(lookalike);
    expect(root.querySelector(".card-clarify .clarify-link a")!.textContent).toBe(
      "https://xn--githb-7ve.com/login",
    );

    // A link that is no web page is shown as text.
    const odd = new Timeline();
    odd.apply(
      ev("event.clarify.request.elicitation_url", (e) => {
        e.payload.url = "javascript:alert(1)";
      }),
    );
    show(odd);
    expect(root.querySelector(".card-clarify .clarify-link a")).toBeNull();
    expect(root.querySelector(".card-clarify .clarify-link")!.textContent).toBe(
      "javascript:alert(1)",
    );
  });
});

describe("tool groups (T26)", () => {
  it("folds the result into the call row, collapsed, opened shows the result", async () => {
    const t = new Timeline();
    t.apply(ev("event.tool.call", withSeq(1)));
    show(t);
    expect(
      root.querySelector(".tool-group")!.classList.contains("tool-running"),
    ).toBe(true);

    t.apply(ev("event.tool.result", withSeq(2)));
    show(t);
    const groups = root.querySelectorAll(".tool-group");
    expect(groups).toHaveLength(1);
    const g = groups[0];
    expect(g.classList.contains("tool-error")).toBe(true);
    expect(
      g.querySelector(".tool-group-head")!.getAttribute("aria-expanded"),
    ).toBe("false");
    expect(g.querySelector(".tool-row")).toBeNull();
    await act(async () =>
      g.querySelector<HTMLButtonElement>(".tool-group-head")!.click(),
    );
    const row = g.querySelector("details.tool-row")!;
    expect(row.hasAttribute("open")).toBe(false);
    expect(row.classList.contains("tool-error")).toBe(true);
    expect(row.querySelector("summary")!.textContent).toContain("$");
    expect(row.querySelector("summary")!.textContent).toContain("cargo test");
    expect(row.querySelector(".mark-error")).not.toBeNull();
    expect(row.textContent).toContain("1 test failed");
  });

  it("shows an Edit's change as a diff, a MultiEdit's per edit, not their arguments", async () => {
    const t = new Timeline();
    t.apply(
      call(1, "a", "Edit", { file_path: "/w/a.md", old_string: "one\ntwo\nfour", new_string: "one\nthree\nfour", replace_all: true }),
    );
    t.apply(result(2, "a"));
    t.apply(
      call(3, "b", "MultiEdit", {
        file_path: "/w/b.md",
        edits: [
          { old_string: "x", new_string: "y" },
          { old_string: "p", new_string: "q" },
        ],
      }),
    );
    t.apply(result(4, "b", true));
    t.apply(call(5, "c", "Bash", { command: "ls" }));
    t.apply(result(6, "c"));
    // Arguments cut at 4 KB.
    const cut = call(7, "d", "Bash", {});
    (cut.payload as { args_preview: string }).args_preview = '{"command":"echo a\\necho b';
    t.apply(cut);
    t.apply(result(8, "d"));
    show(t);
    const g = root.querySelector(".tool-group")!;
    await act(async () =>
      g.querySelector<HTMLButtonElement>(".tool-group-head")!.click(),
    );
    const [edit, multi, bash, cutRow] = [...g.querySelectorAll("details.tool-row")];
    const labels = (row: Element) => [...row.querySelectorAll(".label")].map((l) => l.textContent);
    // An edit that went through has no Result; a failed one keeps its Error.
    expect(labels(edit)).toEqual([]);
    expect(labels(multi)).toEqual(["Error"]);
    expect(labels(bash)).toEqual(["Arguments", "Result"]);
    const diff = edit.querySelector(".tool-change .md-code-diff")!;
    expect(diff.querySelector("pre")!.textContent).toBe(" one\n-two\n+three\n four");
    expect([...diff.querySelectorAll(".diff-del")].map((s) => s.textContent)).toEqual(["-two"]);
    expect([...diff.querySelectorAll(".diff-add")].map((s) => s.textContent)).toEqual(["+three"]);
    // The newline after a marked line is outside its block, so the next line starts a row.
    expect(diff.querySelector(".diff-add")!.nextSibling!.textContent).toBe("\n");
    // `replace_all` is named in the block's bar.
    expect(diff.querySelector(".md-code-lang")!.textContent).toBe("All occurrences");
    expect(edit.textContent).not.toContain("Arguments");
    expect(edit.textContent).not.toContain("old_string");
    expect([...multi.querySelectorAll(".tool-change .md-code-lang")].map((s) => s.textContent)).toEqual([
      "Edit 1 of 2",
      "Edit 2 of 2",
    ]);
    // Other tools keep their arguments.
    expect(bash.querySelector(".tool-change")).toBeNull();
    expect(bash.textContent).toContain("Arguments");
    // As indented JSON.
    expect(bash.querySelector(".tool-row-body .pre")!.textContent).toBe('{\n  "command": "ls"\n}');
    expect(bash.querySelector(".tool-cut")).toBeNull();
    // A cut preview as far as it goes, marked cut.
    expect(cutRow.querySelector(".tool-row-body .pre")!.textContent).toBe('{\n  "command": "echo a\n    echo b"\n}');
    expect(cutRow.querySelector(".tool-cut")!.textContent).toBe(
      "The preview is cut at 4 KB: the rest of the arguments are not shown.",
    );
  });

  it("a result's http and https addresses are links; an Artifact call's page is a line under the run (SPEC 0.180)", async () => {
    const out = (seq: number, call_id: string, result_preview: string) =>
      ev("event.tool.result", (e) => {
        e.seq = seq;
        e.payload = { call_id, error: false, result_preview };
      });
    const t = new Timeline();
    t.apply(call(1, "a", "Bash", { command: "curl -I example.com" }));
    const text =
      'See https://example.com/a?b=1. Not javascript:alert(1) or ftp://x.y/z; "url":"https://en.wikipedia.org/wiki/Rust_(programming_language)", [x](https://a.b/c)';
    t.apply(out(2, "a", text));
    t.apply(call(3, "p", "Artifact", { file_path: "report.html" }));
    t.apply(out(4, "p", "Published Report at https://claude.ai/artifact/abc"));
    // The same page opened again in the run: still one line.
    t.apply(call(5, "q", "Artifact", { file_path: "report.html" }));
    t.apply(out(6, "q", "Published Report at https://claude.ai/artifact/abc"));
    show(t);
    expect(root.querySelectorAll(".tool-artifact")).toHaveLength(1);
    const g = root.querySelector(".tool-group")!;
    // Folded, the page is on a line of its own after the run.
    const page = root.querySelector<HTMLAnchorElement>(".tool-artifact a")!;
    expect(page.getAttribute("href")).toBe("https://claude.ai/artifact/abc");
    expect(page.getAttribute("target")).toBe("_blank");
    expect(g.contains(page)).toBe(false);
    await act(async () =>
      g.querySelector<HTMLButtonElement>(".tool-group-head")!.click(),
    );
    const [, result] = [...g.querySelectorAll("details.tool-row")[0].querySelectorAll(".pre")];
    // A bracket the address opened stays in it; one that closes the text around it does not.
    expect([...result.querySelectorAll("a")].map((a) => a.getAttribute("href"))).toEqual([
      "https://example.com/a?b=1",
      "https://en.wikipedia.org/wiki/Rust_(programming_language)",
      "https://a.b/c",
    ]);
    expect(result.textContent).toBe(text);
  });

  it("labels a result's link with the address it opens, a look-alike host in its punycode form (SPEC 0.180)", async () => {
    const t = new Timeline();
    t.apply(call(1, "a", "Bash", { command: "curl -I example.com" }));
    t.apply(
      ev("event.tool.result", (e) => {
        e.seq = 2;
        e.payload = { call_id: "a", error: false, result_preview: "See https://githаb.com/login" };
      }),
    );
    show(t);
    const g = root.querySelector(".tool-group")!;
    await act(async () =>
      g.querySelector<HTMLButtonElement>(".tool-group-head")!.click(),
    );
    const a = g.querySelector<HTMLAnchorElement>("details.tool-row a")!;
    expect(a.getAttribute("href")).toBe("https://xn--githb-7ve.com/login");
    expect(a.textContent).toBe("https://xn--githb-7ve.com/login");
  });

  it("shows a result without its call as a group of its own", () => {
    const t = new Timeline();
    t.apply(ev("event.tool.result"));
    show(t);
    expect(root.querySelectorAll(".tool-group")).toHaveLength(1);
  });

  it("keeps an interim message as a message of its own, the group under it", async () => {
    const t = new Timeline();
    t.apply(
      agentText(
        1,
        "The encoders are wired right.\nFixing the **config**.",
        false,
      ),
    );
    t.apply(call(2, "a", "Bash", { command: "ninja -C build" }));
    t.apply(result(4, "a"));
    t.apply(call(5, "b", "Bash", { command: "python3 - <<'EOF'\nimport sys\n\nprint(sys.argv)\nEOF" }));
    t.apply(result(40, "b"));
    t.apply(agentText(41, "Done.", true));
    show(t);
    // Both messages are messages; the group says only what ran.
    const msgs = root.querySelectorAll(".msg-agent");
    expect(msgs).toHaveLength(2);
    expect(msgs[0].querySelector("strong")!.textContent).toBe("config");
    const g = root.querySelector(".tool-group")!;
    expect(g.querySelector(".tool-stats")!.textContent).toBe(
      "2 tool calls · 38 s",
    );
    // Two calls: the tools they called, each once, not a command.
    expect(g.querySelector(".tool-kinds")!.textContent).toBe("(Bash)");
    expect(g.querySelector(".tool-peek.mono")).toBeNull();
    expect(g.querySelector(".mark-ok")).not.toBeNull();
    await act(async () =>
      g.querySelector<HTMLButtonElement>(".tool-group-head")!.click(),
    );
    const rows = [...g.querySelectorAll(".tool-row summary")];
    expect(rows.map((s) => s.querySelector(".tool-line")!.textContent)).toEqual([
      "ninja -C build",
      "python3 - <<'EOF'",
    ]);
    // A script: its first line and how many follow (blank lines not counted).
    expect(rows[0].querySelector(".tool-more")).toBeNull();
    expect(rows[1].querySelector(".tool-more")!.textContent).toBe("+3 lines");
  });

  it("shows the first line of a script whose arguments the worker cut at 4 KB", async () => {
    const t = new Timeline();
    // A worker before SPEC 0.155: `command` the first key, the cut inside it, the other keys lost.
    const script = `cat > /tmp/ruler.py <<'EOF'\n${"print('ruler')\n".repeat(400)}EOF`;
    const cut = JSON.stringify({ command: script, description: "Check the ruler", timeout: 60000 }).slice(0, 4096);
    t.apply(
      ev("event.tool.call", (e) => {
        e.seq = 2;
        e.payload = { call_id: "a", tool: "Bash", args_preview: cut };
      }),
    );
    t.apply(result(40, "a"));
    t.apply(agentText(41, "Done.", true));
    show(t);
    const g = root.querySelector(".tool-group")!;
    expect(g.querySelector(".tool-peek.mono")!.textContent).toBe("$ cat > /tmp/ruler.py <<'EOF'");
    await act(async () =>
      g.querySelector<HTMLButtonElement>(".tool-group-head")!.click(),
    );
    const row = g.querySelector(".tool-row summary")!;
    expect(row.querySelector(".tool-line")!.textContent).toBe("cat > /tmp/ruler.py <<'EOF'");
    // Only the lines the preview carries are counted: at least that many follow.
    expect(row.querySelector(".tool-more")!.textContent).toMatch(/^\+\d+\+ lines$/);
  });

  it('says "N tool calls · time" and the tools called; other tools show their argument', async () => {
    const t = new Timeline();
    t.apply(agentText(1, "Done.", true));
    t.apply(call(2, "a", "Read", { file_path: "Cargo.toml" }));
    t.apply(result(3, "a"));
    t.apply(call(4, "c", "Skill", { skill: "worktree", args: "cut one" }));
    t.apply(result(5, "c"));
    t.apply(call(6, "b", "Grep", { pattern: "TODO" }));
    t.apply(result(7, "b", true));
    show(t);
    const g = root.querySelector(".tool-group")!;
    expect(g.querySelector(".tool-stats")!.textContent).toBe(
      "3 tool calls · 5 s",
    );
    expect(g.querySelector(".tool-kinds")!.textContent).toBe("(Read, Skill, Grep)");
    expect(g.classList.contains("tool-error")).toBe(true);
    await act(async () =>
      g.querySelector<HTMLButtonElement>(".tool-group-head")!.click(),
    );
    const rows = [...g.querySelectorAll(".tool-row summary")];
    expect(rows[0].querySelector(".tool-name")!.textContent).toBe("Read");
    expect(rows[0].querySelector(".tool-line")!.textContent).toBe("Cargo.toml");
    // The Skill tool says which skill.
    expect(rows[1].querySelector(".tool-name")!.textContent).toBe("Skill");
    expect(rows[1].querySelector(".tool-line")!.textContent).toBe("worktree");
    expect(rows[2].querySelector(".mark-error")).not.toBeNull();
  });

  it("a SendMessage row says what was sent: its summary, else its message", async () => {
    const t = new Timeline();
    t.apply(agentText(1, "Done.", true));
    // As Claude Code 2.1.312 writes them: the recipient, a summary, the message.
    t.apply(call(2, "a", "SendMessage", { to: "a07b414bf5db5fff8", summary: "Round 1 answers", message: "Answers to your questions.\n\n1. MRI: normal." }));
    t.apply(result(3, "a"));
    t.apply(call(4, "b", "SendMessage", { to: "researcher", message: "Check the API.\nThen report." }));
    t.apply(result(5, "b"));
    show(t);
    const g = root.querySelector(".tool-group")!;
    await act(async () =>
      g.querySelector<HTMLButtonElement>(".tool-group-head")!.click(),
    );
    const rows = [...g.querySelectorAll(".tool-row summary")];
    expect(rows.map((s) => s.querySelector(".tool-line")!.textContent)).toEqual([
      "Round 1 answers",
      "Check the API. Then report.",
    ]);
  });

  it("a group lists each tool once, in the order first called, an MCP tool as server/tool; one call shows its line", async () => {
    const t = new Timeline();
    t.apply(agentText(1, "Looking.", true));
    t.apply(call(2, "a", "Bash", { command: "ls" }));
    t.apply(result(3, "a"));
    t.apply(call(4, "b", "Read", { file_path: "a.txt" }));
    t.apply(result(5, "b"));
    t.apply(call(6, "c", "Bash", { command: "pwd" }));
    t.apply(result(7, "c"));
    t.apply(call(8, "d", "mcp__curator__screenshot", {}));
    t.apply(result(9, "d"));
    t.apply(agentText(10, "One more.", true));
    t.apply(call(11, "e", "Bash", { command: "make test" }));
    t.apply(result(12, "e"));
    show(t);
    const [many, one] = [...root.querySelectorAll(".tool-group")];
    expect(many.querySelector(".tool-stats")!.textContent).toMatch(/^4 tool calls\b/);
    expect(many.querySelector(".tool-kinds")!.textContent).toBe("(Bash, Read, curator/screenshot)");
    expect(many.querySelector(".tool-peek.mono")).toBeNull();
    expect(one.querySelector(".tool-stats")!.textContent).toMatch(/^1 tool call\b/);
    expect(one.querySelector(".tool-peek.mono")!.textContent).toBe("$ make test");
    expect(one.querySelector(".tool-kinds")).toBeNull();
  });

  it("a call's line shows its arguments when it has no main one, and its result after them", async () => {
    // As the worker sends them: no `duration_ms` (Claude Code reports no time of a call).
    const out = (seq: number, call_id: string, result_preview: string, error_text?: string) =>
      ev("event.tool.result", (e) => {
        e.seq = seq;
        e.ts = new Date(Date.parse("2026-10-22T10:00:00Z") + seq * 1000).toISOString();
        e.payload = {
          call_id,
          error: error_text !== undefined,
          result_preview,
          ...(error_text !== undefined && { error_text }),
        };
      });
    // `args_preview` as the worker writes it: the shortest value first (SPEC §4).
    const preview = (seq: number, call_id: string, tool: string, args_preview: string) =>
      ev("event.tool.call", (e) => {
        e.seq = seq;
        e.ts = new Date(Date.parse("2026-10-22T10:00:00Z") + seq * 1000).toISOString();
        e.payload = { call_id, tool, args_preview };
      });
    const t = new Timeline();
    t.apply(agentText(1, "Moving.", true));
    // An MCP tool's input has none of the keys a line reads: its fields in the preview's order,
    // as Claude Code shows each; a lone `result` field of the answer is its value.
    t.apply(preview(2, "a", "mcp__probe-bench__move", '{"z":85,"x":90.7,"y":80.9}'));
    t.apply(out(3, "a", '{"result":"X90.70 Y80.90 Z85.00"}'));
    t.apply(preview(4, "b", "mcp__gitea__list_issues", '{"limit":5,"owner":"me"}'));
    t.apply(out(5, "b", '{"total": 2, "items": [1, 2]}'));
    t.apply(call(6, "c", "mcp__probe-bench__look", {}));
    t.apply(out(7, "c", "3 files\nchanged"));
    t.apply(call(8, "d", "Bash", { command: "git status --short" }));
    t.apply(out(9, "d", " M a.ts\n M b.ts"));
    // A change that went through shows its change, not its result; a failed one, its error.
    t.apply(call(10, "e", "Edit", { file_path: "a.ts", old_string: "a", new_string: "b" }));
    t.apply(out(11, "e", "The file a.ts has been updated successfully."));
    t.apply(call(12, "f", "Write", { file_path: "b.ts", content: "x" }));
    t.apply(out(13, "f", "", "File has not been read yet."));
    // A number or a list stays as written: read and written again, `1.10` would be `1.1`.
    t.apply(call(14, "h", "Bash", { command: "cat VERSION" }));
    t.apply(out(15, "h", "1.10"));
    t.apply(call(16, "i", "Bash", { command: "date +%s%N" }));
    t.apply(out(17, "i", "1763452800123456789"));
    // The CLI's own error, in its tags as it writes it; a long one cut at 300 bytes before its
    // closing tag.
    t.apply(call(18, "j", "mcp__probe-bench__point", { at: "a1" }));
    t.apply(
      out(19, "j", "", "<tool_use_error>Error: No such tool available: mcp__probe-bench__point</tool_use_error>"),
    );
    const cut = `<tool_use_error>String to replace not found in file.\nString: ${"x".repeat(300)}`.slice(0, 300);
    t.apply(call(20, "k", "Edit", { file_path: "c.ts", old_string: "x".repeat(300), new_string: "y" }));
    t.apply(out(21, "k", "", cut));
    t.apply(agentText(22, "One more.", true));
    t.apply(call(23, "g", "mcp__probe-bench__snapshot", { zoom: 2 }));
    t.apply(out(24, "g", '{"result": 42}'));
    show(t);
    const [g, one] = [...root.querySelectorAll(".tool-group")];
    // Many calls: the head names the tools, no result.
    expect(g.querySelector(".tool-group-head .tool-result")).toBeNull();
    // One call: its line and its result.
    expect(one.querySelector(".tool-peek.mono")!.textContent).toBe("snapshot (probe-bench) zoom: 2");
    expect(one.querySelector(".tool-group-head .tool-result")!.textContent).toBe("42");
    await act(async () =>
      g.querySelector<HTMLButtonElement>(".tool-group-head")!.click(),
    );
    const rows = [...g.querySelectorAll(".tool-row summary")];
    expect(rows.map((s) => s.querySelector(".tool-line")!.textContent)).toEqual([
      "z: 85, x: 90.7, y: 80.9",
      'limit: 5, owner: "me"',
      "",
      "git status --short",
      "a.ts",
      "b.ts",
      "cat VERSION",
      "date +%s%N",
      'at: "a1"',
      "c.ts",
    ]);
    expect(rows.map((s) => s.querySelector(".tool-result")?.textContent ?? null)).toEqual([
      "X90.70 Y80.90 Z85.00",
      "total: 2, items: [1,2]",
      "3 files changed",
      "M a.ts M b.ts",
      null,
      "File has not been read yet.",
      "1.10",
      "1763452800123456789",
      "Error: No such tool available: mcp__probe-bench__point",
      // 300 bytes less the 61 of the tag, the sentence and "String: ".
      `String to replace not found in file. String: ${"x".repeat(239)}`,
    ]);
    // Each row's time: from the call to its result.
    expect(rows.map((s) => s.querySelector(".tool-time")?.textContent)).toEqual(Array(10).fill("1 s"));
  });

  it("a thinking line shows as much of its text as the line holds, not 60 characters", () => {
    const text = "The board is resting on a translucent ring, back side up with the connector to the left.";
    const t = new Timeline();
    t.apply(ev("event.agent.thinking", (e) => ((e.seq = 1), (e.payload = { text }))));
    show(t);
    expect(root.querySelector(".thinking-peek")!.textContent).toBe(`Thinking · ${text}`);
  });

  it("takes its mark from the last call", () => {
    const t = new Timeline();
    t.apply(call(1, "a", "Bash", { command: "cat README.md" }));
    t.apply(result(2, "a", true));
    t.apply(call(3, "b", "Bash", { command: "ls" }));
    t.apply(result(4, "b"));
    show(t);
    const g = root.querySelector(".tool-group")!;
    expect(g.classList.contains("tool-ok")).toBe(true);
    expect(g.querySelector(".tool-group-head .mark-ok")).not.toBeNull();
  });

  it("a message closes the group", () => {
    const t = new Timeline();
    t.apply(call(1, "a", "Bash", { command: "ls" }));
    t.apply(result(2, "a"));
    t.apply(agentText(3, "Looking further.", false));
    t.apply(call(4, "b", "Bash", { command: "pwd" }));
    show(t);
    const groups = root.querySelectorAll(".tool-group");
    expect(groups).toHaveLength(2);
    expect(root.querySelector(".msg-agent")!.textContent).toContain("Looking further.");
    expect(groups[1].classList.contains("tool-running")).toBe(true);
  });

  it("a call its turn left without a result is not finished; a call of the turn going on runs", async () => {
    const t = new Timeline();
    t.applyAll([
      call(1, "a", "Bash", { command: "sleep 600" }),
      ev("event.system", (e) => {
        e.seq = 2;
        e.payload = { code: "turn_lost", text: "The agent process stopped during a turn; the turn was lost" };
      }),
      ev("event.user.message", withSeq(3)),
      call(4, "b", "Bash", { command: "make" }),
    ]);
    show(t);
    const [lost, now] = [...root.querySelectorAll(".tool-group")];
    expect(lost.classList.contains("tool-unfinished")).toBe(true);
    expect(lost.querySelector(".pulse-dot")).toBeNull();
    expect(lost.querySelector(".tool-group-head .mark-error")!.getAttribute("aria-label")).toBe("not finished");
    // The one-column gutter marks it as a failed call.
    expect(lost.parentElement!.querySelector(".feed-mark-error")).not.toBeNull();
    await act(async () => lost.querySelector<HTMLButtonElement>(".tool-group-head")!.click());
    const row = lost.querySelector(".tool-row")!;
    expect(row.classList.contains("tool-unfinished")).toBe(true);
    expect(row.querySelector(".mark-error")!.getAttribute("aria-label")).toBe("not finished");
    expect(now.classList.contains("tool-running")).toBe(true);
    expect(now.querySelector(".tool-group-head .pulse-dot")).not.toBeNull();
  });
});

describe("a subagent's card (SPEC §6, since 0.135)", () => {
  const AGENT = {
    description: "Fix schematic power nets in import",
    prompt: "Fix the nets.",
    subagent_type: "general-purpose",
  };
  const at = (seq: number) =>
    new Date(Date.parse("2026-10-22T10:00:00Z") + seq * 1000).toISOString();
  const answer = (seq: number, call_id: string, result_preview: string, ts = at(seq)) =>
    ev("event.tool.result", (e) => {
      e.seq = seq;
      e.ts = ts;
      e.payload = { call_id, result_preview, error: false, duration_ms: 4200 };
    });
  const notification = (seq: number, call_id: string | undefined, status: string, task_id?: string, text = "4") =>
    ev("event.system", (e) => {
      e.seq = seq;
      e.ts = at(seq);
      e.payload = { text, code: "task_notification", call_id, status, task_id };
    });
  const failure = (seq: number, call_id: string, result_preview: string, error_text?: string) =>
    ev("event.tool.result", (e) => {
      e.seq = seq;
      e.ts = at(seq);
      e.payload = { call_id, result_preview, error: true, error_text, duration_ms: 4200 };
    });
  const live = (agents: object[]) =>
    ev("event.subagents.live", (e) => {
      e.payload.agents = agents;
    });
  const state = (t: Timeline, id: string) => {
    const b = buildFeed(t.items(), t).find(
      (x) => x.kind === "subagent" && x.call.payload.call_id === id,
    );
    return b?.kind === "subagent" ? subagentState(b.call, b.result, t) : undefined;
  };

  it("an Agent or Task call is a block of its own that ends the group before it, its result in it and at its place", () => {
    const t = new Timeline();
    t.apply(call(1, "a", "Bash", { command: "ls" }));
    t.apply(result(2, "a"));
    t.apply(call(3, "ag", "Agent", AGENT));
    t.apply(call(4, "b", "Read", { file_path: "a.txt" }));
    t.apply(result(5, "b"));
    t.apply(result(6, "ag"));
    t.apply(call(7, "tk", "Task", AGENT));
    const blocks = buildFeed(t.items(), t);
    expect(blocks.map((b) => b.kind)).toEqual(["tools", "subagent", "tools", "subagent-end", "subagent"]);
    const [bash, ag, read, end, tk] = blocks;
    expect(bash.kind === "tools" && bash.rows.length).toBe(1);
    expect(ag.kind === "subagent" && [ag.call.seq, ag.result?.seq]).toEqual([3, 6]);
    expect(read.kind === "tools" && read.rows.length).toBe(1);
    expect(end.kind === "subagent-end" && [end.call.seq, end.end.seq]).toEqual([3, 6]);
    expect(tk.kind === "subagent" && tk.result).toBeUndefined();
  });

  it("is running while the live list has its call, then as the first notification of the call says", () => {
    const t = new Timeline();
    t.apply(call(1, "ag", "Agent", AGENT));
    expect(state(t, "ag")).toEqual({ state: "running" });
    t.apply(live([{ task_id: "a1", description: "Fix", duration_ms: 1000, call_id: "ag" }]));
    expect(state(t, "ag")).toMatchObject({ state: "running", live: { task_id: "a1" } });
    // The CLI's answer for a subagent moved to the background, its row gone: no end seen.
    t.apply(answer(2, "ag", "Async agent launched successfully.\nagentId: a1"));
    t.apply(live([]));
    expect(state(t, "ag")).toEqual({ state: "background" });
    t.apply(notification(31, "other", "failed"));
    t.apply(notification(61, "ag", "completed"));
    expect(state(t, "ag")).toEqual({ state: "done", ms: 60_000 });
    // A later end of the call, a SendMessage that continued the subagent, is not its end.
    t.apply(notification(8041, "ag", "failed"));
    t.apply(notification(8042, undefined, "stopped", "a1"));
    expect(state(t, "ag")).toEqual({ state: "done", ms: 60_000 });
  });

  it("without a row or an end takes its state from its result: failed, in the background, done", () => {
    const t = new Timeline();
    t.apply(call(1, "e", "Agent", AGENT));
    t.apply(result(5, "e", true));
    t.apply(call(6, "bg", "Agent", { ...AGENT, run_in_background: true }));
    t.apply(result(7, "bg"));
    t.apply(call(8, "ok", "Agent", AGENT));
    t.apply(result(18, "ok"));
    t.apply(call(19, "same", "Agent", AGENT));
    t.apply(answer(20, "same", "4", at(19)));
    expect(state(t, "e")).toEqual({ state: "failed" });
    expect(state(t, "bg")).toEqual({ state: "background" });
    expect(state(t, "ok")).toEqual({ state: "done", ms: 10_000 });
    // Times that say nothing: the result's `duration_ms`.
    expect(state(t, "same")).toEqual({ state: "done", ms: 4200 });
  });

  it("stopped by Stop: an error result that says the request was interrupted", () => {
    const t = new Timeline();
    const stop = "[Request interrupted by user for tool use]";
    t.apply(call(1, "text", "Agent", AGENT));
    t.apply(failure(2, "text", "4", stop));
    t.apply(call(3, "preview", "Agent", AGENT));
    t.apply(failure(4, "preview", stop));
    t.apply(call(5, "other", "Agent", AGENT));
    t.apply(failure(6, "other", "4", "The subagent crashed"));
    expect(state(t, "text")).toEqual({ state: "stopped" });
    expect(state(t, "preview")).toEqual({ state: "stopped" });
    expect(state(t, "other")).toEqual({ state: "failed" });
  });

  it("in the background, ends with a notification that names only its task: the agentId of its result", () => {
    const t = new Timeline();
    t.apply(call(1, "bg", "Agent", { ...AGENT, run_in_background: true }));
    t.apply(
      answer(
        2,
        "bg",
        "Async agent launched successfully. (The agent works in the background.)\n" +
          "agentId: a52af2b436ded4cea (internal ID - do not mention to user.)",
      ),
    );
    expect(state(t, "bg")).toEqual({ state: "background" });
    t.apply(notification(31, undefined, "failed", "b0ther"));
    expect(state(t, "bg")).toEqual({ state: "background" });
    t.apply(notification(61, undefined, "completed", "a52af2b436ded4cea"));
    expect(state(t, "bg")).toEqual({ state: "done", ms: 60_000 });
  });

  it("running: its type, the background chip, its task, what it does and its counts, ticking", async () => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] });
    try {
      const t = new Timeline();
      t.apply(call(1, "ag", "Agent", { ...AGENT, run_in_background: true }));
      t.apply(
        live([
          {
            task_id: "a1",
            description: "Fix",
            activity: "Running Verify the samples",
            total_tokens: 155_300,
            tool_uses: 188,
            duration_ms: 253_000,
            call_id: "ag",
          },
        ]),
      );
      t.liveAgentsAt = NOW;
      let clock = NOW;
      show(t, { now: () => clock });
      expect(root.querySelector(".tool-group")).toBeNull();
      const card = root.querySelector(".subagent")!;
      expect(card.classList.contains("subagent-running")).toBe(true);
      // The head says what the card is (SPEC 0.141), not only the icon.
      expect(card.getAttribute("aria-label")).toBe("Subagent");
      expect(card.querySelector(".subagent-label")!.textContent).toBe("Agent started:");
      expect(card.querySelector(".subagent-type")!.textContent).toBe("general-purpose");
      expect(card.querySelector(".subagent-chip")!.textContent).toBe("background");
      expect(card.querySelector(".subagent-task")!.textContent).toBe(AGENT.description);
      expect(card.querySelector(".subagent-status .pulse-dot")).not.toBeNull();
      // What it does alone: the CLI's text often starts "Running …" itself.
      expect(card.querySelector(".subagent-state")).toBeNull();
      expect(card.querySelector(".subagent-activity")!.textContent).toBe("Running Verify the samples");
      expect(card.querySelector(".subagent-stats")!.textContent).toBe("155.3k tokens · 188 tools · 4m 13s");
      await act(async () => {
        clock += 2000;
        vi.advanceTimersByTime(1000);
      });
      expect(card.querySelector(".subagent-stats")!.textContent).toBe("155.3k tokens · 188 tools · 4m 15s");
    } finally {
      vi.useRealTimers();
    }
  });

  it("a foreground subagent stays done: a later notification of the agentId its result names is no end of it", () => {
    const t = new Timeline();
    t.apply(call(1, "fg", "Agent", AGENT));
    t.apply(
      answer(
        11,
        "fg",
        "Fixed.\nagentId: a1 (use SendMessage with to: 'a1', summary: '<5-10 word recap>' to continue this agent)",
      ),
    );
    // A SendMessage that continued it, ended much later.
    t.apply(notification(2831, undefined, "failed", "a1"));
    expect(state(t, "fg")).toEqual({ state: "done", ms: 10_000 });
  });

  it("a background launch whose preview is cut in its prompt keeps its type and the chip", () => {
    const t = new Timeline();
    t.apply(
      ev("event.tool.call", (e) => {
        e.seq = 1;
        e.ts = at(1);
        e.payload = {
          call_id: "ag",
          tool: "Agent",
          args_preview:
            '{"description":"Fix the nets","run_in_background":true,"subagent_type":"general-purpose","prompt":"' +
            "x".repeat(4000),
        };
      }),
    );
    show(t);
    const card = root.querySelector(".subagent")!;
    expect(card.querySelector(".subagent-type")!.textContent).toBe("general-purpose");
    expect(card.querySelector(".subagent-chip")!.textContent).toBe("background");
    expect(card.querySelector(".subagent-task")!.textContent).toBe("Fix the nets");
  });

  it("running without what it does: Running and the time since its call", () => {
    const t = new Timeline();
    t.apply(call(1, "ag", "Agent", AGENT));
    show(t, { now: () => Date.parse(at(1)) + 5000 });
    const card = root.querySelector(".subagent")!;
    expect(card.querySelector(".subagent-status .pulse-dot")).not.toBeNull();
    expect(card.querySelector(".subagent-state")!.textContent).toBe("Running");
    expect(card.querySelector(".subagent-activity")).toBeNull();
    expect(card.querySelector(".subagent-stats")!.textContent).toBe("5s");
  });

  it("failed with its error, stopped, in the background", () => {
    const t = new Timeline();
    t.apply(call(1, "e", "Agent", AGENT));
    t.apply(failure(2, "e", "4", "The subagent crashed"));
    t.apply(call(3, "st", "Agent", AGENT));
    t.apply(failure(4, "st", "[Request interrupted by user for tool use]"));
    t.apply(call(5, "bg", "Agent", { ...AGENT, run_in_background: true }));
    t.apply(answer(6, "bg", "Async agent launched successfully.\nagentId: a1"));
    show(t);
    const [failed, stopped, background] = root.querySelectorAll(".subagent:not(.subagent-end)");
    expect(failed.classList.contains("subagent-failed")).toBe(true);
    expect(failed.querySelector(".subagent-status .mark-error")).not.toBeNull();
    expect(failed.querySelector(".subagent-state")!.textContent).toBe("Failed");
    expect(failed.querySelector(".subagent-error")!.textContent).toBe("The subagent crashed");
    expect(stopped.classList.contains("subagent-stopped")).toBe(true);
    expect(stopped.querySelector(".subagent-status")!.textContent).toBe("Stopped");
    expect(background.classList.contains("subagent-background")).toBe(true);
    expect(background.querySelector(".subagent-status")!.textContent).toBe("In background");
    expect(background.querySelector(".subagent-chip")!.textContent).toBe("background");
  });

  it("done: a check and its run time; the head opens the call's arguments and result", async () => {
    const t = new Timeline();
    t.apply(call(1, "ag", "Agent", AGENT));
    t.apply(result(254, "ag"));
    show(t);
    const card = root.querySelector(".subagent")!;
    expect(card.classList.contains("subagent-done")).toBe(true);
    expect(card.querySelector(".subagent-status .mark-ok")).not.toBeNull();
    expect(card.querySelector(".subagent-state")!.textContent).toBe("Done · 4m 13s");
    expect(card.querySelector(".subagent-chip")).toBeNull();
    expect(card.querySelector(".tool-row-body")).toBeNull();
    const head = card.querySelector<HTMLButtonElement>(".subagent-head")!;
    await act(async () => head.click());
    expect(head.getAttribute("aria-expanded")).toBe("true");
    expect([...card.querySelectorAll(".label")].map((l) => l.textContent)).toEqual(["Arguments", "Result"]);
    expect(card.querySelector(".tool-row-body")!.textContent).toContain("out ag");
  });

  it("ends with a card at its notification, by its call or by the agentId of its launch, not at the launch's result", async () => {
    const t = new Timeline();
    t.apply(call(1, "ag", "Agent", { ...AGENT, run_in_background: true }));
    t.apply(answer(2, "ag", "Async agent launched successfully.\nagentId: a52af2b436ded4cea (internal ID)"));
    t.apply(call(3, "rs", "Agent", { ...AGENT, description: "Check the board", run_in_background: true }));
    t.apply(answer(4, "rs", "Async agent launched successfully.\nagentId: b11 (internal ID)"));
    t.apply(notification(61, "ag", "completed", "a52af2b436ded4cea", "## Report\n\nThe nets are **fixed**."));
    // After a restart the CLI reports a subagent cut off by it without its call.
    t.apply(notification(124, undefined, "failed", "b11", "Agent \"Check the board\" failed"));
    const blocks = buildFeed(t.items(), t);
    expect(blocks.map((b) => (b.kind === "subagent-end" ? [b.kind, b.call.payload.call_id, b.end.seq] : b.kind))).toEqual([
      "subagent",
      "subagent",
      ["subagent-end", "ag", 61],
      ["subagent-end", "rs", 124],
    ]);
    show(t);
    const [done, failed] = root.querySelectorAll(".subagent-end");
    expect(done.classList.contains("subagent-done")).toBe(true);
    expect(done.getAttribute("aria-label")).toBe("Subagent ended");
    expect(done.querySelector(".subagent-label")!.textContent).toBe("Agent finished:");
    expect(done.querySelector(".subagent-icon .mark-ok")).not.toBeNull();
    expect(done.querySelector(".subagent-task")!.textContent).toBe(AGENT.description);
    expect(done.querySelector(".subagent-state")!.textContent).toBe("done");
    expect(done.querySelector(".subagent-stats")!.textContent).toBe("1m 0s");
    expect(done.querySelector(".subagent-end-peek")!.textContent).toBe("Report The nets are fixed.");
    expect(failed.classList.contains("subagent-failed")).toBe(true);
    expect(failed.querySelector(".subagent-icon .mark-error")).not.toBeNull();
    expect(failed.querySelector(".subagent-task")!.textContent).toBe("Check the board");
    expect(failed.querySelector(".subagent-state")!.textContent).toBe("failed");
    expect(failed.querySelector(".subagent-stats")!.textContent).toBe("2m 1s");
    // Not a "Background task" line as well.
    expect(root.querySelector(".line-system-task_notification")).toBeNull();
    // The head opens the whole text as Markdown.
    const head = done.querySelector<HTMLButtonElement>(".subagent-head")!;
    await act(async () => head.click());
    expect(head.getAttribute("aria-expanded")).toBe("true");
    expect(done.querySelector(".subagent-end-text h2")!.textContent).toBe("Report");
    expect(done.querySelector(".subagent-end-text strong")!.textContent).toBe("fixed");
    expect(done.querySelector(".subagent-end-peek")).toBeNull();
  });

  it("one that ran in the foreground ends with a card at its result: done with its text, failed with the error, stopped", () => {
    const t = new Timeline();
    t.apply(call(1, "ok", "Agent", AGENT));
    // The CLI's trailer after the subagent's report: its id and what it spent.
    t.apply(
      answer(
        254,
        "ok",
        "Fixed the nets.\nagentId: a1 (use SendMessage with to: 'a1', summary: '<5-10 word recap>' to continue this agent)\n" +
          "<usage>subagent_tokens: 158597\ntool_uses: 12\nduration_ms: 253000</usage>",
      ),
    );
    t.apply(call(255, "e", "Agent", AGENT));
    t.apply(failure(256, "e", "4", "The subagent crashed"));
    t.apply(call(257, "st", "Agent", AGENT));
    t.apply(failure(258, "st", "[Request interrupted by user for tool use]"));
    const blocks = buildFeed(t.items(), t);
    expect(blocks.map((b) => (b.kind === "subagent-end" ? [b.kind, b.end.seq] : b.kind))).toEqual([
      "subagent",
      ["subagent-end", 254],
      "subagent",
      ["subagent-end", 256],
      "subagent",
      ["subagent-end", 258],
    ]);
    show(t);
    const [done, failed, stopped] = root.querySelectorAll(".subagent-end");
    expect(done.classList.contains("subagent-done")).toBe(true);
    expect(done.querySelector(".subagent-stats")!.textContent).toBe("4m 13s");
    expect(done.querySelector(".subagent-end-peek")!.textContent).toBe("Fixed the nets.");
    expect(failed.classList.contains("subagent-failed")).toBe(true);
    expect(failed.querySelector(".subagent-state")!.textContent).toBe("failed");
    expect(failed.querySelector(".subagent-end-peek")!.textContent).toBe("The subagent crashed");
    expect(stopped.classList.contains("subagent-stopped")).toBe(true);
    expect(stopped.querySelector(".subagent-state")!.textContent).toBe("stopped");
    expect(stopped.querySelector('.subagent-icon [aria-label="stopped"]')).not.toBeNull();
  });

  it("a later end of the same call, a SendMessage that continued it, has no run time", () => {
    const t = new Timeline();
    t.apply(call(1, "ag", "Agent", { ...AGENT, run_in_background: true }));
    t.apply(answer(2, "ag", "Async agent launched successfully.\nagentId: a1 (internal ID)"));
    t.apply(notification(61, "ag", "completed", "a1", "Fixed"));
    t.apply(notification(8041, undefined, "completed", "a1", "Fixed again"));
    show(t);
    const [first, again] = root.querySelectorAll(".subagent-end");
    // The start card, too, by the first end.
    expect(root.querySelector(".subagent:not(.subagent-end) .subagent-state")!.textContent).toBe("Done · 1m 0s");
    expect(first.querySelector(".subagent-stats")!.textContent).toBe("1m 0s");
    expect(again.querySelector(".subagent-end-peek")!.textContent).toBe("Fixed again");
    expect(again.querySelector(".subagent-stats")).toBeNull();
  });

  it("says what the subagent spent at its end, when its notification says (SPEC 0.181)", () => {
    const spent = (e: HqcEvent) => {
      (e.payload as any).usage = { total_tokens: 155300, tool_uses: 188, duration_ms: 253000 };
      return e;
    };
    const t = new Timeline();
    t.apply(call(1, "ag", "Agent", { ...AGENT, run_in_background: true }));
    t.apply(answer(2, "ag", "Async agent launched successfully.\nagentId: a1 (internal ID)"));
    t.apply(spent(notification(61, "ag", "completed", "a1", "Fixed")));
    t.apply(spent(notification(8041, undefined, "completed", "a1", "Fixed again")));
    show(t);
    const [first, again] = root.querySelectorAll(".subagent-end");
    expect(first.querySelector(".subagent-stats")!.textContent).toBe("155.3k tokens · 188 tools · 1m 0s");
    // A later end has no run time from the call.
    expect(again.querySelector(".subagent-stats")!.textContent).toBe("155.3k tokens · 188 tools");
  });

  it("a request for a call of the same batch keeps its call across a subagent's end card", () => {
    // Ask mode: the CLI runs the batch's calls together, so the request comes after the
    // subagent's result.
    const t = new Timeline();
    t.apply(call(1, "ag", "Agent", AGENT));
    t.apply(call(2, "sh", "Bash", { command: "cargo test" }));
    t.apply(answer(3, "ag", "Fixed."));
    t.apply(
      ev("event.approval.request", (e) => {
        e.seq = 4;
        e.ts = at(4);
      }),
    );
    const blocks = buildFeed(t.items(), t);
    expect(blocks.map((b) => b.kind)).toEqual(["subagent", "subagent-end", "approval"]);
    const request = blocks[2];
    expect(request.kind === "approval" && request.call?.payload.call_id).toBe("sh");
  });

  it("a request does not take a call whose result came before it: a subagent's own request", () => {
    // Ask mode: a subagent's request is in the feed, its call is not.
    const t = new Timeline();
    t.apply(call(1, "ls", "Bash", { command: "ls" }));
    t.apply(result(2, "ls"));
    t.apply(call(3, "ag", "Agent", AGENT));
    t.apply(
      ev("event.approval.request", (e) => {
        e.seq = 4;
        e.ts = at(4);
      }),
    );
    const blocks = buildFeed(t.items(), t);
    expect(blocks.map((b) => b.kind)).toEqual(["tools", "subagent", "approval"]);
    const [group, , request] = blocks;
    expect(group.kind === "tools" && group.rows.length).toBe(1);
    expect(request.kind === "approval" && request.call).toBeUndefined();
  });

  it("a command's or a Monitor's notification stays a Background task line", () => {
    const t = new Timeline();
    t.apply(call(1, "sh", "Bash", { command: "sleep 60", run_in_background: true }));
    t.apply(result(2, "sh"));
    t.apply(notification(61, "sh", "completed", "b1", 'Background command "sleep 60" completed (exit code 0)'));
    // A Monitor's event: its task only, no call nor status.
    t.apply(
      ev("event.system", (e) => {
        e.seq = 62;
        e.ts = at(62);
        e.payload = { text: 'Monitor event: "build log"', code: "task_notification", task_id: "bm1" };
      }),
    );
    show(t);
    expect(root.querySelector(".subagent-end")).toBeNull();
    expect(root.querySelectorAll("details.line-system-task_notification")).toHaveLength(2);
  });
});

describe("messages", () => {
  it("shows an optimistic message and replaces it with the echo", () => {
    const t = new Timeline();
    t.addPending("op-1", "Run the tests");
    show(t);
    expect(root.querySelector(".msg-pending")!.textContent).toContain(
      "Run the tests",
    );
    t.apply(ev("event.user.message"));
    show(t);
    expect(root.querySelector(".msg-pending")).toBeNull();
    expect(root.querySelectorAll(".msg-user")).toHaveLength(1);
  });

  it("offers resend for a message the hub did not accept", async () => {
    const t = new Timeline();
    t.addPending("op-9", "hi");
    t.failPending("op-9", "HTTP 502");
    const { actions } = show(t);
    // An alert box under the message: the mark, "Not sent" and the reason, then Retry and
    // Discard as buttons of one size.
    const alert = root.querySelector(".msg-failed .send-alert")!;
    expect(alert.getAttribute("role")).toBe("alert");
    expect(
      [...alert.children].map((c) => [c.className, c.textContent]),
    ).toEqual([
      ["send-alert-icon", ""],
      ["send-alert-text", "Not sentHTTP 502"],
      ["send-alert-acts", "RetryDiscard"],
    ]);
    expect(
      [...alert.querySelectorAll(".send-alert-acts button")].map((b) => b.className),
    ).toEqual(["btn btn-small btn-primary btn-resend", "btn btn-small btn-discard"]);
    await act(async () => buttonByText("Retry")!.click());
    expect(actions.resend).toHaveBeenCalledWith("op-9");
    await act(async () => buttonByText("Discard")!.click());
    expect(actions.discard).toHaveBeenCalledWith("op-9");
  });

  it("marks a failed delivery", () => {
    const t = new Timeline();
    t.apply(ev("event.user.message", (e) => (e.delivery = "failed")));
    show(t);
    expect(root.querySelector(".delivery-failed")).not.toBeNull();
    expect(t.hasFailedDelivery()).toBe(true);
  });

  it("puts the usage of a turn under its final answer, muted", () => {
    const t = new Timeline();
    t.apply(agentText(1, "All tests passed.", true));
    t.apply(
      ev("event.status", (e) => ((e.seq = 2), (e.payload = { state: "idle" }))),
    );
    t.apply(ev("event.usage", withSeq(3)));
    show(t);
    const meta = root.querySelector(".msg-final > .meta")!;
    expect(meta.querySelector(".usage")!.textContent).toBe(
      "1.2k → 340 tokens · cache 9k · $0.012",
    );
    expect(root.querySelector(".line-usage")).toBeNull();
  });

  it("draws no line for a usage with the session's total only, and counts that total", () => {
    const t = new Timeline();
    t.apply(agentText(1, "All tests passed.", true));
    t.apply(ev("event.usage", withSeq(2)));
    // What a background subagent spent after the turn: no tokens, no cost of a turn.
    t.apply(
      ev(
        "event.usage",
        (e) => (
          (e.seq = 3),
          (e.payload = {
            input_tokens: 0,
            output_tokens: 0,
            session_cost_usd: 2.5,
          })
        ),
      ),
    );
    show(t);
    expect(root.querySelectorAll(".usage")).toHaveLength(1);
    expect(root.querySelector(".line-usage")).toBeNull();
    expect(t.usageTotals().cost).toBe(2.5);
  });

  it("counts the conversations before a /clear in the session's cost", () => {
    const at = (seq: number) =>
      `0199f3a2-7c10-7000-8000-0000000003${String(seq).padStart(2, "0")}`;
    const usage = (seq: number, total: number) =>
      ev("event.usage", (e) => {
        e.seq = seq;
        e.event_id = at(seq);
        e.payload = {
          input_tokens: 10,
          output_tokens: 5,
          cost_usd: 0.25,
          session_cost_usd: total,
        };
      });
    const cleared = (seq: number) =>
      ev("event.system", (e) => {
        e.seq = seq;
        e.event_id = at(seq);
        e.payload = { text: "Conversation cleared", code: "conversation_cleared" };
      });
    const history = [
      usage(1, 1.25),
      usage(2, 2.5),
      cleared(3),
      cleared(4),
      usage(5, 0.25),
      // The agent unloaded and resumed: the CLI goes on from the new transcript's total.
      usage(6, 0.75),
    ];
    const t = new Timeline();
    for (const e of history.slice(0, 4)) t.apply(e);
    expect(t.usageTotals().cost).toBe(2.5);
    for (const e of history.slice(4)) t.apply(e);
    expect(t.usageTotals().cost).toBe(3.25);
    // A chat opened again gets the newest events first, then the older ones.
    const reopened = new Timeline();
    for (const e of [...history].reverse()) reopened.apply(e);
    expect(reopened.usageTotals().cost).toBe(3.25);
  });

  it("draws the line of a usage with no tokens but a cost, as /compact's", () => {
    const t = new Timeline();
    t.apply(agentText(1, "All tests passed.", true));
    t.apply(ev("event.usage", withSeq(2)));
    t.apply(
      ev(
        "event.usage",
        (e) => (
          (e.seq = 3),
          (e.payload = {
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0014,
            session_cost_usd: 2.6,
          })
        ),
      ),
    );
    show(t);
    expect(root.querySelector(".line-usage")!.textContent).toContain("$0.001");
  });

  it("updates delivery from a delivery frame and from a copy the hub sent again", () => {
    const t = new Timeline();
    t.apply(
      ev(
        "event.user.message",
        (e) => ((e.delivery = "pending"), delete e.delivered_at),
      ),
    );
    // The same seq again (REST page and backlog overlap) is the same event; only its
    // delivery may be newer (a frame lost while the socket was down).
    t.apply(ev("event.user.message"));
    expect(t.events()).toHaveLength(1);
    expect(t.events()[0].delivery).toBe("delivered");
    t.apply(
      ev(
        "event.user.message",
        (e) => ((e.delivery = "pending"), delete e.delivered_at),
      ),
    );
    expect(t.events()[0].delivery).toBe("pending");
    const d = parseServerFrame(
      JSON.parse(readFileSync(join(WIRE, "client.delivery.json"), "utf8")),
    );
    if (d.type !== "delivery") throw new Error("not a delivery frame");
    t.applyDelivery(d);
    expect(t.events()[0]).toMatchObject({
      delivery: "delivered",
      delivered_at: d.delivered_at,
    });
    // A change for another event of the same seq is ignored.
    t.applyDelivery({ ...d, event_id: "other", delivery: "failed" });
    expect(t.events()[0].delivery).toBe("delivered");
  });

  it("shows the POST /input reply without moving lastSeq", () => {
    const t = new Timeline();
    t.apply(ev("event.system", withSeq(1)));
    t.addPending("op-1", "Run the tests");
    t.applyReply(ev("event.user.message", withSeq(5)));
    expect(t.items().some((i) => i.kind === "pending")).toBe(false);
    expect(t.lastSeq).toBe(1);
    t.apply(ev("event.user.message", withSeq(5)));
    expect(t.lastSeq).toBe(5);
    expect(t.events()).toHaveLength(2);
  });

  it("renders system as a line, status only for an error with a reason", () => {
    const t = new Timeline();
    const status = (seq: number, state: string, detail?: string) =>
      ev("event.status", (e) => {
        e.seq = seq;
        e.payload = detail === undefined ? { state } : { state, detail };
      });
    t.apply(status(1, "waiting", "approval requested"));
    t.apply(ev("event.system", withSeq(2)));
    t.apply(status(3, "idle"));
    t.apply(status(4, "running"));
    t.apply(status(5, "error"));
    t.apply(status(6, "error", "claude exited with code 1"));
    show(t);
    const lines = [...root.querySelectorAll(".line")].map((l) => l.textContent);
    expect(lines).toHaveLength(2);
    // session_created says when the session began (Main board).
    expect(lines[0]).toMatch(/^Session started today at \d\d:\d\d$/);
    expect(lines[1]).toBe("Error · claude exited with code 1");
    expect(t.events()).toHaveLength(6);
  });

  it("marks a call denied by the worker policy", () => {
    const t = new Timeline();
    t.apply(
      ev("event.system", (e) => {
        e.seq = 1;
        e.payload = {
          code: "policy_denied",
          text: "Denied by the worker's permission policy: Permission to use Bash with command git push has been denied.",
        };
      }),
    );
    show(t);
    const line = root.querySelector(".line-system-policy_denied")!;
    expect(line.textContent).toContain("Blocked by worker policy");
    expect(line.textContent).toContain("git push");
  });
});

describe("turn indicator (T13, T26): from the session state, no events of its own", () => {
  function draw(state: any, timeline = new Timeline()) {
    root = document.createElement("div");
    document.body.appendChild(root);
    act(() =>
      render(
        <ActivityLine state={state} timeline={timeline} now={() => NOW} />,
        root,
      ),
    );
    return root.querySelector(".activity");
  }

  it('running: "Agent is working" with a pulse', () => {
    const line = draw("running")!;
    expect(line.textContent).toBe("Agent is working");
    expect(line.querySelector(".pulse-dot")).not.toBeNull();
  });

  it("running with an activity (SPEC 0.179): what the agent waits on instead", () => {
    const line = (activity: Activity, state: any = "running", t = new Timeline()) => {
      root = document.createElement("div");
      document.body.appendChild(root);
      act(() =>
        render(
          <ActivityLine state={state} timeline={t} now={() => NOW} activity={activity} />,
          root,
        ),
      );
      const text = root.querySelector(".activity")?.textContent;
      render(null, root);
      return text;
    };
    const retry = { kind: "retrying", attempt: 3, max_retries: 10, retry_delay_ms: 7400 } as const;
    expect(line({ ...retry, error_status: 529 })).toBe("Retrying 3/10 (529), next in 8 s");
    expect(line(retry)).toBe("Retrying 3/10 (no response), next in 8 s");
    // The next attempt goes at once: no delay to count.
    expect(line({ ...retry, retry_delay_ms: 0 })).toBe("Retrying 3/10 (no response)");
    expect(line({ kind: "compacting" })).toBe("Compacting the conversation…");
    expect(line({ kind: "thinking", estimated_tokens: 12345 })).toBe("Thinking · ~12k tokens");
    expect(line({ kind: "thinking", estimated_tokens: 840 })).toBe("Thinking · ~840 tokens");
    // A running command keeps its own line; other states keep theirs.
    const t = new Timeline();
    t.apply(call(1, "b", "Bash", { command: "sleep 30" }));
    expect(line({ kind: "compacting" }, "running", t)).toContain("sleep 30");
    expect(line({ kind: "compacting" }, "queued")).toBe("Input queued");
  });

  it('running with the last tool.call unanswered: "Running <command> <time>"', () => {
    const t = new Timeline();
    t.apply(call(1, "a", "Read", { file_path: "a.rs" }));
    t.apply(result(2, "a"));
    t.apply(
      ev("event.tool.call", (e) => {
        e.seq = 3;
        e.ts = "2026-10-22T10:28:36Z";
        e.payload = {
          call_id: "b",
          tool: "Bash",
          args_preview: '{"command":"cd build && ctest"}',
        };
      }),
    );
    const line = draw("running", t)!;
    expect(line.querySelector(".activity-label")!.textContent).toBe("Running");
    expect(line.querySelector(".activity-what")!.textContent).toBe(
      "cd build && ctest",
    );
    expect(line.querySelector(".activity-timer")!.textContent).toBe("1:24");
    t.apply(result(4, "b"));
    render(null, root);
    expect(draw("running", t)!.textContent).toBe("Agent is working");
  });

  it('a call younger than 2 s leaves the line at "Agent is working"', () => {
    const t = new Timeline();
    t.apply(
      ev("event.tool.call", (e) => {
        e.seq = 1;
        e.ts = new Date(NOW - 500).toISOString();
        e.payload = {
          call_id: "a",
          tool: "Bash",
          args_preview: '{"command":"git status"}',
        };
      }),
    );
    const line = (at: number) => {
      root = document.createElement("div");
      document.body.appendChild(root);
      act(() =>
        render(
          <ActivityLine
            state="running"
            timeline={t}
            now={() => at}
            foreground={["a"]}
            onBackground={vi.fn()}
          />,
          root,
        ),
      );
      return root.querySelector(".activity")!;
    };
    expect(line(NOW).textContent).toBe("Agent is working");
    render(null, root);
    const later = line(NOW + 1500);
    expect(later.querySelector(".activity-label")!.textContent).toBe("Running");
    expect(later.querySelector(".activity-what")!.textContent).toBe("git status");
    expect(later.querySelector(".activity-timer")!.textContent).toBe("0:02");
  });

  it("a call that waited for a permission or an answer counts its 2 s from the answer", () => {
    for (const kind of ["approval", "clarify"]) {
      const t = new Timeline();
      t.apply(
        ev("event.tool.call", (e) => {
          e.seq = 1;
          e.ts = new Date(NOW - 60_000).toISOString();
          e.payload = {
            call_id: "a",
            tool: "Bash",
            args_preview: '{"command":"git status"}',
          };
        }),
      );
      t.apply(ev(`event.${kind}.request`, withSeq(2)));
      t.apply(
        ev(`event.${kind}.response`, (e) => {
          e.seq = 3;
          e.ts = new Date(NOW - 500).toISOString();
        }),
      );
      expect(draw("running", t)!.textContent).toBe("Agent is working");
      render(null, root);
    }
  });

  it("names the call on a tick of its own, with no new event", () => {
    const t = new Timeline();
    t.apply(
      ev("event.tool.call", (e) => {
        e.seq = 1;
        e.ts = new Date(NOW - 500).toISOString();
        e.payload = {
          call_id: "a",
          tool: "Bash",
          args_preview: '{"command":"cargo build"}',
        };
      }),
    );
    try {
      vi.useFakeTimers({ toFake: ["Date", "setInterval", "clearInterval"] });
      vi.setSystemTime(NOW);
      root = document.createElement("div");
      document.body.appendChild(root);
      act(() => render(<ActivityLine state="running" timeline={t} />, root));
      expect(root.querySelector(".activity")!.textContent).toBe(
        "Agent is working",
      );
      act(() => void vi.advanceTimersByTime(2000));
      expect(root.querySelector(".activity-what")!.textContent).toBe(
        "cargo build",
      );
    } finally {
      render(null, root);
      vi.useRealTimers();
    }
  });

  it("names an MCP tool as the feed does, its arguments after it, paths relative to the session's folder", () => {
    const line = (tool: string, args: object) => {
      const t = new Timeline();
      t.apply(call(1, "a", tool, args));
      if (root) render(null, root);
      root = document.createElement("div");
      document.body.appendChild(root);
      act(() => render(<ActivityLine state="running" timeline={t} now={() => NOW} root="/w/hqc" />, root));
      return root.querySelector(".activity-what")!.textContent;
    };
    expect(line("mcp__probe-bench__move", { x: 90.7, y: 80.9 })).toBe("move (probe-bench) x: 90.7, y: 80.9");
    expect(line("mcp__probe-bench__look", {})).toBe("look (probe-bench)");
    expect(line("Read", { file_path: "/w/hqc/src/a.rs" })).toBe("Read src/a.rs");
    expect(line("Bash", { command: "cat /w/hqc/Cargo.toml" })).toBe("cat Cargo.toml");
  });

  it("a call before the last end of a turn is not the running one", () => {
    const t = new Timeline();
    t.applyAll([
      call(1, "a", "Bash", { command: "sleep 600" }),
      agentText(2, "Stopped.", true),
      ev("event.user.message", withSeq(3)),
    ]);
    expect(draw("running", t)!.textContent).toBe("Agent is working");
    render(null, root);
    t.apply(call(4, "b", "Bash", { command: "make" }));
    expect(draw("running", t)!.querySelector(".activity-what")!.textContent).toBe("make");
  });

  it('a waiting hqc_ask (T32): "Waiting for a reply from <session> <time>"', () => {
    const t = new Timeline();
    t.apply(
      ev("event.tool.call", (e) => {
        e.seq = 1;
        e.ts = "2026-10-22T10:28:36Z";
        e.payload = {
          call_id: "q",
          tool: "mcp__hqc__hqc_ask",
          args_preview:
            '{"text":"Is the build green?","session":"hqc/build-bot"}',
        };
      }),
    );
    const line = draw("running", t)!;
    expect(line.classList.contains("activity-ask")).toBe(true);
    expect(line.querySelector(".activity-label")!.textContent).toBe(
      "Waiting for a reply",
    );
    expect(line.querySelector(".activity-what")!.textContent).toBe(
      "from hqc/build-bot",
    );
    expect(line.querySelector(".activity-timer")!.textContent).toBe("1:24");
    t.apply(result(2, "q"));
    render(null, root);
    expect(draw("running", t)!.textContent).toBe("Agent is working");
  });

  it('a call the runtime runs in the foreground: "Run in background", which moves it', () => {
    const t = new Timeline();
    t.apply(call(1, "toolu_a", "Bash", { command: "sleep 30" }));
    const moved = vi.fn();
    const line = (foreground: string[] | undefined) => {
      root = document.createElement("div");
      document.body.appendChild(root);
      act(() =>
        render(
          <ActivityLine
            state="running"
            timeline={t}
            now={() => NOW}
            foreground={foreground}
            onBackground={moved}
          />,
          root,
        ),
      );
      return root.querySelector<HTMLButtonElement>(".activity-background");
    };
    // A running call the worker did not report as a foreground one: no button.
    expect(line(undefined)).toBeNull();
    render(null, root);
    expect(line(["toolu_b"])).toBeNull();
    render(null, root);
    const button = line(["toolu_b", "toolu_a"])!;
    expect(button.textContent).toBe("Run in background");
    act(() => button.click());
    expect(moved).toHaveBeenCalledWith("toolu_a");
  });

  it("queued and waiting: their line; waiting with its request on screen: none", () => {
    expect(draw("queued")!.textContent).toBe("Input queued");
    render(null, root);
    expect(draw("waiting")!.textContent).toBe("Waiting for approval");
    render(null, root);
    const t = new Timeline();
    t.apply(ev("event.approval.request", withSeq(1)));
    expect(draw("waiting", t)).toBeNull();
  });

  it("nothing in other states", () => {
    for (const state of [
      "idle",
      "error",
      "archived",
      "worker_offline",
      "locked_vscode",
      "unknown",
    ]) {
      expect(draw(state)).toBeNull();
      render(null, root);
    }
  });
});

describe("worker's service session (T17, SPEC §10.2)", () => {
  const log = (seq: number, level: string, text: string) =>
    ev("event.system", (e) => {
      e.seq = seq;
      e.ts = "2026-10-22T10:00:07Z";
      e.payload = { text, code: "worker_log", level };
    });
  const workerRequest = (seq: number) =>
    ev("event.approval.request", (e) => {
      const r = raw("worker.worker.approval.request").payload;
      e.seq = seq;
      e.author = { kind: "system" };
      e.payload = {
        request_id: r.request_id,
        tool: r.kind,
        description: r.description,
        options: [
          { id: "once", label: "Allow", canonical: "once" },
          { id: "deny", label: "Deny", canonical: "deny" },
        ],
        expires_at: "2026-10-22T11:00:00Z",
      };
    });

  it("renders a log line with its time, level and text as is", () => {
    const t = new Timeline();
    t.apply(
      log(
        1,
        "warn",
        "hqc_worker::connection: hub connection lost: closed_by_hub *x*",
      ),
    );
    t.apply(log(2, "error", "outbox write failed"));
    show(t);
    const lines = [...root.querySelectorAll(".line-worker-log")];
    expect(lines).toHaveLength(2);
    expect(lines[0].classList.contains("level-warn")).toBe(true);
    expect(lines[0].querySelector(".log-level")!.textContent).toBe("warning");
    expect(lines[0].querySelector(".log-time")!.getAttribute("datetime")).toBe(
      "2026-10-22T10:00:07Z",
    );
    expect(lines[0].querySelector(".log-time")!.textContent).toMatch(
      /^\d\d:\d\d:\d\d$/,
    );
    // Not Markdown: underscores and stars stay.
    expect(lines[0].querySelector(".log-text")!.textContent).toBe(
      "hqc_worker::connection: hub connection lost: closed_by_hub *x*",
    );
    expect(lines[1].classList.contains("level-error")).toBe(true);
    expect(lines[1].querySelector(".log-level")!.textContent).toBe("error");
  });

  it("shows the confirmation of a new folder with once / deny, no text answer, and sends the answer", async () => {
    const t = new Timeline();
    t.apply(workerRequest(1));
    const { actions } = show(t);
    expect(root.querySelector(".perm-title")!.textContent).toBe(
      "The worker asks for permission",
    );
    expect(root.querySelector(".perm-sub")!.textContent).toBe(
      "sessions in a new folder",
    );
    expect(root.querySelector(".perm-what")!.textContent).toBe(
      raw("worker.worker.approval.request").payload.description,
    );
    const buttons = optionButtons();
    expect(buttons.map((b) => b.childNodes[0].textContent)).toEqual([
      CANONICAL_LABELS.once,
      CANONICAL_LABELS.deny,
    ]);
    expect(buttonByText("Reply with text instead")).toBeUndefined();
    await act(async () => buttons[0].click());
    expect(actions.approve).toHaveBeenCalledWith(
      raw("worker.worker.approval.request").payload.request_id,
      "once",
    );
  });

  it("names an agent's move of its session in the header, outside the service session", () => {
    const t = new Timeline();
    t.apply(
      ev("event.approval.request", (e) => {
        e.seq = 1;
        e.author = { kind: "system" };
        e.payload = {
          request_id: "move-1",
          tool: "hqc_move",
          description:
            "The agent asks to move this session into a new folder (made if you allow it):\n/w/foo",
          options: [
            { id: "once", label: "Allow", canonical: "once" },
            { id: "deny", label: "Deny", canonical: "deny" },
          ],
        };
      }),
    );
    show(t);
    expect(root.querySelector(".perm-title")!.textContent).toBe(
      "The agent asks to move this session",
    );
    render(null, root);
    show(t, { system: true });
    expect(root.querySelector(".perm-title")!.textContent).toBe(
      "The worker asks for permission",
    );
  });

  it("asks whether the session moves after its transcript (SPEC §9.2)", () => {
    const t = new Timeline();
    t.apply(
      ev("event.approval.request", (e) => {
        e.seq = 1;
        e.author = { kind: "system" };
        e.payload = {
          request_id: "follow-1",
          tool: "session.follow",
          description:
            "The transcript of this session is continued in /w/popcorn, of project popcorn, in environment work (its folder now, /w/home, is in environment default). Allowed, the session moves there, and its next start runs the agent there.",
          options: [
            { id: "once", label: "Allow", canonical: "once" },
            { id: "deny", label: "Deny", canonical: "deny" },
          ],
        };
      }),
    );
    show(t);
    expect(root.querySelector(".perm-title")!.textContent).toBe(
      "Move this session after its transcript?",
    );
    expect(root.querySelector(".perm-sub")!.textContent).toBe(
      "move after the transcript",
    );
  });

  it('"requests only" keeps the request cards only', () => {
    const t = new Timeline();
    t.apply(log(1, "warn", "a"));
    t.apply(workerRequest(2));
    t.apply(log(3, "error", "b"));
    const items = onlyRequests(t.items());
    expect(items).toHaveLength(1);
    expect(items[0].kind === "event" && items[0].event.type).toBe(
      "approval.request",
    );
  });
});

describe("fork of a transcript (T20, SPEC §9.3)", () => {
  it("draws session_forked as a warning line with an icon", () => {
    const forked = raw("worker.mirror.event.forked").payload.event;
    const t = new Timeline();
    t.apply(
      ev("event.system", (e) => {
        e.seq = 1;
        e.payload = forked.payload;
      }),
    );
    show(t);
    const line = root.querySelector(".line-system-session_forked")!;
    expect(line.classList.contains("line-warning")).toBe(true);
    expect(line.querySelector(".warn-icon")!.getAttribute("aria-label")).toBe(
      "Warning",
    );
    expect(line.querySelector("b")!.textContent).toBe("Fork");
    expect(line.textContent).toContain("1 turn from hqc is not in its context");
    // Other system lines stay plain.
    render(null, root);
    const t2 = new Timeline();
    t2.apply(ev("event.system", withSeq(1)));
    show(t2);
    expect(root.querySelector(".line-warning")).toBeNull();
  });
});

describe("the feed like the Claude Code tab", () => {
  const system = (seq: number, code: string, text: string) =>
    ev("event.system", (e) => {
      e.seq = seq;
      e.payload = { code, text };
    });

  it("folds a background task's notification into a line with its whole text as the peek", () => {
    const t = new Timeline();
    t.apply(
      system(
        1,
        "task_notification",
        'Background command "npm test" completed (exit code 0)\nAll 12 suites passed',
      ),
    );
    show(t);
    const fold = root.querySelector("details.line-system-task_notification")!;
    expect(fold.querySelector("summary .fold-label")!.textContent).toBe(
      "Background task ·",
    );
    expect(fold.querySelector("summary .fold-peek")!.textContent).toBe(
      'Background command "npm test" completed (exit code 0) All 12 suites passed',
    );
    expect(fold.querySelector(".fold-text")!.textContent).toBe(
      'Background command "npm test" completed (exit code 0)All 12 suites passed',
    );
    expect(root.querySelector(".msg-agent")).toBeNull();
  });

  it("says how a task ended and what it spent", () => {
    const t = new Timeline();
    t.apply(ev("event.system.task_notification", withSeq(1)));
    t.apply(
      ev("event.system.task_notification", (e) => {
        e.seq = 2;
        e.event_id = "0199f3a2-7c10-7000-8000-0000000001d2";
        e.payload.status = "stopped";
        delete e.payload.usage;
      }),
    );
    t.apply(
      ev("event.system.task_notification", (e) => {
        e.seq = 3;
        e.event_id = "0199f3a2-7c10-7000-8000-0000000001d3";
        e.payload.status = "completed";
      }),
    );
    show(t);
    const folds = [...root.querySelectorAll("details.line-system-task_notification")];
    expect(folds.map((f) => f.querySelector(".fold-label")!.textContent)).toEqual([
      "Background task failed ·",
      "Background task stopped ·",
      "Background task ·",
    ]);
    expect(folds.map((f) => f.querySelector(".fold-stats")?.textContent ?? null)).toEqual([
      "· 155.3k tokens · 188 tools · 4m 13s",
      null,
      "· 155.3k tokens · 188 tools · 4m 13s",
    ]);
  });

  it("peeks at a subagent's Markdown report without its marks and shows it open as Markdown", () => {
    const report =
      "### Summary\n**Asked:** close two findings in `redraw()`.\n\n- first fixed\n- second fixed";
    const t = new Timeline();
    t.apply(system(1, "task_notification", report));
    show(t);
    const fold = root.querySelector("details.line-system-task_notification")!;
    expect(fold.querySelector("summary .fold-peek")!.textContent).toBe(
      "Summary Asked: close two findings in redraw(). first fixed second fixed",
    );
    const body = fold.querySelector(".fold-text.md")!;
    expect(body.querySelector("h3")!.textContent).toBe("Summary");
    expect(body.querySelector("p strong")!.textContent).toBe("Asked:");
    expect(body.querySelector("p code")!.textContent).toBe("redraw()");
    expect([...body.querySelectorAll("ul li")].map((li) => li.textContent)).toEqual([
      "first fixed",
      "second fixed",
    ]);
  });

  it("draws the compaction of the context as a divider", () => {
    const t = new Timeline();
    t.apply(system(1, "context_compacted", "Context compacted"));
    show(t);
    const line = root.querySelector(".line.line-system-context_compacted")!;
    expect(line.textContent).toBe("Context compacted");
    expect(root.querySelector("details.line-system-context_compacted")).toBeNull();
  });

  it("folds the summary of a compaction under a Context compacted line", () => {
    const summary =
      "This session is being continued from a previous conversation.\n\nSummary:\n1. Tests pass";
    const t = new Timeline();
    t.apply(system(1, "context_compacted", summary));
    show(t);
    const fold = root.querySelector("details.line-fold.line-system-context_compacted")!;
    expect(fold.querySelector("summary .fold-label")!.textContent).toBe("Context compacted");
    expect(fold.querySelector("summary .fold-peek")).toBeNull();
    const body = fold.querySelector(".fold-text.md")!;
    expect([...body.querySelectorAll(":scope > p")].map((p) => p.textContent)).toEqual([
      "This session is being continued from a previous conversation.",
      "Summary:",
    ]);
    expect(body.querySelector("ol li")!.textContent).toBe("Tests pass");
    expect(root.querySelector(".line.line-system-context_compacted")).toBeNull();
  });

  it("says how a compaction came and what it freed when the runtime did (SPEC 0.179)", () => {
    const t = new Timeline();
    t.apply(ev("event.system.context_compacted"));
    t.apply(
      ev("event.system.context_compacted", (e) => {
        e.seq = 45;
        e.payload = { code: "context_compacted", text: "Context compacted", trigger: "manual", pre_tokens: 9000 };
      }),
    );
    show(t);
    expect(
      root.querySelector("details.line-system-context_compacted summary .fold-label")!.textContent,
    ).toBe("Context compacted · auto · 153k tokens freed");
    expect(root.querySelector(".line.line-system-context_compacted")!.textContent).toBe(
      "Context compacted · manual",
    );
  });

  it("hides what a retraction takes back, also when it loads after the retraction (SPEC 0.179)", () => {
    const refused = ev("event.agent.message", (e) => {
      e.seq = 1;
      e.event_id = "0199f3a2-7c10-7000-8000-0000000001aa";
      e.payload = { text: "I can't help with that.", final: false };
    });
    const call1 = call(2, "toolu_r", "Bash", { command: "ls" });
    const retraction = ev("event.retraction", (e) => {
      e.seq = 4;
      e.payload.event_ids = [refused.event_id, call1.event_id];
    });
    const t = new Timeline();
    t.apply(refused);
    t.apply(call1);
    t.apply(ev("event.system", (e) => {
      e.seq = 3;
      e.payload = { code: "model_refusal_fallback", text: "claude-opus-5-5 → claude-sonnet-5" };
    }));
    t.apply(retraction);
    t.apply(agentText(5, "Here it is.", true));
    show(t);
    expect(root.textContent).not.toContain("I can't help with that.");
    expect(root.querySelector(".tool-group")).toBeNull();
    expect(root.textContent).toContain("claude-opus-5-5 → claude-sonnet-5");
    expect(root.textContent).toContain("Here it is.");
    // An older page brought after the retraction: still hidden.
    const late = new Timeline();
    late.apply(retraction);
    late.apply(refused);
    expect(late.events().map((e) => e.seq)).toEqual([]);
  });

  it("shows a reply Claude Code wrote itself as an error-style service line, not a message", () => {
    const t = new Timeline();
    t.apply(
      ev("event.agent.message", (e) => {
        e.seq = 1;
        e.payload = { text: "Credit balance is too low", final: true, runtime: true };
      }),
    );
    show(t);
    const line = root.querySelector(".line.line-system.line-runtime")!;
    expect(line.textContent).toBe("Credit balance is too low");
    expect(root.querySelector(".msg-agent")).toBeNull();
  });

  it("shows an answer an interrupt cut off muted, marked interrupted after its time", () => {
    const t = new Timeline();
    t.apply(ev("event.agent.message.interrupted"));
    t.apply(agentText(44, "All done.", true));
    show(t);
    const [cut, whole] = [...root.querySelectorAll(".msg-agent")];
    expect(cut.classList.contains("msg-interrupted")).toBe(true);
    expect(cut.querySelector(".msg-time .interrupted")!.textContent).toBe(" · interrupted");
    expect(whole.classList.contains("msg-interrupted")).toBe(false);
    expect(whole.querySelector(".interrupted")).toBeNull();
  });

  const clarify = () =>
    ev("event.clarify.request", (e) => {
      e.seq = 1;
      e.payload.questions[0].header = "Focus";
      e.payload.questions[0].option_descriptions = [
        "axum + Postgres hub work",
        "",
      ];
      e.payload.questions[1].header = "Checks";
    });
  const type = (field: HTMLInputElement, value: string) =>
    act(() => {
      field.value = value;
      field.dispatchEvent(new Event("input", { bubbles: true }));
    });

  it("shows a question's header and each option's description under its label", () => {
    const t = new Timeline();
    t.apply(clarify());
    show(t);
    const qs = root.querySelectorAll(".card-clarify fieldset");
    expect(qs[0].querySelector("legend .question-header")!.textContent).toBe(
      "Focus",
    );
    expect(qs[0].querySelector("legend .question-text")!.textContent).toBe(
      "Which area should I focus on first?",
    );
    const choices = [...qs[0].querySelectorAll(".choice:not(.choice-other)")];
    expect(
      choices.map((c) => c.querySelector(".choice-label")!.textContent),
    ).toEqual(["Hub", "Worker"]);
    expect(choices[0].querySelector(".choice-desc")!.textContent).toBe(
      "axum + Postgres hub work",
    );
    expect(choices[1].querySelector(".choice-desc")).toBeNull();
    // Checkboxes for a multi question, Other among them; a question without options has
    // its text field only.
    expect(qs[1].querySelectorAll("input[type=checkbox]")).toHaveLength(4);
    expect(qs[2].querySelector(".question-header")).toBeNull();
    expect(qs[2].querySelector(".choice-other")).toBeNull();
  });

  it("sends the text of Other as the answer, for a multi question beside the checked labels", async () => {
    const t = new Timeline();
    t.apply(clarify());
    const { actions } = show(t);
    const qs = root.querySelectorAll(".card-clarify fieldset");
    const hub = () => root.querySelector<HTMLInputElement>('input[value="Hub"]')!;
    await act(() => hub().click());
    // Typing into Other chooses it in place of Hub.
    await type(
      qs[0].querySelector<HTMLInputElement>(".choice-other input[type=text]")!,
      "Both, hub first",
    );
    expect(hub().checked).toBe(false);
    expect(
      qs[0].querySelector<HTMLInputElement>(".choice-other input[type=radio]")!
        .checked,
    ).toBe(true);
    await act(() =>
      root.querySelector<HTMLInputElement>('input[value="clippy"]')!.click(),
    );
    await type(
      qs[1].querySelector<HTMLInputElement>(".choice-other input[type=text]")!,
      "miri",
    );
    await type(qs[2].querySelector<HTMLInputElement>("input[type=text]")!, "no");
    await act(async () =>
      root
        .querySelector<HTMLButtonElement>(".card-clarify button[type=submit]")!
        .click(),
    );
    expect(actions.answer).toHaveBeenCalledWith("req-2", {
      q1: "Both, hub first",
      q2: ["clippy", "miri"],
      q3: "no",
    });
  });

  it("an answered question shows a typed answer in Other", () => {
    const t = new Timeline();
    t.apply(clarify());
    t.apply(
      ev("event.clarify.response", (e) => {
        e.seq = 2;
        e.payload.answers = { q1: "Both, hub first", q2: ["fmt", "miri"] };
      }),
    );
    show(t);
    const qs = root.querySelectorAll(".card-clarify fieldset");
    const other = (i: number, sel: string) =>
      qs[i].querySelector<HTMLInputElement>(`.choice-other ${sel}`)!;
    expect(other(0, "input[type=radio]").checked).toBe(true);
    expect(other(0, "input[type=text]").value).toBe("Both, hub first");
    expect(
      qs[1].querySelector<HTMLInputElement>('input[value="fmt"]')!.checked,
    ).toBe(true);
    expect(other(1, "input[type=checkbox]").checked).toBe(true);
    expect(other(1, "input[type=text]").value).toBe("miri");
  });

  // The worker writes `args_preview` as compact JSON with the keys sorted (serde_json).
  const approvalFor = (seq: number, tool: string) =>
    ev("event.approval.request", (e) => {
      e.seq = seq;
      e.payload.request_id = `req-${seq}`;
      e.payload.tool = tool;
      e.payload.description = "/Users/me/work/hqc/src/main.rs";
      delete e.payload.policy_hint;
      delete e.payload.rule_suggestions;
    });
  const rawCall = (seq: number, tool: string, args_preview: string) =>
    ev("event.tool.call", (e) => {
      e.seq = seq;
      e.payload = { call_id: `c${seq}`, tool, args_preview };
    });

  it("shows an Edit as a diff of old_string and new_string", () => {
    const t = new Timeline();
    t.apply(
      call(1, "c1", "Edit", {
        file_path: "/Users/me/work/hqc/src/main.rs",
        new_string: "fn main() {\n    run();\n}",
        old_string: "fn main() {\n    todo!();\n}",
        replace_all: false,
      }),
    );
    t.apply(approvalFor(2, "Edit"));
    show(t);
    const diff = root.querySelector(".perm-change .md-code-diff")!;
    expect(diff.querySelector("pre")!.textContent).toBe(
      " fn main() {\n-    todo!();\n+    run();\n }",
    );
    expect(
      [...diff.querySelectorAll(".diff-del")].map((l) => l.textContent),
    ).toEqual(["-    todo!();"]);
    expect(
      [...diff.querySelectorAll(".diff-add")].map((l) => l.textContent),
    ).toEqual(["+    run();"]);
    expect(root.querySelector(".perm-cut")).toBeNull();
  });

  it("shows each edit of a MultiEdit and the content of a Write", () => {
    const t = new Timeline();
    t.apply(
      call(1, "c1", "MultiEdit", {
        edits: [
          { new_string: "a2", old_string: "a1" },
          { new_string: "b2", old_string: "---\nb1", replace_all: true },
        ],
        file_path: "/x/y.md",
      }),
    );
    t.apply(approvalFor(2, "MultiEdit"));
    t.apply(call(3, "c3", "Write", { content: "line 1\nline 2", file_path: "/x/z.md" }));
    t.apply(approvalFor(4, "Write"));
    show(t);
    const cards = [...root.querySelectorAll(".perm")];
    expect(cards).toHaveLength(2);
    const edits = [...cards[0].querySelectorAll(".perm-change .md-code")];
    expect(edits.map((d) => d.querySelector("pre")!.textContent)).toEqual([
      "-a1\n+a2",
      "----\n-b1\n+b2",
    ]);
    expect(
      edits.map((d) => d.querySelector(".md-code-lang")!.textContent),
    ).toEqual(["Edit 1 of 2", "Edit 2 of 2"]);
    // A removed `---` line is a removed line, not a file header.
    expect(edits[1].querySelector(".diff-file")).toBeNull();
    expect(edits[1].querySelectorAll(".diff-del")).toHaveLength(2);
    const content = cards[1].querySelector(".perm-change .md-code")!;
    expect(content.classList.contains("md-code-diff")).toBe(false);
    expect(content.querySelector(".md-code-lang")!.textContent).toBe("Content");
    expect(content.querySelector("pre")!.textContent).toBe("line 1\nline 2");
  });

  it("shows what a preview cut at 4 KB has and says it is cut", () => {
    const full = JSON.stringify({
      file_path: "/x/y.ts",
      new_string: "fixed();",
      old_string: "old line\n".repeat(600),
      replace_all: false,
    });
    const t = new Timeline();
    t.apply(rawCall(1, "Edit", full.slice(0, 4096)));
    t.apply(approvalFor(2, "Edit"));
    show(t);
    const pre = root.querySelector(".perm-change pre")!.textContent!;
    expect(pre.startsWith("-old line\n-old line\n")).toBe(true);
    expect(pre.endsWith("\n+fixed();")).toBe(true);
    expect(root.querySelector(".perm-cut")!.textContent).toBe(
      "The preview is cut at 4 KB: the rest of the change is not shown.",
    );
  });

  it("makes a path relative to the session's folder only where the path starts", () => {
    expect(relativePaths("/backup/home/u/p/f.rs", "/home/u/p")).toBe("/backup/home/u/p/f.rs");
    expect(
      relativePaths("cat /home/u/p/a.rs '/home/u/p/b c' --out=/home/u/p/o", "/home/u/p/"),
    ).toBe("cat a.rs 'b c' --out=o");
    expect(relativePaths("/home/u/p/f.rs", "/home/u/p")).toBe("f.rs");
  });

  it("reads a JSON preview cut anywhere: an open string ends, a dangling key goes", () => {
    expect(cutJson('{"a":"x\\')).toEqual({ value: { a: "x" }, cut: true });
    expect(cutJson('{"a":"x\\u00')).toEqual({ value: { a: "x" }, cut: true });
    expect(cutJson('{"a":"x\\\\')).toEqual({ value: { a: "x\\" }, cut: true });
    expect(cutJson('{"edits":[{"new_string":"ab","old_st')).toEqual({
      value: { edits: [{ new_string: "ab" }] },
      cut: true,
    });
    expect(cutJson('{"a":1,"b":tru')).toEqual({ value: { a: 1 }, cut: true });
    expect(cutJson('{"a":"x",')).toEqual({ value: { a: "x" }, cut: true });
    expect(cutJson('{"a":"x"}')).toEqual({ value: { a: "x" }, cut: false });
    expect(cutJson("not json")).toBeUndefined();
  });

  it("writes a call's arguments as indented JSON, the lines of a string under its key", () => {
    const args = {
      description: "Wait",
      n: 2,
      ok: true,
      none: null,
      tags: ["a", { b: [] }],
      empty: {},
      prompt: 'one\n\ntwo "q"\\n',
    };
    expect(argsText(JSON.stringify(args))).toEqual({
      text: [
        "{",
        '  "description": "Wait",',
        '  "n": 2,',
        '  "ok": true,',
        '  "none": null,',
        '  "tags": [',
        '    "a",',
        "    {",
        '      "b": []',
        "    }",
        "  ],",
        '  "empty": {},',
        '  "prompt": "one',
        "",
        '    two \\"q\\"\\\\n"',
        "}",
      ].join("\n"),
      cut: false,
    });
    // A string that ends with a line break: its closing quote stays under the key.
    expect(argsText('{"a":"x\\n"}')).toEqual({ text: '{\n  "a": "x\n    "\n}', cut: false });
    // Cut at 4 KB: as far as it goes.
    expect(argsText('{"prompt":"a\\nb')).toEqual({ text: '{\n  "prompt": "a\n    b"\n}', cut: true });
    expect(argsText("not json")).toBeUndefined();
  });

  it("the group's header: its tools, or its one call; MCP tools by name and server, paths relative to the session's folder", async () => {
    const t = new Timeline();
    t.apply(call(1, "a", "Read", { file_path: "/Users/me/work/hqc/src/main.rs" }));
    t.apply(result(2, "a"));
    t.apply(
      call(3, "b", "mcp__plugin_chrome-devtools-mcp_chrome-devtools__take_screenshot", {
        filePath: "/tmp/s.png",
      }),
    );
    t.apply(result(4, "b"));
    t.apply(
      call(5, "c", "Bash", {
        command: "cat /Users/me/work/hqc/Cargo.toml /Users/me/work/hqc-old/x",
      }),
    );
    t.apply(result(6, "c"));
    t.apply(agentText(7, "Next.", false));
    t.apply(call(8, "d", "mcp__gitea__list_issues", { query: "label:bug" }));
    t.apply(result(9, "d"));
    show(t, { workspaceRoot: "/Users/me/work/hqc" });
    const [g, g2] = [...root.querySelectorAll(".tool-group")];
    expect(g.querySelector(".tool-group-head .tool-peek")!.textContent).toBe(
      "(Read, chrome-devtools/take_screenshot, Bash)",
    );
    expect(g2.querySelector(".tool-group-head .tool-peek")!.textContent).toBe(
      "list_issues (gitea) label:bug",
    );
    await act(async () =>
      g.querySelector<HTMLButtonElement>(".tool-group-head")!.click(),
    );
    const rows = [...g.querySelectorAll(".tool-row summary")];
    expect(rows[0].querySelector(".tool-line")!.textContent).toBe("src/main.rs");
    expect(rows[1].querySelector(".tool-name")!.textContent).toBe(
      "take_screenshot (chrome-devtools)",
    );
    expect(rows[2].querySelector(".tool-line")!.textContent).toBe(
      "cat Cargo.toml /Users/me/work/hqc-old/x",
    );
  });

  it("shows the user's own message as typed: no Markdown, line breaks kept", () => {
    const text = "**not bold** and `code`\n- not a list";
    const t = new Timeline();
    t.apply(
      ev("event.user.message", (e) => {
        e.payload.text = text;
        e.payload.artifact_ids = [];
      }),
    );
    t.addPending("op-2", "_still_ typed");
    show(t);
    const [sent, pending] = [...root.querySelectorAll(".msg-user .msg-bubble")];
    expect(sent.querySelector(".msg-text")!.textContent).toBe(text);
    expect(pending.querySelector(".msg-text")!.textContent).toBe("_still_ typed");
    expect(root.querySelector(".msg-user strong, .msg-user em, .msg-user li, .msg-user code")).toBeNull();
  });
});
