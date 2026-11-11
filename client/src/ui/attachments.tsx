// Attachments in the chat (SPEC §4 `attachment`, `user.message.artifact_ids`, §12): an
// image inline, a diff as a diff, HTML in a sandboxed iframe when artifacts have their own
// domain (a link otherwise), anything else a card with name, size and "download". Links
// are signed and short-lived: asked for when shown or clicked, renewed on 403/410. An image
// and a Markdown text open in a viewer over the chat.

import { useEffect, useRef, useState } from 'preact/hooks';
import { formatSize, MAX_INLINE_TEXT, MAX_OPEN_TEXT, type ArtifactStore } from '../artifacts';
import type { Artifact, ArtifactKind } from '../proto';
import type { PendingAttachment } from '../timeline';
import { IconCross } from './icons';
import { CodeBlock, Markdown } from './Markdown';

/** What the viewer over the chat shows: an image, or a text rendered as Markdown. */
export interface ViewedArtifact {
  id: string;
  name: string;
  kind: 'image' | 'markdown';
}

export interface AttachmentActions {
  /** Absent: the card shows only what the event says and an "open" button. */
  artifacts?: ArtifactStore;
  openArtifact(id: string, download?: boolean): void;
  /** Shows the artifact in the viewer over the chat; absent: it opens as `openArtifact`. */
  viewArtifact?(a: ViewedArtifact): void;
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
  const open = () =>
    // Only with its size known: the 2 MB cap of "Open" holds before the text is read.
    actions.viewArtifact && meta && isMarkdown(kind, name, meta) ? actions.viewArtifact({ id, name, kind: 'markdown' }) : actions.openArtifact(id);
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
          <button type="button" class="btn btn-small" onClick={open}>
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

/** A text the viewer renders as Markdown: named `.md` or `.markdown`, or typed `text/markdown`. */
function isMarkdown(kind: ArtifactKind, name: string, meta: Artifact | null): boolean {
  return kind === 'text' && (/\.(md|markdown)$/i.test(name) || (meta?.mime ?? '').startsWith('text/markdown'));
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
    <button
      type="button"
      class="attachment-image"
      title={name}
      onClick={() => (actions.viewArtifact ? actions.viewArtifact({ id, name, kind: 'image' }) : actions.openArtifact(id))}
    >
      {url ? <img src={url} alt={name} loading="lazy" onError={renew} /> : <span class="muted">{name}</span>}
    </button>
  );
}

/** The viewer over the chat (SPEC §12): an image fitted to the window, never enlarged past its own
 *  size, or a Markdown text rendered as the feed renders the agent's messages. It closes on
 *  its button, Escape and a click outside it; while it is open no key reaches the chat behind
 *  it (the request card takes Enter and Escape on `document`). */
export function ArtifactViewer({
  id,
  name,
  kind,
  store,
  phone,
  onClose,
  onDownload,
}: ViewedArtifact & { store: ArtifactStore; phone?: boolean; onClose: () => void; onDownload: () => void }) {
  const body = useRef<HTMLDivElement>(null);
  // A click closes the viewer only if it also went down on the scrim: a selection of text
  // that ends past the panel is not a click outside it.
  const downOnScrim = useRef(false);
  useEffect(() => {
    // The body takes the focus, not the close button: Space, the arrows and PageDown scroll
    // the text, and Space does not press "Close".
    body.current?.focus();
    // On `window` in the capture phase: before any listener of the page. The default action
    // stays, so a focused button still activates.
    const key = (e: KeyboardEvent) => {
      e.stopPropagation();
      if (e.key !== 'Escape') return;
      e.preventDefault();
      onClose();
    };
    addEventListener('keydown', key, true);
    return () => removeEventListener('keydown', key, true);
  }, []);
  return (
    <div
      class={`modal-scrim viewer-scrim ${phone ? 'modal-phone' : ''}`}
      onPointerDown={(e) => (downOnScrim.current = e.target === e.currentTarget)}
      onClick={(e) => downOnScrim.current && e.target === e.currentTarget && onClose()}
    >
      <section class={`viewer viewer-${kind}`} role="dialog" aria-modal="true" aria-label={name}>
        <header class="viewer-head">
          <span class="viewer-name" title={name}>
            {name}
          </span>
          <button type="button" class="btn btn-small btn-ghost" onClick={onDownload}>
            Download
          </button>
          <button type="button" class="icon-btn" aria-label="Close" onClick={onClose}>
            <IconCross size={13} />
          </button>
        </header>
        <div class="viewer-body" ref={body} tabIndex={-1}>
          {kind === 'image' ? <ViewerImage id={id} name={name} store={store} /> : <ViewerMarkdown id={id} store={store} />}
        </div>
      </section>
    </div>
  );
}

function ViewerImage({ id, name, store }: { id: string; name: string; store: ArtifactStore }) {
  const { url, failed, renew } = useArtifactUrl(store, id);
  if (failed) return <div class="error-text">The image failed to load</div>;
  if (!url) return <div class="muted">loading…</div>;
  return <img src={url} alt={name} onError={renew} />;
}

function ViewerMarkdown({ id, store }: { id: string; store: ArtifactStore }) {
  const [text, setText] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    store.text(id, MAX_OPEN_TEXT).then(
      (t) => live && setText(t),
      (e: Error) => live && setError(e.message),
    );
    return () => {
      live = false;
    };
  }, [store, id]);
  if (error) return <div class="error-text">The document failed to load: {error}</div>;
  if (text === null) return <div class="muted">loading…</div>;
  return <Markdown text={text} />;
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
