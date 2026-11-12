// A HEIC or HEIF file in, a JPEG out (null when it cannot be made), off the main thread:
// libheif takes about a second for a 12-megapixel photo.

import { decodeHeic } from './heic-decode';

// libheif decodes inside a timer of its own: a failure there escapes the `try` below as an
// unhandled rejection, which never reaches the page's `onerror`.
self.onunhandledrejection = () => postMessage(null);

self.onmessage = async (e: MessageEvent<ArrayBuffer>) => {
  let jpeg: Blob | null = null;
  try {
    const { width, height, data } = await decodeHeic(e.data);
    const c = new OffscreenCanvas(width, height);
    c.getContext('2d')!.putImageData(new ImageData(data, width, height), 0, 0);
    jpeg = await c.convertToBlob({ type: 'image/jpeg', quality: 0.9 });
  } catch {
    // Not a file libheif reads, or no OffscreenCanvas: the caller keeps the HEIC.
  }
  postMessage(jpeg);
};
