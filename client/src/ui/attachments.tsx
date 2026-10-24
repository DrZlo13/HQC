// Attachments in the chat (SPEC §4 `attachment`, `user.message.artifact_ids`, §12): an
// image inline, a diff as a diff, HTML in a sandboxed iframe when artifacts have their own
// domain (a link otherwise), anything else a card with name, size and "download". Links
// are signed and short-lived: asked for when shown or clicked, renewed on 403/410.

import { useEffect, useState } from 'preact/hooks';
import { formatSize, MAX_INLINE_TEXT, MAX_OPEN_TEXT, type ArtifactStore } from '../artifacts';
import type { Artifact, ArtifactKind } from '../proto';
import type { PendingAttachment } from '../timeline';
import { CodeBlock } from './Markdown';

export interface AttachmentActions {
  /** Absent: the card shows only what the event says and an "open" button. */
  artifacts?: ArtifactStore;
  openArtifact(id: string, download?: boolean): void;
}

const KIND_LABELS: Record<ArtifactKind, string> = {
  file: 'file',
  image: 'image',
  diff: 'diff',
  html: 'HTML',
  text: 'text',
  unknown: 'attachment',
};

export function AttachmentView({
  id,
  kind,
  name,
  caption,
  actions,
}: {
  id: string;
  kind?: ArtifactKind;
  name?: string;
  caption?: string;
  actions: AttachmentActions;
}) {
  const store = actions.artifacts;
  const [meta, setMeta] = useState<Artifact | null>(null);
  const [metaFailed, setMetaFailed] = useState(false);
  useEffect(() => {
    if (!store) return;
    let live = true;
    store.meta(id).then(
      (m) => live && setMeta(m),
      () => live && setMetaFailed(true),
    );
    return () => {
      live = false;
    };
  }, [store, id]);
  const k = meta?.kind ?? kind ?? 'unknown';
  const n = meta?.name ?? name ?? 'attachment';
  let body;
  if (store && k === 'image') body = <ImageView id={id} name={n} store={store} actions={actions} />;
  // The body of a diff is loaded only once its size is known (T22).
  else if (store && k === 'diff' && !meta && !metaFailed) body = <div class="card card-attachment muted">{n} · loading…</div>;
  else if (store && k === 'diff' && meta && meta.size <= MAX_INLINE_TEXT)
    body = <DiffView id={id} name={n} store={store} actions={actions} />;
  else if (store && k === 'html') body = <HtmlView id={id} name={n} meta={meta} store={store} actions={actions} />;
  else body = <FileCard id={id} name={n} kind={k} meta={meta} actions={actions} />;
  return (
    <div class={`attachment attachment-kind-${k}`}>
      {body}
      {caption && <div class="attachment-caption">{caption}</div>}
    </div>
  );
}

function FileCard({
  id,
  name,
  kind,
  meta,
  actions,
}: {
  id: string;
  name: string;
  kind: ArtifactKind;
  meta: Artifact | null;
  actions: AttachmentActions;
}) {
  // Large text and diffs are only downloaded: a browser tab would hold all of it (T22).
  const openable = kind !== 'file' && !((kind === 'text' || kind === 'diff') && meta && meta.size > MAX_OPEN_TEXT);
  return (
    <div class="card card-attachment">
      <div class="card-title">
        <span class="attachment-name">{name}</span>{' '}
        <span class="muted">
          {KIND_LABELS[kind]}
          {meta ? ` · ${formatSize(meta.size)}` : ''}
        </span>
      </div>
      <div class="actions">
        {openable && (
          <button type="button" class="btn btn-small" onClick={() => actions.openArtifact(id)}>
            Open
          </button>
        )}
        <button type="button" class="btn btn-small" onClick={() => actions.openArtifact(id, true)}>
          Download
        </button>
      </div>
    </div>
  );
}

/** A signed link for `id`, renewed by `renew()` (an expired link on `<img>` or `<iframe>`). */
function useArtifactUrl(store: ArtifactStore, id: string) {
  const [url, setUrl] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  const [renewed, setRenewed] = useState(false);
  useEffect(() => {
    let live = true;
    store.url(id).then(
      (u) => live && setUrl(u),
      () => live && setFailed(true),
    );
    return () => {
      live = false;
    };
  }, [store, id]);
  const renew = () => {
    // The status of an <img> load is not visible: renew once, then give up.
    if (renewed) return setFailed(true);
    setRenewed(true);
    store.url(id, true).then(setUrl, () => setFailed(true));
  };
  return { url, failed, renew };
}

function ImageView({ id, name, store, actions }: { id: string; name: string; store: ArtifactStore; actions: AttachmentActions }) {
  const { url, failed, renew } = useArtifactUrl(store, id);
  if (failed) return <FileCard id={id} name={name} kind="image" meta={null} actions={actions} />;
  return (
    <button type="button" class="attachment-image" title={name} onClick={() => actions.openArtifact(id)}>
      {url ? <img src={url} alt={name} loading="lazy" onError={renew} /> : <span class="muted">{name}</span>}
    </button>
  );
}

function DiffView({ id, name, store, actions }: { id: string; name: string; store: ArtifactStore; actions: AttachmentActions }) {
  const [text, setText] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    store.text(id).then(
      (t) => live && setText(t),
      (e: Error) => live && setError(e.message),
    );
    return () => {
      live = false;
    };
  }, [store, id]);
  const download = (
    <button type="button" class="btn-copy" title="Download" onClick={() => actions.openArtifact(id, true)}>
      Download
    </button>
  );
  if (error) {
    return (
      <div class="card card-attachment">
        <div class="card-title">{name}</div>
        <div class="error-text">Diff failed to load: {error}</div>
      </div>
    );
  }
  if (text === null) return <div class="card card-attachment muted">{name} · loading…</div>;
  return <CodeBlock code={text} lang="diff" title={name} extra={download} />;
}

function HtmlView({
  id,
  name,
  meta,
  store,
  actions,
}: {
  id: string;
  name: string;
  meta: Artifact | null;
  store: ArtifactStore;
  actions: AttachmentActions;
}) {
  const { url } = useArtifactUrl(store, id);
  // Rendered only from a separate artifact domain: on the hub's own domain HTML is served
  // as a download and would have the hub's origin (SPEC §12).
  if (!url || !store.isSeparateOrigin(url)) {
    return <FileCard id={id} name={name} kind="html" meta={meta} actions={actions} />;
  }
  return (
    <div class="card card-attachment">
      <div class="card-title">
        <span class="attachment-name">{name}</span>{' '}
        <button type="button" class="btn btn-small" onClick={() => actions.openArtifact(id)}>
          Open
        </button>
      </div>
      <iframe class="attachment-html" src={url} title={name} sandbox="allow-scripts" referrerpolicy="no-referrer" />
    </div>
  );
}

/** Files of a message that is not stored yet: local previews, nothing fetched. */
export function PendingAttachments({ files }: { files: PendingAttachment[] }) {
  return (
    <div class="attachments">
      {files.map((f, i) =>
        f.preview ? (
          <img key={i} class="attachment-thumb" src={f.preview} alt={f.name} title={f.name} />
        ) : (
          <span key={i} class="attachment-chip">
            {f.name} <span class="muted">{formatSize(f.size)}</span>
          </span>
        ),
      )}
    </div>
  );
}
