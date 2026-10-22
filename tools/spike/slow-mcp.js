// Minimal stdio MCP server with one tool `slow(seconds)` that sleeps and returns.
// Logs every request with a timestamp to $SLOW_MCP_LOG.
const fs = require("fs");
const log = (s) => process.env.SLOW_MCP_LOG &&
  fs.appendFileSync(process.env.SLOW_MCP_LOG, `${new Date().toISOString()} ${s}\n`);
const send = (o) => process.stdout.write(JSON.stringify(o) + "\n");
let buf = "";
process.stdin.on("data", (d) => {
  buf += d;
  let i;
  while ((i = buf.indexOf("\n")) >= 0) {
    const line = buf.slice(0, i); buf = buf.slice(i + 1);
    if (!line.trim()) continue;
    const m = JSON.parse(line);
    log(`<- ${line}`);
    if (m.method === "initialize") {
      send({ jsonrpc: "2.0", id: m.id, result: {
        protocolVersion: m.params.protocolVersion, capabilities: { tools: {} },
        serverInfo: { name: "slow", version: "0.0.1" } } });
    } else if (m.method === "tools/list") {
      send({ jsonrpc: "2.0", id: m.id, result: { tools: [{
        name: "slow", description: "Sleep for the given number of seconds, then return.",
        inputSchema: { type: "object", properties: { seconds: { type: "number" } },
                       required: ["seconds"] } }] } });
    } else if (m.method === "tools/call") {
      const s = m.params.arguments.seconds;
      const token = m.params._meta && m.params._meta.progressToken;
      const every = Number(process.env.SLOW_MCP_PROGRESS_MS || 0);
      let n = 0;
      const tick = every && token !== undefined && setInterval(() => {
        n += 1;
        log(`-> progress ${n}`);
        send({ jsonrpc: "2.0", method: "notifications/progress",
               params: { progressToken: token, progress: n, message: `waiting ${n}` } });
      }, every);
      setTimeout(() => {
        if (tick) clearInterval(tick);
        log(`-> result after ${s}s`);
        send({ jsonrpc: "2.0", id: m.id, result: { content: [{ type: "text", text: `slept ${s}s` }] } });
      }, s * 1000);
    } else if (m.id !== undefined) {
      send({ jsonrpc: "2.0", id: m.id, error: { code: -32601, message: "not found" } });
    }
  }
});
