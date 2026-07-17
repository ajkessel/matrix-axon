import { useEffect, useState } from 'preact/hooks'
import { ErrorBanner } from '../components/ErrorBanner'
import { useServices } from '../services'
import type { Account } from '../stores/accounts'
import { ServerStatus } from './ServerStatus'

/**
 * The account lifecycle page (ADR 0046, M-W3): list with state and
 * sync-readiness, login (add account), logout, recover, delete-with-confirm,
 * and the active-account switch persisted in settings.
 */
export function AccountsPage() {
  return (
    <div class="page">
      <h1>Accounts</h1>
      <AccountLifecycle />
    </div>
  )
}

export function AccountLifecycle() {
  const { accounts } = useServices()

  useEffect(() => {
    void accounts.refresh()
  }, [accounts])

  return (
    <>
      <ErrorBanner error={accounts.error} />
      {accounts.loading.value ? (
        <p>Loading accounts…</p>
      ) : accounts.accounts.value.length === 0 ? (
        <p>No accounts yet — add one below.</p>
      ) : (
        <ul class="cards">
          {accounts.accounts.value.map((account) => (
            <AccountCard key={account.account_id} account={account} />
          ))}
        </ul>
      )}
      <AddAccountForm />
      <ServerStatus />
    </>
  )
}

/**
 * Sync-readiness per ADR 0030. The server does not expose `sync_state` yet
 * (see `stores/accounts.ts`); when absent nothing renders, and when the
 * server catches up the badge lights up without a client change.
 */
function SyncBadge({ account }: { account: Account }) {
  const state = account.sync_state
  if (state === undefined || state === 'ready') {
    return null
  }
  const hint =
    state === 'offline'
      ? 'server lost the homeserver connection and is retrying'
      : 'first sync not finished — sending may be unreliable'
  return (
    <span class={`badge sync-${state}`} title={hint}>
      {state}
    </span>
  )
}

function AccountCard({ account }: { account: Account }) {
  const { accounts, settings } = useServices()
  const [confirmingDelete, setConfirmingDelete] = useState(false)
  const [recoverKey, setRecoverKey] = useState<string | null>(null)

  const id = account.account_id
  const pending = accounts.pending.value
  const busy = pending !== null
  const isActiveChoice = settings.activeAccountId.value === id

  return (
    <li class={`card account-${account.state}`}>
      <div class="card-head">
        <strong>{account.user_id}</strong>
        <span class={`badge state-${account.state}`}>{account.state}</span>
        {account.verified === true && (
          <span class="badge verified" title="axon's device is cross-signed">
            verified
          </span>
        )}
        <SyncBadge account={account} />
      </div>
      <div class="card-meta">{account.homeserver_url}</div>

      <div class="card-actions">
        {account.state === 'active' && (
          <>
            <label class="switch">
              <input
                type="radio"
                name="active-account"
                checked={isActiveChoice}
                onChange={() => (settings.activeAccountId.value = id)}
              />
              use this account
            </label>
            <button
              type="button"
              disabled={busy}
              onClick={() => setRecoverKey(recoverKey === null ? '' : null)}
            >
              Recover keys
            </button>
            <button
              type="button"
              disabled={busy}
              onClick={() => void accounts.logout(id)}
            >
              Log out
            </button>
          </>
        )}
        {account.state !== 'deleting' &&
          (confirmingDelete ? (
            <span class="confirm">
              Delete {account.user_id} from axon permanently?
              <button
                type="button"
                class="danger"
                disabled={busy}
                onClick={() => {
                  void accounts
                    .remove(id)
                    .then(() => setConfirmingDelete(false))
                }}
              >
                Confirm delete
              </button>
              <button type="button" onClick={() => setConfirmingDelete(false)}>
                Cancel
              </button>
            </span>
          ) : (
            <button
              type="button"
              class="danger"
              disabled={busy}
              onClick={() => setConfirmingDelete(true)}
            >
              Delete
            </button>
          ))}
      </div>

      {recoverKey !== null && account.state === 'active' && (
        <form
          class="inline-form"
          onSubmit={(event) => {
            event.preventDefault()
            void accounts
              .recover(id, recoverKey)
              .then((ok) => ok && setRecoverKey(null))
          }}
        >
          <label>
            Recovery key
            <input
              type="password"
              value={recoverKey}
              placeholder="EsTc …"
              onInput={(event) => setRecoverKey(event.currentTarget.value)}
            />
          </label>
          <button type="submit" disabled={busy || recoverKey.trim() === ''}>
            Recover
          </button>
        </form>
      )}
    </li>
  )
}

function AddAccountForm() {
  const { accounts } = useServices()
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [recoveryKey, setRecoveryKey] = useState('')
  const [homeserver, setHomeserver] = useState('')
  const busy = accounts.pending.value !== null

  return (
    <div class="account-add">
      <h2>Add account</h2>
      <form
        class="stack-form"
        onSubmit={(event) => {
          event.preventDefault()
          void accounts
            .login({
              username: username.trim(),
              password,
              recovery_key:
                recoveryKey.trim() === '' ? undefined : recoveryKey.trim(),
              homeserver_url:
                homeserver.trim() === '' ? undefined : homeserver.trim(),
            })
            .then((ok) => {
              setPassword('')
              setRecoveryKey('')
              if (ok) {
                setUsername('')
                setHomeserver('')
              }
            })
        }}
      >
        <label>
          Matrix user ID
          <input
            value={username}
            placeholder="@alice:example.org"
            onInput={(event) => setUsername(event.currentTarget.value)}
          />
        </label>
        <label>
          Password
          <input
            type="password"
            value={password}
            onInput={(event) => setPassword(event.currentTarget.value)}
          />
        </label>
        <label>
          Matrix Recovery Key (optional - can be entered later or skipped with
          SAS verification)
          <input
            type="password"
            value={recoveryKey}
            placeholder="EsTc …"
            onInput={(event) => setRecoveryKey(event.currentTarget.value)}
          />
        </label>
        <label>
          Homeserver URL{' '}
          <span class="muted">(optional — autodiscovered when omitted)</span>
          <input
            value={homeserver}
            placeholder="https://matrix.example.org"
            onInput={(event) => setHomeserver(event.currentTarget.value)}
          />
        </label>
        <button
          type="submit"
          disabled={busy || username.trim() === '' || password === ''}
        >
          Log in
        </button>
      </form>
      <p class="muted">
        The password authenticates once with the homeserver and is never stored.
        Logging in with a logged-out account's user ID reactivates it.
      </p>
    </div>
  )
}
