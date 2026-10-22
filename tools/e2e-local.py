#!/usr/bin/env python3
"""REST side of tools/e2e-local.sh (SPEC §19.1 on one machine). Standard library only.

Subcommands (hub URL from $HUB, a loopback dev-mode hub):
  wait-health                     wait until GET /health answers
  vapid <public_key>              GET /api/push/vapid returns this key
  client                          GET / is the web client (HQC_CLIENT_DIR)
  confirm <enroll.log>            read the code printed by `hqc-worker enroll`, confirm it
  wait-online <worker_id_file>    wait until the worker is online in GET /api/workers
  scenario <workspace> <out.json> session, input, approval (once), final DONE, idle
  report <out.json>               events with seq and type, and the checks
"""

import json
import os
import re
import sys
import time
import urllib.error
import urllib.request

HUB = os.environ["HUB"].rstrip("/")
MODEL = os.environ.get("E2E_MODEL", "claude-haiku-5-5")
PROMPT = "Run the shell command: touch hqc-e2e && echo hqc-e2e. Then reply with exactly: DONE"


def log(*a):
    print("[e2e]", *a, file=sys.stderr, flush=True)


def call(method, path, body=None, ok=(200, 201, 204)):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(HUB + path, data=data, method=method)
    if data is not None:
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            status, raw = r.status, r.read()
    except urllib.error.HTTPError as e:
        status, raw = e.code, e.read()
    if status not in ok:
        raise SystemExit(f"{method} {path}: HTTP {status} {raw[:500]!r}")
    return json.loads(raw) if raw else None


def until(what, timeout, fn, every=0.5):
    deadline = time.monotonic() + timeout
    while True:
        try:
            v = fn()
        except (urllib.error.URLError, ConnectionError, OSError):
            v = None
        if v:
            return v
        if time.monotonic() > deadline:
            raise SystemExit(f"timeout ({timeout}s) waiting for {what}")
        time.sleep(every)


def wait_health():
    def up():
        with urllib.request.urlopen(HUB + "/health", timeout=2) as r:
            return r.status == 200
    until("hub /health", 60, up)
    log("hub is up")


def vapid(expected):
    got = call("GET", "/api/push/vapid")["public_key"]
    if got != expected:
        raise SystemExit(f"/api/push/vapid: {got!r}, expected {expected!r}")
    log("push is on, vapid key served")


def client():
    with urllib.request.urlopen(HUB + "/", timeout=10) as r:
        ct, body = r.headers.get("Content-Type", ""), r.read().decode()
    if not ct.startswith("text/html") or "<html" not in body.lower():
        raise SystemExit(f"GET /: {ct!r}, not the client's index.html")
    with urllib.request.urlopen(HUB + "/sw.js", timeout=10) as r:
        r.read()
    log("web client served from HQC_CLIENT_DIR")


def confirm(enroll_log):
    def code():
        try:
            text = open(enroll_log).read()
        except FileNotFoundError:
            return None
        m = re.search(r"Enrollment code: ([A-Z0-9]{4}-[A-Z0-9]{4})", text)
        return m and m.group(1)
    c = until("enrollment code in " + enroll_log, 30, code)
    r = call("POST", "/api/workers/enroll/confirm", {"code": c.lower(), "name": "e2e-local"})
    log(f"enrollment {c} confirmed: worker {r['worker_id']}")
    print(r["worker_id"])


def wait_online(worker_id_file):
    wid = open(worker_id_file).read().strip()

    def online():
        ws = call("GET", "/api/workers")["workers"]
        return any(w["id"] == wid and w["status"] == "online" for w in ws)
    until(f"worker {wid} online", 60, online)
    log(f"worker {wid} is online")


