import { defineConfig, devices } from '@playwright/test'

const PORT = 4599
const BASE_URL = `http://127.0.0.1:${PORT}`

/**
 * The web e2e lanes: a headless Chromium drives the built app against the
 * dependency-free mock backend (`e2e/mock-server.mjs`), which serves `dist/`
 * and a real `/v1/ws`. Run `pnpm build` first — the mock serves static assets,
 * so the app must be built. `reuseExistingServer` keeps local reruns fast; CI
 * starts fresh.
 *
 * Lanes: live sockets (ADR 0061), two-pane layout (ADR 0062), keyboard
 * shortcuts (ADR 0063).
 *
 * One worker, because the mock backend is a single process with shared state:
 * `two-tabs-live` posts `/__e2e/drop-sockets`, which severs *every* connected
 * socket. Run in parallel, that would knock the other specs offline mid-test.
 */
export default defineConfig({
  testDir: './e2e',
  workers: 1,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? 'github' : 'list',
  use: {
    baseURL: BASE_URL,
    trace: 'on-first-retry',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: 'node e2e/mock-server.mjs',
    url: BASE_URL,
    env: { PORT: String(PORT) },
    reuseExistingServer: !process.env.CI,
    timeout: 30_000,
  },
})
