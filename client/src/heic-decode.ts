// HEIC and HEIF decoded in JavaScript, for browsers that do not decode them: libheif's
// pure-JavaScript build, as the hub's CSP allows no WebAssembly. Loaded only by the HEIC
// worker.

import libheif from 'libheif-js';

/** The first image of a HEIC or HEIF file as RGBA pixels. */
export async function decodeHeic(buf: ArrayBuffer | Uint8Array): Promise<{ width: number; height: number; data: Uint8ClampedArray<ArrayBuffer> }> {
  const images = new libheif.HeifDecoder().decode(buf instanceof Uint8Array ? buf : new Uint8Array(buf));
  try {
    const img = images[0];
    if (!img) throw new Error('not a HEIC image');
    const width = img.get_width();
    const height = img.get_height();
    const data = new Uint8ClampedArray(width * height * 4);
    await new Promise<void>((resolve, reject) =>
      img.display({ data, width, height }, (d) => (d ? resolve() : reject(new Error('HEIC decoding failed')))),
    );
    return { width, height, data };
  } finally {
    for (const i of images) i.free();
  }
}
