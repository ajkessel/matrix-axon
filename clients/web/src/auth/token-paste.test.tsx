import { cleanup, fireEvent, render } from '@testing-library/preact'
import { afterEach, describe, expect, it } from 'vitest'
import { memoryStorage } from '../test/memory-storage'
import { createAuthPersistence } from './persistence'
import { createTokenPasteProvider } from './token-paste'

// testing-library's auto-cleanup needs vitest's injected globals, which this
// config doesn't enable; unmount between tests explicitly.
afterEach(cleanup)

describe('createTokenPasteProvider', () => {
  it('starts signed out with an empty storage', () => {
    const provider = createTokenPasteProvider(memoryStorage())
    expect(provider.getToken()).toBeNull()
    expect(provider.signedIn.value).toBe(false)
  })

  it('restores a previously stored token', () => {
    const provider = createTokenPasteProvider(
      memoryStorage({ 'axon.token': 'tok-persisted' }),
    )
    expect(provider.getToken()).toBe('tok-persisted')
    expect(provider.signedIn.value).toBe(true)
  })

  it('setToken trims, persists, and flips signedIn', () => {
    const storage = memoryStorage()
    const provider = createTokenPasteProvider(storage)
    provider.setToken('  tok-123  ')
    expect(provider.getToken()).toBe('tok-123')
    expect(storage.getItem('axon.token')).toBe('tok-123')
    expect(provider.signedIn.value).toBe(true)
  })

  it('stores a one-time token in session storage when remember me is off', () => {
    const persistentStorage = memoryStorage()
    const sessionStorage = memoryStorage()
    const persistence = createAuthPersistence({
      persistentStorage,
      sessionStorage,
      rememberMeDefault: false,
    })
    const provider = createTokenPasteProvider(persistence)

    provider.setToken('tok-session')

    expect(provider.getToken()).toBe('tok-session')
    expect(persistentStorage.getItem('axon.token')).toBeNull()
    expect(sessionStorage.getItem('axon.token')).toBe('tok-session')
  })

  it('restores a token from session storage', () => {
    const provider = createTokenPasteProvider(
      createAuthPersistence({
        persistentStorage: memoryStorage(),
        sessionStorage: memoryStorage({ 'axon.token': 'tok-session' }),
      }),
    )

    expect(provider.getToken()).toBe('tok-session')
    expect(provider.signedIn.value).toBe(true)
  })

  it('ignores a blank paste', () => {
    const storage = memoryStorage()
    const provider = createTokenPasteProvider(storage)
    provider.setToken('   ')
    expect(provider.getToken()).toBeNull()
    expect(storage.getItem('axon.token')).toBeNull()
  })

  it('onAuthFailure discards the token (revoked server-side)', () => {
    const storage = memoryStorage()
    const provider = createTokenPasteProvider(storage)
    provider.setToken('tok-revoked')
    provider.onAuthFailure()
    expect(provider.getToken()).toBeNull()
    expect(storage.getItem('axon.token')).toBeNull()
    expect(provider.signedIn.value).toBe(false)
  })

  it('clearToken signs out explicitly', () => {
    const provider = createTokenPasteProvider(memoryStorage())
    provider.setToken('tok-123')
    provider.clearToken()
    expect(provider.signedIn.value).toBe(false)
  })
})

describe('LoginBootstrap', () => {
  it('stores the pasted token on submit', () => {
    const provider = createTokenPasteProvider(memoryStorage())
    const { getByLabelText, getByRole } = render(<provider.LoginBootstrap />)

    fireEvent.input(getByLabelText('Access token'), {
      target: { value: 'tok-pasted' },
    })
    fireEvent.click(getByRole('button', { name: 'Sign in' }))

    expect(provider.getToken()).toBe('tok-pasted')
    expect(provider.signedIn.value).toBe(true)
  })

  it('disables submit while the field is blank', () => {
    const provider = createTokenPasteProvider(memoryStorage())
    const { getByRole } = render(<provider.LoginBootstrap />)
    expect(
      (getByRole('button', { name: 'Sign in' }) as HTMLButtonElement).disabled,
    ).toBe(true)
  })
})
