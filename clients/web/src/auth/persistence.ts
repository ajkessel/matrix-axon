import { signal, type Signal } from '@preact/signals'

export type AuthStorageMode = 'persistent' | 'session'

export interface StoredAuthValue {
  value: string
  mode: AuthStorageMode
}

export interface AuthPersistence {
  rememberMe: Signal<boolean>
  read(key: string): StoredAuthValue | null
  write(key: string, value: string, mode?: AuthStorageMode): void
  remove(key: string): void
}

export interface AuthPersistenceOptions {
  persistentStorage?: Storage
  sessionStorage?: Storage
  rememberMeDefault?: boolean
}

export function createAuthPersistence({
  persistentStorage = window.localStorage,
  sessionStorage = window.sessionStorage,
  rememberMeDefault = true,
}: AuthPersistenceOptions = {}): AuthPersistence {
  const rememberMe = signal(rememberMeDefault)

  return {
    rememberMe,
    read(key) {
      const persistent = persistentStorage.getItem(key)
      if (persistent !== null) {
        return { value: persistent, mode: 'persistent' }
      }
      const session = sessionStorage.getItem(key)
      return session === null ? null : { value: session, mode: 'session' }
    },
    write(key, value, mode = rememberMe.value ? 'persistent' : 'session') {
      if (mode === 'persistent') {
        persistentStorage.setItem(key, value)
        sessionStorage.removeItem(key)
      } else {
        sessionStorage.setItem(key, value)
        persistentStorage.removeItem(key)
      }
    },
    remove(key) {
      persistentStorage.removeItem(key)
      sessionStorage.removeItem(key)
    },
  }
}
