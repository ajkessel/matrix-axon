/// <reference types="vitest/config" />
import { execFileSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { defineConfig } from 'vite'
import preact from '@preact/preset-vite'

// In development the axon server runs on another origin and serves no CORS
// headers (ADR 0046), so the dev server proxies API and WebSocket traffic.
// Override the target with AXON_SERVER_URL if your server is not on :8080.
const axonServer = process.env.AXON_SERVER_URL ?? 'http://localhost:8080'

// Vite blocks dev-server requests whose Host header isn't localhost. To reach
// the dev server through another hostname (a tunnel, a LAN name, a reverse
// proxy), list the extra hostnames — comma-separated — without editing this
// file: AXON_DEV_ALLOWED_HOSTS=axon-web.example.net,axon-dev.local pnpm dev
const allowedHosts = (process.env.AXON_DEV_ALLOWED_HOSTS ?? '')
  .split(',')
  .map((host) => host.trim())
  .filter((host) => host !== '')
const webClientDir = fileURLToPath(new URL('.', import.meta.url))

function git(args: string[]): string | null {
  try {
    return execFileSync('git', args, {
      cwd: webClientDir,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    }).trim()
  } catch {
    return null
  }
}

function webClientVersion(): string {
  const override = process.env.VITE_AXON_WEB_VERSION?.trim()
  if (override) {
    return override
  }

  const hash = git(['rev-parse', '--short=12', 'HEAD']) ?? 'unknown'
  const dirty = git(['status', '--short', '--', '.']) !== ''
  return dirty ? `${hash}-dirty` : hash
}

export default defineConfig({
  plugins: [preact()],
  define: {
    __AXON_WEB_VERSION__: JSON.stringify(webClientVersion()),
    __AXON_WEB_BUILT_AT__: JSON.stringify(new Date().toISOString()),
  },
  server: {
    allowedHosts,
    proxy: {
      '/v1': {
        target: axonServer,
        changeOrigin: true,
        ws: true,
      },
    },
  },
  test: {
    environment: 'jsdom',
    setupFiles: ['./src/test/setup.ts'],
    // Unit tests live in src; the Playwright e2e specs in e2e/ are run by
    // `pnpm test:e2e`, not vitest (both use the `.spec.ts` suffix).
    include: ['src/**/*.{test,spec}.{ts,tsx}'],
  },
})
