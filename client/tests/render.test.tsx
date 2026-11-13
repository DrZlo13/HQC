// @vitest-environment happy-dom
// Rendering of the chat feed from examples/wire (SPEC §4, §10.1), T26 markup: tool groups,
// the permission card with its hotkeys and text answer, messages, service lines.

import { afterEach, describe, expect, it, vi } from "vitest";
import { render } from "preact";
import { act } from "preact/test-utils";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { parseEvent, parseServerFrame, type HqcEvent } from "../src/proto";
import { Timeline } from "../src/timeline";
import {
  ActivityLine,
  ChatItems,
  onlyRequests,
  type ChatActions,
} from "../src/ui/cards";
import { CANONICAL_LABELS } from "../src/ui/labels";
import { buildFeed, cutJson, relativePaths } from "../src/ui/feed";

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
  // Transport only: the stream text, the subagents above the input (chat.test.tsx), and the
  // token rate, which reaches the client as the session's `output_per_s`.
  const names = readdirSync(WIRE)
    .filter(
      (f) =>
        f.startsWith("event.") &&
        f !== "event.agent.delta.json" &&
        f !== "event.subagents.live.json" &&
        f !== "event.tokens.rate.json",
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

  it("an older page loaded behind the live tail ends no stream", () => {
    const t = new Timeline();
    t.apply(ev("event.agent.message", withSeq(50)));
    t.apply(ev("event.agent.delta"));
    t.applyAll([ev("event.agent.message", withSeq(10)), ev("event.status", withSeq(11))]);
    expect(t.streaming).toBe("Running");
  });
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

  it("a final answer keeps its usage line in its block", () => {
    const t = new Timeline();
    t.applyAll([input(1, true), answer(2), ev("event.usage", withSeq(3))]);
    const blocks = buildFeed(t.items(), t);
    expect(blocks.map((b) => b.kind)).toEqual(["agent", "item"]);
    const [first] = blocks;
    expect(first.kind === "agent" ? first.usage?.seq : undefined).toBe(3);
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
    expect(msg.querySelector(".msg-bubble.msg-noted > .msg-note")!.textContent).toBe(
      "instead of the request",
    );
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
    show(t);
    const g = root.querySelector(".tool-group")!;
    await act(async () =>
      g.querySelector<HTMLButtonElement>(".tool-group-head")!.click(),
    );
    const [edit, multi, bash] = [...g.querySelectorAll("details.tool-row")];
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
