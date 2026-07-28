import { useEffect, useState } from 'preact/hooks'
import { apiErrorMessage } from '../api/client'
import { useServices } from '../services'
import { useShortcuts } from '../shortcuts'
import type { EventDto } from '../stores/timeline'
import { BodyPortal } from './BodyPortal'
import { FormattedBody } from './FormattedBody'
import { useModalFocus } from './use-modal-focus'

/**
 * The forensic edit trail of one event (ADR 0033; `GET …/events/{id}/edits`,
 * oldest first). Rendered as a dismissable overlay from the "(edited)"
 * marker.
 */
export function EditHistory({
  accountId,
  eventId,
  onClose,
}: {
  accountId: string
  eventId: string
  onClose: () => void
}) {
  const { api } = useServices()
  const [edits, setEdits] = useState<EventDto[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  const { containerRef } = useModalFocus<HTMLDivElement>()

  // As the topmost surface this modal claims Escape first — hence capture,
  // which does not depend on mount order relative to the page's handler.
  useShortcuts(
    {
      Escape: (event) => {
        event.preventDefault()
        onClose()
      },
    },
    { whileTyping: true, capture: true },
  )

  useEffect(() => {
    let cancelled = false
    void api
      .GET('/v1/accounts/{account_id}/events/{event_id}/edits', {
        params: { path: { account_id: accountId, event_id: eventId } },
      })
      .then(
        ({ data, error: apiError }) => {
          if (cancelled) {
            return
          }
          if (apiError !== undefined) {
            setError(apiErrorMessage(apiError))
          } else {
            setEdits(data.data)
          }
        },
        // Network-level failure rejects instead of returning the envelope
        // (WCR-02); it reads the same to the user.
        (cause: unknown) => {
          if (!cancelled) {
            setError(cause instanceof Error ? cause.message : String(cause))
          }
        },
      )
    return () => {
      cancelled = true
    }
  }, [api, accountId, eventId])

  return (
    <BodyPortal>
      <div
        ref={containerRef}
        class="overlay"
        role="dialog"
        aria-modal="true"
        aria-label="Edit history"
      >
        <div class="overlay-panel">
          <div class="overlay-head">
            <h2>Edit history</h2>
            <button type="button" class="ghost" onClick={onClose}>
              Close
            </button>
          </div>
          {error !== null && <p class="muted">Could not load edits: {error}</p>}
          {edits === null && error === null && <p class="muted">Loading…</p>}
          {edits !== null && edits.length === 0 && (
            <p class="muted">No edit events stored for this message.</p>
          )}
          {edits !== null && edits.length > 0 && (
            <ol class="edit-trail">
              {edits.map((edit) => (
                <li key={edit.event_id}>
                  <span class="muted">
                    {new Date(edit.origin_ts).toLocaleString()}
                  </span>{' '}
                  <FormattedBody
                    accountId={accountId}
                    body={newContentBody(edit) ?? edit.body}
                    content={newContent(edit)}
                  />
                </li>
              ))}
            </ol>
          )}
        </div>
      </div>
    </BodyPortal>
  )
}

/**
 * An `m.replace` edit carries the replacement under `m.new_content`; the
 * top-level body is the `* `-prefixed fallback. Prefer the real content.
 */
function newContent(edit: EventDto): unknown {
  const content = edit.content as { 'm.new_content'?: unknown } | null
  return content?.['m.new_content'] ?? edit.content
}

function newContentBody(edit: EventDto): string | null {
  const inner = newContent(edit) as { body?: unknown } | null
  return typeof inner?.body === 'string' ? inner.body : null
}
