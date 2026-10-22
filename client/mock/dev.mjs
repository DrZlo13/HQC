// `npm run dev:mock`: the mock hub plus the Vite dev server proxying /api to it.
// MOCK_PORT (default 8787) and MOCK_SPEED (default 1) as in mock/server.mjs.

import { startMock } from './server.mjs';

const mock = await startMock({
  port: Number(process.env.MOCK_PORT ?? 8787),
  speed: Number(process.env.MOCK_SPEED ?? 1),
  log: (l) => console.log(`[mock] ${l}`),
});
console.log(`[mock] hub on ${mock.url}`);

process.env.HQC_HUB_URL = mock.url;
const { createServer } = await import('vite');
const server = await createServer();
await server.listen();
server.printUrls();
