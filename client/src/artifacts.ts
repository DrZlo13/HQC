// Artifacts in the chat (SPEC §12): metadata and signed links, cached per artifact. A link
// lives 10 minutes; it is asked for when something is rendered or opened, renewed shortly
// before it expires, and asked for again when the artifact host answers 403 or 410.

import type { Api } from './api';
import { dataUrl, heicToJpeg } from './heic';
import type { Artifact } from './proto';

type Source = Pick<Api, 'artifact' | 'artifactLink'>;

/** A link this close to its expiry is not handed out any more. */
const MARGIN_MS = 30_000;
/** Larger text is not loaded into the chat; it can still be downloaded. */
export const MAX_INLINE_TEXT = 512 * 1024;
/** Larger text and diffs are only offered for download, not opened (T22). */
export const MAX_OPEN_TEXT = 2 * 1024 * 1024;

/** Signed links by artifact id; one map can serve several stores (the chats of a page). */
export type LinkCache = Map<string, { url: string; expires: number }>;

export class ArtifactStore {
  private readonly metas = new Map<string, Promise<Artifact>>();
  private readonly heicImages = new Map<string, Promise<string | null>>();
  private readonly heicDecoded = new Map<string, string>();

  constructor(
    private readonly api: Source,
    private readonly fetchImpl: typeof fetch = (...a) => fetch(...a),
    private readonly now: () => number = Date.now,
    /** Where relative links point: the hub's own origin. */
    private readonly origin: string = typeof location !== 'undefined' ? location.origin : 'http://localhost',
    private readonly links: LinkCache = new Map(),
  ) {}

  /** The artifact's record; `fresh` asks the hub again (it may have taken another kind). */
  meta(id: string, fresh = false): Promise<Artifact> {
    let p = fresh ? undefined : this.metas.get(id);
    if (!p) {
      p = this.api.artifact(id);
      this.metas.set(id, p);
      // A failure is not cached: the next render tries again.
      p.catch(() => this.metas.delete(id));
    }
    return p;
  }

  /** The cached signed link if it still lives, without asking the hub: an image shown again
   *  has its address in the first render. */
  cachedUrl(id: string): string | undefined {
    const cached = this.links.get(id);
    return cached && cached.expires - this.now() > MARGIN_MS ? cached.url : undefined;
  }

  /** A signed link; `fresh` drops the cached one (it was refused). */
  async url(id: string, fresh = false): Promise<string> {
    const cached = this.links.get(id);
    if (!fresh && cached && cached.expires - this.now() > MARGIN_MS) return cached.url;
    const l = await this.api.artifactLink(id);
    const expires = Date.parse(l.expires_at);
    this.links.set(id, { url: l.url, expires: Number.isNaN(expires) ? this.now() : expires });
    return l.url;
  }

  /** The link for "download": always an attachment. */
  async downloadUrl(id: string): Promise<string> {
    const u = await this.url(id);
    return `${u}${u.includes('?') ? '&' : '?'}dl=1`;
  }

  /** The content as text; an expired or refused link is renewed once (403/410). A body
   *  announced larger than `maxBytes` is not read. */
  async text(id: string, maxBytes = MAX_INLINE_TEXT): Promise<string> {
    const res = await this.content(id);
    const len = Number(res.headers.get('content-length') ?? NaN);
    if (len > maxBytes) {
      void res.body?.cancel();
      throw new Error('attachment too large');
    }
    return res.text();
  }

  /** The content as a Blob; an expired or refused link is renewed once (403/410). */
  async blob(id: string): Promise<Blob> {
    return (await this.content(id)).blob();
  }

  /** A HEIC or HEIF image the browser does not show, as a JPEG `data:` URL; null where it
   *  cannot be decoded. Decoded once for the feed and the viewer; a failure is not cached. */
  heicImage(id: string): Promise<string | null> {
    let p = this.heicImages.get(id);
    if (!p) {
      p = this.blob(id)
        .then(heicToJpeg)
        .then(async (jpeg) => (jpeg && (await dataUrl(jpeg))) || null)
        .catch(() => null);
      this.heicImages.set(id, p);
      void p.then((u) => (u === null ? this.heicImages.delete(id) : this.heicDecoded.set(id, u)));
    }
    return p;
  }

  /** The JPEG `data:` URL of a HEIC decoded already, without waiting: an image shown again
   *  (the viewer opened from the feed) starts from it, not from the link that fails. */
  decodedHeic(id: string): string | undefined {
    return this.heicDecoded.get(id);
  }

  private async content(id: string): Promise<Response> {
    for (const fresh of [false, true]) {
      const res = await this.fetchImpl(await this.url(id, fresh));
      if (res.ok) return res;
      if (res.status !== 403 && res.status !== 410) throw new Error(`HTTP ${res.status}`);
    }
    throw new Error('attachment link rejected');
  }

  /** The link points to a separate artifact domain (`HQC_ARTIFACT_PUBLIC_BASE`): only there
   *  HTML is rendered in a sandboxed iframe (SPEC §12). */
  isSeparateOrigin(url: string): boolean {
    try {
      return new URL(url, this.origin).origin !== new URL(this.origin).origin;
    } catch {
      return false;
    }
  }
}

/** `48.2 KB`. */
export function formatSize(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ['KB', 'MB', 'GB'];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}
