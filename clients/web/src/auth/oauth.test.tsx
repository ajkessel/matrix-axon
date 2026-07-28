import { HttpResponse, http } from 'msw'
import { setupServer } from 'msw/node'
import { afterAll, afterEach, beforeAll, describe, expect, it } from 'vitest'
import { memoryStorage } from '../test/memory-storage'
import { createOAuthAuthProvider, parseOAuthProviders } from './oauth'

const BASE_URL = 'http://axon.test'
const TOKEN_URL = `${BASE_URL}/v1/oauth/token`

const server = setupServer()
beforeAll(() => server.listen({ onUnhandledRequest: 'error' }))
afterEach(() => {
  server.resetHandlers()
  history.replaceState(null, '', '/')
})
afterAll(() => server.close())

describe('parseOAuthProviders', () => {
  it('parses deployment configured provider labels', () => {
    expect(parseOAuthProviders('google:Google,microsoft:Microsoft')).toEqual([
      { provider: 'google', label: 'Google' },
      { provider: 'microsoft', label: 'Microsoft' },
    ])
  })

  it('defaults missing labels and rejects invalid provider ids', () => {
    expect(parseOAuthProviders('google, bad id:Bad,ms-work:Work')).toEqual([
      { provider: 'google', label: 'google' },
      { provider: 'ms-work', label: 'Work' },
    ])
  })
})

