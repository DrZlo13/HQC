import { defineConfig } from 'vitest/config';
import preact from '@preact/preset-vite';

// The real hub; `npm run dev:mock` starts the mock and points this at it.
const hub = process.env.HQC_HUB_URL ?? 'http://127.0.0.1:8080';

export default defineConfig({
  plugins: [preact()],
  server: {
    // All interfaces, so a phone on the LAN can open the dev server.
    host: true,
    // Extra host names, e.g. an HTTPS tunnel for testing push on a phone.
    allowedHosts: process.env.HQC_DEV_ALLOWED_HOSTS?.split(',').filter(Boolean),
    proxy: {
      '/api': { target: hub, changeOrigin: true, ws: true },
    },
  },
  test: {
    environment: 'node',
    include: ['tests/**/*.test.{ts,tsx}'],
    // Scenario tests wait on the mock's (sped up) turn.
    testTimeout: 20_000,
  },
});
