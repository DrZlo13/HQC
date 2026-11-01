// hqc's allow rules of a folder (T48): the syntax of `hqc_proto::rules`, checked as the user
// types; the hub checks it again. `Tool`, `mcp__server`, `Bash(<words>)`, `Bash(<words> *)`,
// `Bash(<words>:*)`; never a whole command-running tool.

export const MAX_RULE_LEN = 256;
const EXEC_TOOLS = ['Bash', 'PowerShell', 'BashOutput', 'KillShell', 'KillBash'];
const WORD = /^[\p{L}\p{N}\-_./=+,@%:~]+$/u;

/** Why `rule` is not an hqc rule, or `null`. */
export function ruleError(rule: string): string | null {
  if (!rule) return 'The rule is empty.';
  if (new TextEncoder().encode(rule).length > MAX_RULE_LEN) return `The rule is longer than ${MAX_RULE_LEN} bytes.`;
  if (rule !== rule.trim()) return 'The rule has spaces around it.';
  const open = rule.indexOf('(');
  if (open < 0) {
    if (!/^[A-Za-z][A-Za-z0-9_]*$/.test(rule)) {
      return 'Not a valid rule. Use Bash(command *), a tool name like Edit, or mcp__server.';
    }
    if (EXEC_TOOLS.includes(rule)) return `${rule} as a whole would allow every command: use ${rule}(command *).`;
    return null;
  }
  if (rule.slice(0, open) !== 'Bash') return 'Only Bash takes a pattern: use the tool name alone.';
  if (!rule.endsWith(')')) return 'No closing parenthesis at the end.';
  const content = rule.slice(open + 1, -1);
  const body = content.endsWith(':*') || content.endsWith(' *') ? content.slice(0, -2) : content;
  const words = body.split(' ');
  if (!body || words.some((w) => !w)) return 'The command must be words separated by single spaces.';
  const bad = words.find((w) => !WORD.test(w));
  if (bad) return `“${bad}”: a rule cannot have quotes, shell operators or a * other than the final one.`;
  return null;
}

/** The command a Bash rule is about (`git push` of `Bash(git push:*)`), for a check. */
export function ruleCommand(rule: string): string | null {
  const m = /^Bash\((.+)\)$/.exec(rule);
  return m ? m[1].replace(/(:\*| \*)$/, '') : null;
}

/** A tool name (`Edit`, `mcp__hqc__hqc_send`) rather than a command, in the check field. */
export function isToolName(s: string): boolean {
  return /^(mcp__\w+|[A-Z][A-Za-z0-9_]*)$/.test(s.trim());
}