describe('createOAuthAuthProvider', () => {
  it('redeems a callback code and stores the returned token pair', async () => {
    const storage = memoryStorage()
    const pending = memoryStorage({
      'axon.oauth.pending': JSON.stringify({
        state: 'state-123',
        codeVerifier: 'verifier-123',
        provider: 'google',
        redirectUri: 'http://localhost:3000/oauth/callback',
        createdAt: Date.now(),
      }),
    })
    let form = ''
    server.use(
      http.post(TOKEN_URL, async ({ request }) => {
        form = await request.text()
        return HttpResponse.json({
          access_token: 'access-1',
          token_type: 'Bearer',
          expires_in: 3600,
          refresh_token: 'refresh-1',
        })
      }),
    )

    const auth = createOAuthAuthProvider({
      providers: [{ provider: 'google', label: 'Google' }],
      baseUrl: BASE_URL,
      storage,
      pendingStorage: pending,
    })
    const result = await auth.completeRedirect(
      new URL(
        'http://localhost:3000/oauth/callback?code=code-1&state=state-123',
      ),
    )

    expect(result).toEqual({ ok: true })
    expect(auth.signedIn.value).toBe(true)
    expect(auth.getToken()).toBe('access-1')
    expect(pending.getItem('axon.oauth.pending')).toBeNull()
    expect(new URLSearchParams(form).get('grant_type')).toBe(
      'authorization_code',
    )
    expect(new URLSearchParams(form).get('code_verifier')).toBe('verifier-123')
  })

  it('stores a one-time OAuth session in session storage', async () => {
    const storage = memoryStorage()
    const sessionStorage = memoryStorage()
    const pending = memoryStorage({
      'axon.oauth.pending': JSON.stringify({
        state: 'state-123',
        codeVerifier: 'verifier-123',
        provider: 'google',
        redirectUri: 'http://localhost:3000/oauth/callback',
        createdAt: Date.now(),
        storageMode: 'session',
      }),
    })
    server.use(
      http.post(TOKEN_URL, () =>
        HttpResponse.json({
          access_token: 'access-1',
          token_type: 'Bearer',
          expires_in: 3600,
          refresh_token: 'refresh-1',
        }),
      ),
    )

    const auth = createOAuthAuthProvider({
      providers: [{ provider: 'google', label: 'Google' }],
      baseUrl: BASE_URL,
      storage,
      sessionStorage,
      pendingStorage: pending,
    })
    const result = await auth.completeRedirect(
      new URL(
        'http://localhost:3000/oauth/callback?code=code-1&state=state-123',
      ),
    )

    expect(result).toEqual({ ok: true })
    expect(storage.getItem('axon.oauth.session')).toBeNull()
    expect(sessionStorage.getItem('axon.oauth.session')).not.toBeNull()
  })

  it('rejects a callback with mismatched state without calling token exchange', async () => {
    const pending = memoryStorage({
      'axon.oauth.pending': JSON.stringify({
        state: 'expected',
        codeVerifier: 'verifier-123',
        provider: 'google',
        redirectUri: 'http://localhost:3000/oauth/callback',
        createdAt: Date.now(),
      }),
    })
    const auth = createOAuthAuthProvider({
      providers: [{ provider: 'google', label: 'Google' }],
      baseUrl: BASE_URL,
      storage: memoryStorage(),
      pendingStorage: pending,
    })

    const result = await auth.completeRedirect(
      new URL('http://localhost:3000/oauth/callback?code=code-1&state=wrong'),
    )

    expect(result).toEqual({
      ok: false,
      message: 'OAuth sign-in state did not match',
    })
    expect(auth.signedIn.value).toBe(false)
    expect(pending.getItem('axon.oauth.pending')).toBeNull()
  })

  it('refreshes an expired access token and rotates the stored refresh token', async () => {
    const storage = memoryStorage({
      'axon.oauth.session': JSON.stringify({
        accessToken: 'old-access',
        refreshToken: 'old-refresh',
        expiresAt: Date.now() - 1000,
        provider: 'microsoft',
      }),
    })
    let form = ''
    server.use(
      http.post(TOKEN_URL, async ({ request }) => {
        form = await request.text()
        return HttpResponse.json({
          access_token: 'new-access',
          token_type: 'Bearer',
          expires_in: 3600,
          refresh_token: 'new-refresh',
        })
      }),
    )
    const auth = createOAuthAuthProvider({
      providers: [{ provider: 'microsoft', label: 'Microsoft' }],
      baseUrl: BASE_URL,
      storage,
      pendingStorage: memoryStorage(),
    })

    await expect(auth.getToken()).resolves.toBe('new-access')

    const saved = JSON.parse(storage.getItem('axon.oauth.session')!)
    expect(new URLSearchParams(form).get('grant_type')).toBe('refresh_token')
    expect(new URLSearchParams(form).get('refresh_token')).toBe('old-refresh')
    expect(saved.accessToken).toBe('new-access')
    expect(saved.refreshToken).toBe('new-refresh')
    expect(saved.provider).toBe('microsoft')
  })

  it('refreshes an expired one-time access token without promoting it', async () => {
    const storage = memoryStorage()
    const sessionStorage = memoryStorage({
      'axon.oauth.session': JSON.stringify({
        accessToken: 'old-access',
        refreshToken: 'old-refresh',
        expiresAt: Date.now() - 1000,
        provider: 'microsoft',
      }),
    })
    server.use(
      http.post(TOKEN_URL, () =>
        HttpResponse.json({
          access_token: 'new-access',
          token_type: 'Bearer',
          expires_in: 3600,
          refresh_token: 'new-refresh',
        }),
      ),
    )
    const auth = createOAuthAuthProvider({
      providers: [{ provider: 'microsoft', label: 'Microsoft' }],
      baseUrl: BASE_URL,
      storage,
      sessionStorage,
      pendingStorage: memoryStorage(),
    })

    await expect(auth.getToken()).resolves.toBe('new-access')

    expect(storage.getItem('axon.oauth.session')).toBeNull()
    const saved = JSON.parse(sessionStorage.getItem('axon.oauth.session')!)
    expect(saved.accessToken).toBe('new-access')
    expect(saved.refreshToken).toBe('new-refresh')
  })

  it('clears the OAuth session when refresh fails', async () => {
    const storage = memoryStorage({
      'axon.oauth.session': JSON.stringify({
        accessToken: 'old-access',
        refreshToken: 'old-refresh',
        expiresAt: Date.now() - 1000,
        provider: 'google',
      }),
    })
    server.use(
      http.post(TOKEN_URL, () =>
        HttpResponse.json(
          {
            error: 'invalid_grant',
            error_description: 'refresh token expired',
          },
          { status: 400 },
        ),
      ),
    )
    const auth = createOAuthAuthProvider({
      providers: [{ provider: 'google', label: 'Google' }],
      baseUrl: BASE_URL,
      storage,
      pendingStorage: memoryStorage(),
    })

    await expect(auth.getToken()).resolves.toBeNull()
    expect(auth.signedIn.value).toBe(false)
    expect(storage.getItem('axon.oauth.session')).toBeNull()
  })
})
