// HEIC and HEIF where the browser does not decode them (Chrome, Firefox, Brave on a computer):
// libheif decodes them into a JPEG in a worker. The worker is a file of its own, so libheif's
// 3 MB are loaded only when a HEIC needs them.

export const HEIC_NAME = /\.hei[cf]$/i;

/** `b` decoded into a JPEG, or null where it cannot be. One worker per file, ended when it
 *  answers. */
export async function heicToJpeg(b: Blob): Promise<Blob | null> {
  if (typeof Worker === 'undefined') return null;
  const buf = await b.arrayBuffer();
  let w: Worker;
  try {
    w = new Worker(new URL('./heic.worker.ts', import.meta.url), { type: 'module' });
  } catch {
    // A worker the page may not start: the file stays as it is.
    return null;
  }
  return new Promise((resolve) => {
    const done = (jpeg: Blob | null) => {
      w.terminate();
      resolve(jpeg);
    };
    w.onmessage = (e: MessageEvent<Blob | null>) => done(e.data);
    w.onerror = () => done(null);
    w.postMessage(buf, [buf]);
  });
}

export function dataUrl(b: Blob): Promise<string | undefined> {
  if (typeof FileReader === 'undefined') return Promise.resolve(undefined);
  return new Promise((resolve) => {
    const r = new FileReader();
    r.onload = () => resolve(typeof r.result === 'string' ? r.result : undefined);
    r.onerror = () => resolve(undefined);
    r.readAsDataURL(b);
  });
}
