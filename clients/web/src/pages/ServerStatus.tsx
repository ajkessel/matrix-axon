import { useEffect, useState } from 'preact/hooks'
import { apiErrorMessage } from '../api/client'
import type { components } from '../api/schema'
import { useServices } from '../services'

type StatusDto = components['schemas']['StatusDto']

/**
 * Server status (`GET /v1/status`): the backfill engine's disk-space health
 * and per-account progress — the "status" leg of the M-W3 lifecycle scope.
 */
export function ServerStatus() {
  const { api } = useServices()
  const [status, setStatus] = useState<StatusDto | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let cancelled = false
    void api.GET('/v1/status').then(
      ({ data, error: apiError }) => {
        if (cancelled) {
          return
        }
        if (apiError !== undefined) {
          setError(apiErrorMessage(apiError))
        } else {
          setStatus(data.data)
        }
      },
      (cause: unknown) => {
        if (!cancelled) {
          setError(cause instanceof Error ? cause.message : String(cause))
        }
      },
    )
    return () => {
      cancelled = true
    }
  }, [api])

  if (error !== null) {
    return <p class="muted">Server status unavailable: {error}</p>
  }
  if (status === null) {
    return null
  }

  const backfill = status.backfill
  const gib = (backfill.free_bytes / 1024 ** 3).toFixed(1)
  return (
    <section class="panel">
      <h2>Server status</h2>
      <p>
        History backfill:{' '}
        {backfill.paused ? (
          <strong class="warn">
            paused ({backfill.reason ?? 'unknown reason'})
          </strong>
        ) : (
          'running'
        )}{' '}
        · {gib} GiB free
      </p>
      {backfill.accounts.length > 0 && (
        <ul class="status-list">
          {backfill.accounts.map((account) => (
            <li key={account.account_id}>
              <code>{account.account_id.slice(0, 8)}</code>:{' '}
              {account.rooms_backfilled}/{account.rooms_total} rooms backfilled,{' '}
              {account.events} events
              {account.complete ? ' — complete' : ''}
            </li>
          ))}
        </ul>
      )}
    </section>
  )
}
