// Messages between sessions in the feed (T51, SPEC §7.1): violet blocks, ↗ for what left this
// session and ↙ for what came into it, the other session's address as a link (plain text when
// the session is deleted or unknown). The answering session shows the `peer.message` and the
// final message answering it; the asking one its own `hqc_send` / `hqc_ask` call and the ask's
// result. The time a message was delivered comes from the event in the other session.

import { Fragment, type ComponentChildren } from 'preact';
import { useEffect, useState } from 'preact/hooks';
import type { Api } from '../api';
import { ARTIFACT_KINDS, type ArtifactKind, type EventOf, type HqcEvent } from '../proto';
import { AttachmentView } from './attachments';
import { UsageParts, type ChatActions } from './cards';
import { askResult, hqcTool, peerArgs, sendResult, splitReply, type Block, type ReplyFile } from './feed';
import { IconArrowIn, IconArrowOut } from './icons';
import { formatTime } from './labels';
import { CopyButton, Markdown } from './Markdown';

/** `ChatActions.peerMessage` over the events API: the event at `seq` of a session, kept once
 *  it is delivered (it does not change after that), asked again after 15 s until then. */
export function peerMessages(api: Api): (sessionId: string, seq: number) => Promise<HqcEvent | undefined> {
  const cache = new Map<string, { at: number; p: Promise<HqcEvent | undefined> }>();
  return (sessionId, seq) => {
    const key = `${sessionId}/${seq}`;
    const hit = cache.get(key);
    if (hit && Date.now() - hit.at < 15_000) return hit.p;
    const p = api.events(sessionId, seq - 1, 1).then((page) => page.events.find((e) => e.seq === seq));
    cache.set(key, { at: Date.now(), p });
    p.then(
      (e) => e?.delivered_at && cache.set(key, { at: Infinity, p }),
      () => cache.delete(key),
    );
    return p;
  };
}

/** The `peer.message` this session left in session `sessionId` at `seq`. */
function usePeerMessage(actions: ChatActions, sessionId: string | undefined, seq: number | undefined) {
  const [ev, setEv] = useState<EventOf<'peer.message'>>();
  const version = actions.timeline.version;
  useEffect(() => {
    if (!sessionId || seq === undefined || !actions.peerMessage || ev?.delivered_at) return;
    let live = true;
    actions.peerMessage(sessionId, seq).then(
      (e) => live && e?.type === 'peer.message' && setEv(e),
      () => {},
    );
    return () => {
      live = false;
    };
  }, [sessionId, seq, version]);
  return ev;
}

function Relay({
  dir,
  label,
  peer,
  when,
  copy,
  children,
}: {
  dir: 'in' | 'out';
  label: string;
  peer: ComponentChildren;
  when?: string;
  /** The text the copy button in the head copies. */
  copy?: string;
  children: ComponentChildren;
}) {
  return (
    <section class={`relay relay-${dir}`} aria-label={`${label} another session`}>
      <div class="relay-head">
        <span class="relay-arrow">{dir === 'out' ? <IconArrowOut /> : <IconArrowIn />}</span>
        <span class="relay-label">{label}</span>
        {peer}
        <span class="relay-when meta">
          {copy && <CopyButton text={copy} />}
          {when}
        </span>
      </div>
      {children}
    </section>
  );
}

/** The other session: a link while the user has it, else `label` (or `target`) as text. */
function PeerLink({ target, label, actions }: { target: string; label?: string; actions: ChatActions }) {
  const s = target ? actions.findSession?.(target) : undefined;
  if (s)
    return (
      <a class="relay-peer mono" href={`#/s/${s.id}`}>
        {s.address}
      </a>
    );
  return <span class="relay-peer mono">{label ?? target}</span>;
}

function Files({ ids, actions }: { ids: string[]; actions: ChatActions }) {
  if (!ids.length) return null;
  return (
    <div class="relay-files">
      {ids.map((id) => (
        <AttachmentView key={id} id={id} actions={actions} />
      ))}
    </div>
  );
}

/** Files known only by what the agent named (`files` of a call): their names. */
function FileNames({ names }: { names: string[] }) {
  if (!names.length) return null;
  return (
    <div class="relay-files">
      {names.map((n) => (
        <div key={n} class="relay-file">
          <span class="mono">{n.split('/').pop() || n}</span>
        </div>
      ))}
    </div>
  );
}

function Separator() {
  return <div class="relay-sep">sent with hqc_send while answering</div>;
}

const kindOf = (k: string | undefined): ArtifactKind | undefined =>
  (ARTIFACT_KINDS as readonly string[]).includes(k ?? '') ? (k as ArtifactKind) : undefined;

/** "↙ Question from X" / "↙ Message from X": a `peer.message` that came into this session. */
export function PeerMessageBlock({ event, actions }: { event: EventOf<'peer.message'>; actions: ChatActions }) {
  const p = event.payload;
  return (
    <Relay
      dir="in"
      label={p.mode === 'ask' ? 'Question from' : 'Message from'}
      peer={<PeerLink target={p.from_session_id} label={p.from ?? `session ${p.from_session_id.slice(-8)}`} actions={actions} />}
      when={formatTime(event.ts)}
      copy={p.text}
    >
      <Markdown class="text" text={p.text} />
      <Files ids={p.artifact_ids} actions={actions} />
    </Relay>
  );
}

