#!/usr/bin/env python3
"""Drive `claude -p` in stream-json mode, log every stdin/stdout frame, answer can_use_tool.

usage: driver.py [--resume UUID] [--arg-prompt] OUT_DIR CWD POLICY PROMPT [-- extra claude args]
POLICY: once | deny | always | ask_answer | none
--resume UUID (or SPIKE_RESUME): continue that session with --resume instead of a new
--session-id.
--arg-prompt (or SPIKE_ARG_PROMPT=1): plain `claude -p PROMPT` (prompt as argument, text
output, no host prompts), the way a user runs it in a terminal; POLICY is ignored.
"""
import json, os, subprocess, sys, threading, time, uuid

args = sys.argv[1:]
# `--resume UUID` / `--arg-prompt` before the positional arguments: the same as the
# variables, for shells where setting the environment is awkward.
while args and args[0] in ("--resume", "--arg-prompt"):
    if args[0] == "--resume":
        os.environ["SPIKE_RESUME"] = args[1]
        args = args[2:]
    else:
        os.environ["SPIKE_ARG_PROMPT"] = "1"
        args = args[1:]
out, cwd, policy, prompt = args[0:4]
extra = args[5:] if len(args) > 4 and args[4] == "--" else []
os.makedirs(out, exist_ok=True)
resume = os.environ.get("SPIKE_RESUME")
sid = resume or str(uuid.uuid4())
session = ["--resume", sid] if resume else ["--session-id", sid]
if os.environ.get("SPIKE_ARG_PROMPT"):
    cmd = ["claude", "-p", prompt, "--model", "claude-haiku-5-5", "--max-turns", "2"] \
        + session + extra
    json.dump({"cmd": cmd, "cwd": os.path.abspath(cwd), "session_id": sid},
              open(f"{out}/run.json", "w"), indent=1)
    t0 = time.time()
    r = subprocess.run(cmd, cwd=cwd, stdin=subprocess.DEVNULL, stdout=open(f"{out}/stdout.txt", "w"),
                       stderr=open(f"{out}/stderr.txt", "w"),
                       timeout=float(os.environ.get("SPIKE_TIMEOUT", "180")))
    print(f"session={sid} rc={r.returncode} elapsed={time.time() - t0:.1f}s")
    sys.exit(0)
cmd = ["claude", "-p", "--output-format", "stream-json", "--input-format", "stream-json",
       "--permission-prompts", "host", "--include-partial-messages",
       "--replay-user-messages", "--verbose", "--model", "claude-haiku-5-5",
       "--max-turns", "2"] + session + extra
json.dump({"cmd": cmd, "cwd": os.path.abspath(cwd), "session_id": sid, "policy": policy,
           "env": {k: v for k, v in os.environ.items() if k.startswith("MCP_")}},
          open(f"{out}/run.json", "w"), indent=1)

p = subprocess.Popen(cmd, cwd=cwd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                     stderr=open(f"{out}/stderr.txt", "w"), text=True, bufsize=1)
fin = open(f"{out}/stdin.jsonl", "w")
fout = open(f"{out}/stdout.jsonl", "w")
t0 = time.time()
timing = open(f"{out}/timing.txt", "w")


def send(obj):
    line = json.dumps(obj)
    fin.write(line + "\n"); fin.flush()
    p.stdin.write(line + "\n"); p.stdin.flush()


def answer(req_id, req):
    tool, inp = req.get("tool_name"), req.get("input", {})
    if policy == "deny":
        resp = {"behavior": "deny", "message": "denied by hqc spike"}
    elif policy == "always":
        resp = {"behavior": "allow", "updatedInput": inp,
                "updatedPermissions": req.get("permission_suggestions", [])}
    elif policy == "ask_answer" and tool == "AskUserQuestion":
        answers = {q["question"]: q["options"][0]["label"] for q in inp.get("questions", [])}
        resp = {"behavior": "allow", "updatedInput": dict(inp, answers=answers)}
    else:
        resp = {"behavior": "allow", "updatedInput": inp}
    send({"type": "control_response",
          "response": {"subtype": "success", "request_id": req_id, "response": resp}})


if os.environ.get("SPIKE_INIT"):
    send({"type": "control_request", "request_id": "init-1",
          "request": {"subtype": "initialize"}})
send({"type": "user", "message": {"role": "user", "content": prompt}})

deadline = time.time() + float(os.environ.get("SPIKE_TIMEOUT", "180"))
timer = threading.Timer(deadline - time.time(), p.kill); timer.start()
for line in p.stdout:
    fout.write(line); fout.flush()
    try:
        msg = json.loads(line)
    except ValueError:
        continue
    timing.write(f"{time.time() - t0:8.2f} {msg.get('type')} {msg.get('subtype', '')}\n")
    if msg.get("type") == "control_request":
        req = msg.get("request", {})
        if req.get("subtype") == "can_use_tool" and policy != "none":
            answer(msg.get("request_id"), req)
    if msg.get("type") == "result":
        p.stdin.close()
timer.cancel()
p.wait()
print(f"session={sid} rc={p.returncode} elapsed={time.time() - t0:.1f}s")
