import {
  computed,
  signal,
  type ReadonlySignal,
  type Signal,
} from '@preact/signals'
import { apiErrorMessage, type ApiClient } from '../api/client'
import type { components } from '../api/schema'

export type AccountDto = components['schemas']['AccountDto']

/**
 * ADR 0030 sync-readiness. The ADR is accepted but the server does not yet
 * expose `sync_state` on `AccountDto` (no field in the OpenAPI contract as of
 * M-W3), so the generated type has no such property. This local extension
 * reads it opportunistically: the badge stays hidden until a server release
 * adds the field, and when `gen:api` picks it up this alias collapses into
 * the generated type and can be deleted.
 */
export type SyncState = 'connecting' | 'syncing' | 'ready' | 'offline'
export type Account = AccountDto & { sync_state?: SyncState }

/** One in-flight lifecycle action, so the UI can disable just that control. */
export type PendingAction =
  | { kind: 'login' }
  | { kind: 'logout' | 'recover' | 'delete'; accountId: string }

export interface AccountsStore {
  accounts: ReadonlySignal<Account[]>
  /** True until the first load settles (success or failure). */
  loading: ReadonlySignal<boolean>
  /** Human-readable failure of the last operation, or null. */
  error: Signal<string | null>
  pending: ReadonlySignal<PendingAction | null>

  /** Fetch the account list; used at mount and after every mutation. */
  refresh(): Promise<void>
  /** POST /v1/accounts/login — add or reactivate an account. */
  login(req: {
    username: string
    password: string
    homeserver_url?: string
    recovery_key?: string
  }): Promise<boolean>
  /** POST /v1/accounts/{id}/logout — deactivate. */
  logout(accountId: string): Promise<boolean>
  /** POST /v1/accounts/{id}/recover — import keys with a 4S recovery key. */
  recover(accountId: string, recoveryKey: string): Promise<boolean>
  /** DELETE /v1/accounts/{id} — permanent removal. */
  remove(accountId: string): Promise<boolean>
}

/**
 * Account lifecycle state over the API client (ADR 0046, M-W3).
 *
 * Mutations follow one pattern: run, surface the error envelope's message on
 * failure, refresh the list on success — the server's response is the truth,
 * not an optimistic local edit. `error` is a plain writable signal so the UI
 * can also dismiss it.
 */
export function createAccountsStore(api: ApiClient): AccountsStore {
  const accounts = signal<Account[]>([])
  const loading = signal(true)
  const error = signal<string | null>(null)
  const pending = signal<PendingAction | null>(null)

  async function refresh(): Promise<void> {
    try {
      const { data, error: apiError } = await api.GET('/v1/accounts')
      if (apiError !== undefined) {
        error.value = apiErrorMessage(apiError)
        return
      }
      accounts.value = data.data
      error.value = null
    } catch (cause) {
      error.value = cause instanceof Error ? cause.message : String(cause)
    } finally {
      loading.value = false
    }
  }

  /** Shared mutation wrapper: pending flag, error surfacing, refresh. */
  async function run(
    action: PendingAction,
    call: () => Promise<{ error?: unknown; response: Response }>,
  ): Promise<boolean> {
    if (pending.value !== null) {
      return false
    }
    pending.value = action
    try {
      const { error: apiError } = await call()
      if (apiError !== undefined) {
        error.value = apiErrorMessage(apiError)
        return false
      }
      error.value = null
      await refresh()
      return true
    } catch (cause) {
      error.value = cause instanceof Error ? cause.message : String(cause)
      return false
    } finally {
      pending.value = null
    }
  }

  return {
    accounts: computed(() => accounts.value),
    loading: computed(() => loading.value),
    error,
    pending: computed(() => pending.value),
    refresh,

    login: async (req) => {
      if (pending.value !== null) {
        return false
      }
      pending.value = { kind: 'login' }
      let shouldRefresh = false
      let operationError: string | null = null
      try {
        const { data, error: apiError } = await api.POST('/v1/accounts/login', {
          body: {
            username: req.username,
            password: req.password,
            homeserver_url: req.homeserver_url ?? null,
          },
        })
        if (apiError !== undefined) {
          error.value = apiErrorMessage(apiError)
          return false
        }

        shouldRefresh = true
        const recoveryKey = req.recovery_key?.trim() ?? ''
        if (recoveryKey !== '') {
          const { error: recoverError } = await api.POST(
            '/v1/accounts/{account_id}/recover',
            {
              params: { path: { account_id: data.data.account_id } },
              body: { recovery_key: recoveryKey },
            },
          )
          if (recoverError !== undefined) {
            operationError = apiErrorMessage(recoverError)
          }
        }

        error.value = null
        return operationError === null
      } catch (cause) {
        operationError = cause instanceof Error ? cause.message : String(cause)
        return false
      } finally {
        if (shouldRefresh) {
          await refresh()
        }
        if (operationError !== null) {
          error.value = operationError
        }
        pending.value = null
      }
    },

    logout: (accountId) =>
      run({ kind: 'logout', accountId }, () =>
        api.POST('/v1/accounts/{account_id}/logout', {
          params: { path: { account_id: accountId } },
        }),
      ),

    recover: (accountId, recoveryKey) =>
      run({ kind: 'recover', accountId }, () =>
        api.POST('/v1/accounts/{account_id}/recover', {
          params: { path: { account_id: accountId } },
          body: { recovery_key: recoveryKey },
        }),
      ),

    remove: (accountId) =>
      run({ kind: 'delete', accountId }, () =>
        api.DELETE('/v1/accounts/{account_id}', {
          params: { path: { account_id: accountId } },
        }),
      ),
  }
}
