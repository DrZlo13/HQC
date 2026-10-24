# T11: markdown in the client — decisions and open questions

Rendering: `client/src/ui/Markdown.tsx` (marked lexer → Preact elements); copying:
`client/src/clipboard.ts`; tests: `client/tests/markdown.test.tsx` and a step in
`client/tests/e2e.test.tsx`. SPEC did not change: §4 and §6 do not say how to show `text`,
and the build keeps §20 (CSP) as before: no inline scripts (test `csp.test.ts`), no `eval`
or `new Function` in the bundle. The client code does not use `innerHTML` or
`dangerouslySetInnerHTML` (the bundle only contains Preact's own branch for that property).

## Decided without asking

1. **Only absolute links**: `http:`, `https:`, `mailto:` (checked with `new URL`).
   Relative ones (`/api/...`, `#anchor`, `//host`) are shown as the source text
   `[text](address)`, as are `javascript:`/`data:`/`file:`: a relative address in an agent
   message points at the hub, not where the agent meant. If agents write links to files in
   the working copy (`[x](src/main.rs)`), those should be shown differently — question below.
2. **A single line break is `<br>`** (`breaks: true`), as requested; this differs from
   CommonMark, where it collapses into a space.
3. **Raw HTML** is shown as text: block HTML as a monospace paragraph keeping line breaks,
   inline HTML as is within the line.
4. **Entities**: numeric ones (`&#65;`, `&#x41;`) and `&amp; &lt; &gt; &quot;
   &apos; &nbsp;` are decoded; other named ones (`&copy;`) stay as written — the full HTML
   table weighs more than everything else combined.
5. **Images**: italic alt text and a link to the address (if the scheme is allowed),
   otherwise the address as text. `data:` images are therefore shown as a long string.
6. **Task checkboxes** are `disabled`, display only.
7. **The "Copy" button** is on `user.message`, `agent.message` (intermediate and final) and
   `peer.message`; it copies `payload.text` as is. `system` lines, the delta stream and a
   message not yet sent have no button. The buttons and the language bar above code are
   `user-select: none`, so selecting with the mouse and Cmd-C does not put them into the
   copied text.
8. **Streaming**: each delta re-renders the whole text (lexer over the whole buffer,
   `useMemo` on the text). An unclosed code block is shown as a code block to the end of the
   text; an unclosed `**` literally until it closes. The stream cursor is drawn by CSS after
   the last block (after a list or code block it moves to a new line).
9. **No syntax highlighting**, only the language label (the first word after ```` ``` ````).

## Not supported

- Footnotes, math, `<details>`, definition lists, heading anchors, emoji codes `:x:`.
- Markdown in `agent.thinking`, tool previews and `clarify` questions — still `<pre>`/text
  there. Whether it is needed — question below.

## Open questions

1. **Links to working-copy files.** Claude Code agents often write `[main.rs](src/main.rs)`
   or `src/main.rs:42`. Now this is text. It could become a link to an artifact or to a file
   viewer once one exists (§14?).
2. **Markdown in thinking (`agent.thinking`)** — keep `<pre>` or render it the same way?
3. **A long message in `examples/wire`.** There is no long example there, and `crates/` was
   off limits in T11: the test takes the `event.agent.message.json` envelope and substitutes
   ~100 KB of generated text. Proposal: add to `hqc-proto/examples/wire` an example with a
   long markdown answer from a real agent (no secrets) and use it.
4. **Quadratic streaming.** On every delta the lexer walks the whole buffer; on a 100 KB
   answer this is noticeable only on a weak phone. If it becomes a problem — re-render at
   most once per frame or cache the closed blocks.
5. **Bundle size**: 18.6 → 34.1 KB gzip (JS), almost all of the growth is marked. Is that
   acceptable for the first load from a phone through a tunnel, or should markdown load
   lazily (a separate chunk after the first screen)?

## Not checked

- Not looked at in a browser: the T11 environment has no browser. Checked in happy-dom
  (rendering, copying, a scenario through `<App>` with a mock), the build and types. A manual
  run is needed: `npm run dev:mock`, write "show markdown" in any session; separately —
  that Cmd-C on a selection across a code block does not capture "Copy" and the language
  label, and that the fallback copy through a textarea works in Safari on iPhone over http.
