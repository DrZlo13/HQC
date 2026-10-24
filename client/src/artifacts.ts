// Artifacts in the chat (SPEC §12): metadata and signed links, cached per artifact. A link
// lives 10 minutes; it is asked for when something is rendered or opened, renewed shortly
// before it expires, and asked for again when the artifact host answers 403 or 410.

import type { Api } from './api';
import type { Artifact } from './proto';

type Source = Pick<Api, 'artifact' | 'artifactLink'>;

/** A link this close to its expiry is not handed out any more. */
const MARGIN_MS = 30_000;
/** Larger text is not loaded into the chat; it can still be downloaded. */
export const MAX_INLINE_TEXT = 512 * 1024;
/** Larger text and diffs are only offered for download, not opened (T22). */
export const MAX_OPEN_TEXT = 2 * 1024 * 1024;

export class ArtifactStore {
  private readonly metas = new Map<string, Promise<Artifact>>();
  private readonly links = new Map<string, { url: string; expires: number }>();

  constructor(
    private readonly api: Source,
    private readonly fetchImpl: typeof fetch = (...a) => fetch(...a),
    private readonly now: () => number = Date.now,
    /** Where relative links point: the hub's own origin. */
    private readonly origin: string = typeof location !== 'undefined' ? location.origin : 'http://localhost',
  ) {}

  meta(id: string): Promise<Artifact> {
    let p = this.metas.get(id);
    if (!p) {
      p = this.api.artifact(id);
      this.metas.set(id, p);
      // A failure is not cached: the next render tries again.
      p.catch(() => this.metas.delete(id));
    }
    return p;
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
    for (const fresh of [false, true]) {
      const res = await this.fetchImpl(await this.url(id, fresh));
      if (res.ok) {
        const len = Number(res.headers.get('content-length') ?? NaN);
        if (len > maxBytes) {
          void res.body?.cancel();
          throw new Error('attachment too large');
        }
        return res.text();
      }
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
