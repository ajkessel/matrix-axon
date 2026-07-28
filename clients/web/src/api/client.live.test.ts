/**
 * Live round-trip against a real axon server — the M-W2 exit criterion.
 * Skipped unless both env vars are set:
 *
 * ```sh
 * AXON_LIVE_URL=http://localhost:8080 AXON_LIVE_TOKEN=$(axon token issue …) pnpm test
 * ```
 */
import { describe, expect, it } from 'vitest'
import { createApiClient } from './client'

// Tests run under Node, but the app tsconfig is browser-typed and this is the
// one file that reads the environment — declare just what it uses.
declare const process: { env: Record<string, string | undefined> }

const liveUrl = process.env.AXON_LIVE_URL
const liveToken = process.env.AXON_LIVE_TOKEN

describe.runIf(liveUrl !== undefined && liveToken !== undefined)(
  'live server round-trip',
  () => {
    it('GET /v1/accounts succeeds with a real token', async () => {
      const api = createApiClient(
        {
          getToken: () => liveToken!,
          onAuthFailure: () => {
            throw new Error('valid token was rejected')
          },
          LoginBootstrap: () => null,
        },
        liveUrl!,
      )

      const { data, error, response } = await api.GET('/v1/accounts')

      expect(error).toBeUndefined()
      expect(response.status).toBe(200)
      expect(Array.isArray(data?.data)).toBe(true)
    })

    it('a bad token gets the 401 envelope and the provider is told', async () => {
      let failures = 0
      const api = createApiClient(
        {
          getToken: () => 'not-a-real-token',
          onAuthFailure: () => {
            failures += 1
          },
          LoginBootstrap: () => null,
        },
        liveUrl!,
      )

      const { data, error, response } = await api.GET('/v1/accounts')

      expect(response.status).toBe(401)
      expect(data).toBeUndefined()
      expect(error).toMatchObject({ error: { code: 'unauthorized' } })
      expect(failures).toBe(1)
    })
  },
)