/** "↗ Reply to X · delivered <time>": the final message that answered a question, with the
 *  turn's attachments and the sends that went with it (T42). */
export function ReplyToBlock({ block, actions }: { block: Extract<Block, { kind: 'reply-to' }>; actions: ChatActions }) {
  const { event, question, usage } = block;
  const at = event.reply_delivered_at;
  const asker = question.payload.from_session_id;
  return (
    <Relay
      dir="out"
      label="Reply to"
      peer={<PeerLink target={asker} label={question.payload.from} actions={actions} />}
      when={at && `delivered ${formatTime(at)}`}
    >
      <Markdown class="text" text={event.payload.text} />
      {block.sends.map((s) => (
        <JoinedSend key={s.call.seq} asker={asker} call={s.call} result={s.result} actions={actions} />
      ))}
      {block.files.length > 0 && (
        <div class="relay-files">
          {block.files.map((f) => (
            <AttachmentView
              key={f.seq}
              id={f.payload.artifact_id}
              kind={f.payload.kind}
              name={f.payload.name}
              caption={f.payload.caption}
              actions={actions}
            />
          ))}
        </div>
      )}
      <div class="meta">
        <CopyButton text={event.payload.text} />
        {formatTime(event.ts)}
        {usage && (
          <span class="usage">
            {' · '}
            <UsageParts usage={usage} />
          </span>
        )}
      </div>
    </Relay>
  );
}

/** A send to the asker that went with the reply: its text after the separator, its files as
 *  they arrived in the asker. */
function JoinedSend({
  asker,
  call,
  result,
  actions,
}: {
  asker: string;
  call: EventOf<'tool.call'>;
  result?: EventOf<'tool.result'>;
  actions: ChatActions;
}) {
  const args = peerArgs(call.payload.args_preview);
  const msg = usePeerMessage(actions, actions.findSession?.(asker)?.id, sendResult(result)?.message_seq);
  return (
    <>
      <Separator />
      <Markdown class="text" text={args.text} />
      {msg ? <Files ids={msg.payload.artifact_ids} actions={actions} /> : <FileNames names={args.files} />}
    </>
  );
}

const SEND_STATES: Record<string, string> = {
  queued: 'queued',
  buffered: 'waits for its next turn',
  with_reply: 'goes with the reply',
};

/** "↗ Sent to X · delivered <time>": this session's `hqc_send` / `hqc_ask`. */
export function SentBlock({ block, actions }: { block: Extract<Block, { kind: 'sent' }>; actions: ChatActions }) {
  const { call, result } = block;
  const ask = hqcTool(call.payload.tool) === 'ask';
  const args = peerArgs(call.payload.args_preview);
  const failed = result?.payload.error;
  const send = ask ? undefined : sendResult(result);
  const answer = ask && result && !failed ? askResult(result) : undefined;
  const address = (ask ? answer?.from : send?.to) || args.session;
  const target = address ? actions.findSession?.(address) : undefined;
  const msg = usePeerMessage(actions, target?.id, (answer ?? send)?.message_seq);
  let when: string | undefined;
  if (msg?.delivered_at) when = `delivered ${formatTime(msg.delivered_at)}`;
  else if (ask && !result) when = 'waiting for a reply';
  else if (send?.delivery) when = SEND_STATES[send.delivery];
  return (
    <Relay dir="out" label="Sent to" peer={<PeerLink target={address} actions={actions} />} when={when}>
      <Markdown class="text" text={args.text} />
      {msg ? <Files ids={msg.payload.artifact_ids} actions={actions} /> : <FileNames names={args.files} />}
      {failed && <div class="relay-error">Not sent: {result.payload.error_text || result.payload.result_preview}</div>}
    </Relay>
  );
}

/** "↙ Reply from X": the result of this session's `hqc_ask`, the sends that went with the reply
 *  after their separators, the files with their copies in this session's folder. */
export function ReplyFromBlock({ block, actions }: { block: Extract<Block, { kind: 'reply-from' }>; actions: ChatActions }) {
  const res = askResult(block.result);
  const [main, ...joined] = splitReply(res.reply);
  return (
    <Relay
      dir="in"
      label="Reply from"
      peer={<PeerLink target={res.from || peerArgs(block.call.payload.args_preview).session} actions={actions} />}
      when={formatTime(block.result.ts)}
    >
      <Markdown class="text" text={main} />
      {joined.map((t, i) => (
        <Fragment key={i}>
          <Separator />
          <Markdown class="text" text={t} />
        </Fragment>
      ))}
      {res.files.length > 0 && (
        <div class="relay-files">
          {res.files.map((f, i) => (
            <ReplyFileView key={i} file={f} actions={actions} />
          ))}
        </div>
      )}
    </Relay>
  );
}

function ReplyFileView({ file: f, actions }: { file: ReplyFile; actions: ChatActions }) {
  if (f.artifact_id && !f.error)
    return <AttachmentView id={f.artifact_id} kind={kindOf(f.kind)} name={f.name} caption={f.path} actions={actions} />;
  return (
    <div class="relay-file">
      <span class="mono">{f.name}</span>
      <span class={f.error ? 'relay-error' : 'muted mono'}>{f.error ?? f.path}</span>
    </div>
  );
}
