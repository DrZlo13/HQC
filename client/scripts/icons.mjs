// Generates the PNG app icons in public/ (a hub: ring with three spokes) without image
// libraries. Run after changing the design: `node scripts/icons.mjs`.

import { deflateSync } from 'node:zlib';
import { writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const OUT = join(dirname(fileURLToPath(import.meta.url)), '../public');
const BG = [0x1b, 0x1f, 0x27];
const FG = [0x6b, 0x9b, 0xff];

function crc32(buf) {
  let c = ~0;
  for (const b of buf) {
    c ^= b;
    for (let k = 0; k < 8; k++) c = (c >>> 1) ^ (0xedb88320 & -(c & 1));
  }
  return ~c >>> 0;
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const td = Buffer.concat([Buffer.from(type), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(td));
  return Buffer.concat([len, td, crc]);
}

/** Coverage of the foreground at (x, y) in unit coordinates centred on 0. */
function shape(x, y) {
  const r = Math.hypot(x, y);
  if (r < 0.1) return 1; // hub
  if (Math.abs(r - 0.3) < 0.045) return 1; // ring
  for (const a of [-Math.PI / 2, Math.PI / 6, (5 * Math.PI) / 6]) {
    const ux = Math.cos(a);
    const uy = Math.sin(a);
    const t = x * ux + y * uy;
    const d = Math.abs(-x * uy + y * ux);
    if (t > 0 && t < 0.3 && d < 0.03) return 1; // spoke
    if (Math.hypot(x - 0.3 * ux, y - 0.3 * uy) < 0.075) return 1; // node on the ring
  }
  return 0;
}

function png(size) {
  const raw = Buffer.alloc(size * (size * 3 + 1));
  const ss = 4;
  for (let y = 0; y < size; y++) {
    const row = y * (size * 3 + 1);
    raw[row] = 0;
    for (let x = 0; x < size; x++) {
      let cov = 0;
      for (let sy = 0; sy < ss; sy++) {
        for (let sx = 0; sx < ss; sx++) {
          cov += shape((x + (sx + 0.5) / ss) / size - 0.5, (y + (sy + 0.5) / ss) / size - 0.5);
        }
      }
      cov /= ss * ss;
      for (let c = 0; c < 3; c++) raw[row + 1 + x * 3 + c] = Math.round(BG[c] + (FG[c] - BG[c]) * cov);
    }
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8;
  ihdr[9] = 2;
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr),
    chunk('IDAT', deflateSync(raw, { level: 9 })),
    chunk('IEND', Buffer.alloc(0)),
  ]);
}

for (const [name, size] of [
  ['icon-192.png', 192],
  ['icon-512.png', 512],
  ['apple-touch-icon.png', 180],
]) {
  writeFileSync(join(OUT, name), png(size));
}
