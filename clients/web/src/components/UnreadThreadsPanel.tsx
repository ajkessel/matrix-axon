import { useLocation } from 'preact-iso'
import { useServices } from '../services'
import { useShortcuts } from '../shortcuts'
import type { ThreadUnreadEntry } from '../stores/thread-unread'
import { useModalFocus } from './use-modal-focus'

export function UnreadThreadsPanel({ onClose }: { onClose: () => void }) {
  const { threadUnread } = useServices()
  const location = useLocation()
  const { containerRef } = useModalFocus<HTMLDivElement>()
  const entries = threadUnread.entries.value

  useShortcuts(
    {
      Escape: (event) => {
        event.preventDefault()
        onClose()
      },
    },
    { whileTyping: true, capture: true },
  )

  const openEntry = (entry: ThreadUnreadEntry) => {
    location.route(
      `/${entry.accountId}/rooms/${encodeURIComponent(entry.roomId)}?thread=${encodeURIComponent(entry.rootEventId)}`,
    )
    onClose()
  }

  return (
    <div
      ref={containerRef}
      class="overlay unread-threads-overlay"
      role="dialog"
      aria-modal="true"
      aria-label="Unread threads"
    >
      <section class="overlay-panel unread-threads-panel">
        <div class="overlay-head">
          <h2>Unread threads</h2>
          <button type="button" class="ghost" onClick={onClose}>
            Close
          </button>
        </div>
        {entries.length === 0 ? (
          <p class="muted unread-threads-empty">No unread threads.</p>
        ) : (
          <ol class="unread-thread-list">
            {entries.map((entry) => (
              <li
                key={`${entry.accountId}/${entry.roomId}/${entry.rootEventId}`}
              >
                <button
                  type="button"
                  class="unread-thread-entry"
                  onClick={() => openEntry(entry)}
                >
                  <span class="unread-thread-room">{entry.roomTitle}</span>
                  {entry.rootPreview !== null && (
                    <span class="unread-thread-root">{entry.rootPreview}</span>
                  )}
                  <span class="unread-thread-latest">
                    {entry.latestSender !== null && (
                      <span class="event-sender">{entry.latestSender}</span>
                    )}
                    {entry.latestBody ?? 'New thread activity'}
                  </span>
                  <time dateTime={new Date(entry.latestTs).toISOString()}>
                    {new Date(entry.latestTs).toLocaleString(undefined, {
                      month: 'short',
                      day: 'numeric',
                      hour: 'numeric',
                      minute: '2-digit',
                    })}
                  </time>
                </button>
              </li>
            ))}
          </ol>
        )}
      </section>
    </div>
  )
}
