import { defineConfig } from 'vitest/config';
import preact from '@preact/preset-vite';

// The real hub; `npm run dev:mock` starts the mock and points this at it.
const hub = process.env.HQC_HUB_URL ?? 'http://127.0.0.1:8080';

export default defineConfig({
  plugins: [preact()],
  server: {
    // Loopback by default: the proxied dev hub has no authentication. HQC_DEV_LAN=1 opens
    // the dev server to the LAN (a phone on Wi-Fi) — only with `npm run dev:mock`.
    host: process.env.HQC_DEV_LAN === '1' ? true : '127.0.0.1',
    // Extra host names, e.g. an HTTPS tunnel for testing push on a phone.
    allowedHosts: process.env.HQC_DEV_ALLOWED_HOSTS?.split(',').filter(Boolean),
    proxy: {
      // The Host header stays the dev server's, so the hub's Origin check (SPEC §20) sees
      // the page's own origin.
      '/api': { target: hub, ws: true },
    },
  },
  test: {
    environment: 'node',
    include: ['tests/**/*.test.{ts,tsx}'],
    // Scenario tests wait on the mock's (sped up) turn.
    testTimeout: 20_000,
  },
});
