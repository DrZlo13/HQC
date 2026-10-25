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
  const names = readdirSync(WIRE)
    .filter((f) => f.startsWith("event.") && f !== "event.agent.delta.json")
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
    // "Allow" first with ⏎, "Deny" with Esc.
    expect(buttons[0].getAttribute("data-canonical")).toBe("once");
    expect(buttons[0].querySelector(".kbd")!.textContent).toBe("⏎");
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
    expect(msg.querySelector(".msg-bubble")!.textContent).toBe(
      "Build first, tests later",
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
    expect(qs[0].querySelectorAll("input[type=radio]").length).toBe(2);
    expect(qs[1].querySelectorAll("input[type=checkbox]").length).toBe(3);
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

  it("shows a result without its call as a group of its own", () => {
    const t = new Timeline();
    t.apply(ev("event.tool.result"));
    show(t);
    expect(root.querySelectorAll(".tool-group")).toHaveLength(1);
  });

  it("captions a group with the interim message before it, cut to one line", async () => {
    const t = new Timeline();
    t.apply(
      agentText(
        1,
        "Rebuilding the project.\nThen running the **tests**.",
        false,
      ),
    );
    t.apply(call(2, "a", "Bash", { command: "ninja -C build" }));
    t.apply(result(4, "a"));
    t.apply(call(5, "b", "Bash", { command: "ctest" }));
    t.apply(result(40, "b"));
    t.apply(agentText(41, "Done.", true));
    show(t);
    const g = root.querySelector(".tool-group")!;
    expect(g.querySelector(".tool-caption")!.textContent).toBe(
      "Rebuilding the project. Then running the tests .",
    );
    expect(g.querySelector(".tool-stats")!.textContent).toBe(
      "· 2 commands · 38 s",
    );
    expect(g.querySelector(".mark-ok")).not.toBeNull();
    // The interim message is the caption, not a message of its own.
    expect(root.querySelectorAll(".msg-agent")).toHaveLength(1);
    await act(async () =>
      g.querySelector<HTMLButtonElement>(".tool-group-head")!.click(),
    );
    expect(g.querySelector(".tool-group-text strong")!.textContent).toBe(
      "tests",
    );
    const rows = [...g.querySelectorAll(".tool-row summary")].map(
      (s) => s.querySelector(".tool-line")!.textContent,
    );
    expect(rows).toEqual(["ninja -C build", "ctest"]);
  });

  it('without an interim message says "N commands · time"; other tools show their argument', async () => {
    const t = new Timeline();
    t.apply(agentText(1, "Done.", true));
    t.apply(call(2, "a", "Read", { file_path: "Cargo.toml" }));
    t.apply(result(3, "a"));
    t.apply(call(4, "b", "Grep", { pattern: "TODO" }));
    t.apply(result(5, "b", true));
    show(t);
    const g = root.querySelector(".tool-group")!;
    expect(g.querySelector(".tool-caption")).toBeNull();
    expect(g.querySelector(".tool-stats")!.textContent).toBe(
      "2 commands · 3 s",
    );
    expect(g.classList.contains("tool-error")).toBe(true);
    await act(async () =>
      g.querySelector<HTMLButtonElement>(".tool-group-head")!.click(),
    );
    const rows = [...g.querySelectorAll(".tool-row summary")];
    expect(rows[0].querySelector(".tool-name")!.textContent).toBe("Read");
    expect(rows[0].querySelector(".tool-line")!.textContent).toBe("Cargo.toml");
    expect(rows[1].querySelector(".mark-error")).not.toBeNull();
  });

  it("a final answer is not a caption, and the next message closes the group", () => {
    const t = new Timeline();
    t.apply(call(1, "a", "Bash", { command: "ls" }));
    t.apply(result(2, "a"));
    t.apply(agentText(3, "Looking further.", false));
    t.apply(call(4, "b", "Bash", { command: "pwd" }));
    show(t);
    const groups = root.querySelectorAll(".tool-group");
    expect(groups).toHaveLength(2);
    expect(groups[0].querySelector(".tool-caption")).toBeNull();
    expect(groups[1].querySelector(".tool-caption")!.textContent).toBe(
      "Looking further.",
    );
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
    expect(root.querySelector(".msg-failed")!.textContent).toContain(
      "HTTP 502",
    );
    await act(async () => buttonByText("Retry")!.click());
    expect(actions.resend).toHaveBeenCalledWith("op-9");
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
      " · 1.2k → 340 tokens · cache 9k · $0.012",
    );
    expect(root.querySelector(".line-usage")).toBeNull();
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
