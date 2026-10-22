#!/usr/bin/env python3
"""Turn driver.py runs into committable fixtures (crates/hqc-claude-jsonl/fixtures/README.md).

For each NAME=RUN it writes, into OUT:
  <NAME>-<VER>.jsonl         transcript from <projects>/*/<session id>.jsonl
  stream-<NAME>-<VER>.jsonl  driver stdout
  stdin-<NAME>-<VER>.jsonl   driver stdin
Then it re-reads what it wrote and exits 1 if anything it should have removed is left.
"""
import argparse, glob, json, os, re, sys

EMAIL = re.compile(r"[\w.+-]+@[\w-]+(?:\.[\w-]+)+")
HOME_DIR = re.compile(r"/(Users|home)/([^/\s\"'\\]+)")
SAFE_USER = "user"
SAFE_EMAIL = "user@example.com"
STRIPPED = {"stripped": True}
INIT_KEYS = ["type", "subtype", "cwd", "session_id", "model", "permissionMode",
             "claude_code_version", "mcp_servers", "uuid"]


def parse_args():
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--runs", required=True, help="directory with driver.py OUT_DIRs")
    p.add_argument("--out", required=True, help="fixtures directory to write into")
    p.add_argument("--cli-version", required=True, help="Claude Code version for file names")
    p.add_argument("--projects", default=os.path.join(os.path.expanduser("~"), ".claude", "projects"),
                   help="Claude Code transcripts root (default: ~/.claude/projects)")
    p.add_argument("--replace", action="append", default=[], metavar="OLD=NEW",
                   help="extra literal substitution, applied after cwd and before home dirs")
    p.add_argument("pairs", nargs="+", metavar="NAME=RUN",
                   help="fixture name and the driver run subdirectory under --runs")
    return p.parse_args()


class Scrubber:
    def __init__(self, cwd, name, extra):
        target = f"/tmp/hqc-fixture/{name}"
        olds = {cwd, os.path.realpath(cwd)}
        olds |= {o[len("/private"):] for o in olds if o.startswith("/private/")}
        self.subs = [(o, target) for o in sorted(olds, key=len, reverse=True)]
        self.subs += [tuple(r.split("=", 1)) for r in extra]
        # Claude Code encodes cwd in folder names as path with '/' and '.' replaced by '-'.
        self.subs += [(re.sub(r"[/.]", "-", o), re.sub(r"[/.]", "-", target))
                      for o, _ in self.subs[:len(olds)]]

    def text(self, s):
        for old, new in self.subs:
            s = s.replace(old, new)
        users = {m.group(2) for m in HOME_DIR.finditer(s)} - {SAFE_USER}
        s = HOME_DIR.sub(lambda m: f"/{m.group(1)}/{SAFE_USER}", s)
        for u in users:
            s = s.replace(f"-Users-{u}", f"-Users-{SAFE_USER}").replace(f"-home-{u}", f"-home-{SAFE_USER}")
        return EMAIL.sub(SAFE_EMAIL, s)

    def value(self, v):
        if isinstance(v, str):
            return self.text(v)
        if isinstance(v, list):
            return [self.value(x) for x in v]
        if isinstance(v, dict):
            return {k: self.value(x) for k, x in v.items() if "hook" not in k.lower()}
        return v


def transcript_record(rec):
    t = rec.get("type", "")
    if t.startswith("api-request"):
        return None
    if t == "attachment":
        rec["attachment"] = STRIPPED
        if "rendered" in rec:
            rec["rendered"] = STRIPPED
    if t == "file-history-snapshot":
        rec["snapshot"] = STRIPPED
    return rec


def stream_record(msg):
    if msg.get("type") == "system" and msg.get("subtype") == "init":
        return {k: msg[k] for k in INIT_KEYS if k in msg}
    if msg.get("type") == "system" and "hook" in str(msg.get("subtype", "")):
        return None
    return msg


def write(path, src, fn, scrub):
    with open(src) as f, open(path, "w") as out:
        for line in f:
            if line.strip():
                rec = fn(json.loads(line))
                if rec is not None:
                    out.write(json.dumps(scrub.value(rec), ensure_ascii=False) + "\n")


def leftovers(path, cwds):
    """Everything the scrub should have removed, as (line number, reason)."""
    found = []
    with open(path) as f:
        for n, line in enumerate(f, 1):
            rec = json.loads(line)
            if str(rec.get("type", "")).startswith("api-request"):
                found.append((n, "api-request record"))
            if rec.get("type") == "attachment":
                for k in ("attachment", "rendered"):
                    if k in rec and rec[k] != STRIPPED:
                        found.append((n, f"attachment {k} not stripped"))
            if re.search(r'"[^"]*hook[^"]*"\s*:', line, re.I):
                found.append((n, "hook key"))
            for m in HOME_DIR.finditer(line):
                if m.group(2) != SAFE_USER:
                    found.append((n, f"home path {m.group(0)}"))
            for m in EMAIL.finditer(line):
                if m.group(0) != SAFE_EMAIL:
                    found.append((n, f"email {m.group(0)}"))
            for c in cwds:
                if c in line:
                    found.append((n, f"original cwd {c}"))
    return found


def main():
    a = parse_args()
    os.makedirs(a.out, exist_ok=True)
    written, cwds = [], set()
    for pair in a.pairs:
        name, run = pair.split("=", 1)
        d = os.path.join(a.runs, run)
        meta = json.load(open(os.path.join(d, "run.json")))
        sid, cwd = meta["session_id"], meta["cwd"]
        cwds |= {cwd, os.path.realpath(cwd)}
        hits = glob.glob(os.path.join(a.projects, "*", f"{sid}.jsonl"))
        if len(hits) != 1:
            sys.exit(f"{name}: expected one transcript for {sid} under {a.projects}, got {hits}")
        scrub = Scrubber(cwd, name, a.replace)
        files = [
            (f"{name}-{a.cli_version}.jsonl", hits[0], transcript_record),
            (f"stream-{name}-{a.cli_version}.jsonl", os.path.join(d, "stdout.jsonl"), stream_record),
            (f"stdin-{name}-{a.cli_version}.jsonl", os.path.join(d, "stdin.jsonl"), lambda m: m),
        ]
        for fname, src, fn in files:
            path = os.path.join(a.out, fname)
            write(path, src, fn, scrub)
            written.append(path)
        print(f"{name} <- {run} ({sid})")

    # Short relative cwds like "." would match everywhere; only check real paths.
    cwds = {c for c in cwds if c.count("/") > 1}
    bad = [(p, n, why) for p in written for n, why in leftovers(p, cwds)]
    for p, n, why in bad:
        print(f"LEFT {p}:{n}: {why}", file=sys.stderr)
    if bad:
        sys.exit(1)
    print(f"ok: {len(written)} files clean")


if __name__ == "__main__":
    main()