def scenario(workspace, out):
    me = call("GET", "/api/me")
    project = me["personal_project_id"]
    s = call("POST", f"/api/projects/{project}/sessions",
             {"title": "e2e-local", "runtime": "claude_code", "model": MODEL})
    sid = s["id"]
    log(f"session {sid} in project {project}, runtime {s['agent']['runtime']}")
    result = {"session_id": sid, "events": [], "checks": {}}

    def finish(code):
        # Stored as they are now (delivery of the input changes after it is stored).
        result["events"] = call("GET", f"/api/sessions/{sid}/events?after_seq=0&limit=500")["events"]
        result["session"] = call("GET", f"/api/sessions/{sid}")
        with open(out, "w") as f:
            json.dump(result, f, indent=2)
        return code

    events, last, answered = result["events"], 0, set()
    try:
        call("POST", f"/api/sessions/{sid}/input",
             {"text": PROMPT, "artifact_ids": [], "client_op_id": "e2e-1"})

        def poll():
            nonlocal last
            page = call("GET", f"/api/sessions/{sid}/events?after_seq={last}&limit=500")
            for e in page["events"]:
                events.append(e)
                last = max(last, e["seq"])
                log(f"  seq {e['seq']:>3} {e['type']}")
                if e["type"] == "approval.request" and e["payload"]["request_id"] not in answered:
                    p = e["payload"]
                    opt = next(o for o in p["options"] if o["canonical"] == "once")
                    log(f"  approving {p['tool']} with option {opt['id']!r}")
                    call("POST", f"/api/sessions/{sid}/requests/{p['request_id']}",
                         {"option_id": opt["id"]})
                    answered.add(p["request_id"])

        def done():
            poll()
            final = [e for e in events if e["type"] == "agent.message" and e["payload"].get("final")]
            idle = any(e["type"] == "status" and e["payload"].get("state") == "idle" for e in events)
            return final and idle and call("GET", f"/api/sessions/{sid}")["state"] == "idle"

        until("final agent.message and idle", int(os.environ.get("E2E_TURN_TIMEOUT", "240")), done, 1)
        poll()
    except SystemExit as e:
        log(str(e))
        result["error"] = str(e)

    final = [e for e in events if e["type"] == "agent.message" and e["payload"].get("final")]
    checks = result["checks"]
    checks["approval.request answered with once"] = len(answered) >= 1
    checks["approval.response stored"] = any(e["type"] == "approval.response" for e in events)
    checks["final agent.message is DONE"] = bool(final) and final[-1]["payload"]["text"].strip().rstrip(".") == "DONE"
    checks["status idle"] = any(e["type"] == "status" and e["payload"].get("state") == "idle" for e in events)
    stored = call("GET", f"/api/sessions/{sid}/events?after_seq=0&limit=500")["events"]
    checks["input delivered"] = any(e["type"] == "user.message" and e.get("delivery") == "delivered"
                                    for e in stored)
    checks["file hqc-e2e in workspace"] = os.path.exists(os.path.join(workspace, "hqc-e2e"))
    s = call("GET", f"/api/sessions/{sid}")
    checks["session state idle"] = s["state"] == "idle"
    rsid = s["agent"].get("runtime_session_id")
    # Claude Code names the project folder after the real cwd: every char that is not an
    # ASCII letter or digit becomes `-` (SPEC §9, CLI 2.1.308).
    folder = re.sub(r"[^A-Za-z0-9]", "-", os.path.realpath(workspace))
    config = os.environ.get("CLAUDE_CONFIG_DIR") or os.path.expanduser("~/.claude")
    transcript = os.path.join(config, "projects", folder, f"{rsid}.jsonl")
    result["transcript"] = transcript
    checks["transcript in ~/.claude/projects"] = bool(rsid) and os.path.exists(transcript)
    return finish(0 if all(checks.values()) else 1)


def report(out):
    """Print the events (seq, type) and the checks of a finished scenario."""
    r = json.load(open(out))
    print(f"session {r['session_id']}")
    for e in r["events"]:
        p = e["payload"]
        extra = {
            "status": lambda: p.get("state"),
            "system": lambda: p.get("code"),
            "tool.call": lambda: p.get("tool"),
            "approval.request": lambda: p.get("tool"),
            "approval.response": lambda: p.get("option_id"),
            "agent.message": lambda: f"final={p.get('final')} {p.get('text', '')[:60]!r}",
            "user.message": lambda: e.get("delivery"),
        }.get(e["type"], lambda: "")()
        print(f"  {e['seq']:>3}  {e['type']:<18} {extra or ''}")
    print(f"transcript {r.get('transcript')}")
    if r.get("error"):
        print(f"error: {r['error']}")
    for name, ok in r["checks"].items():
        print(f"  [{'ok' if ok else 'FAIL'}] {name}")


def main():
    cmd, args = sys.argv[1], sys.argv[2:]
    fn = {"wait-health": wait_health, "vapid": vapid, "client": client, "confirm": confirm,
          "wait-online": wait_online, "scenario": scenario, "report": report}[cmd]
    sys.exit(fn(*args) or 0)


if __name__ == "__main__":
    main()
