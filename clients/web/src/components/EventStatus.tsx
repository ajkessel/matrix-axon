import type { TimeFormat } from '../stores/settings'
import type { TimelineEvent, TimelineStore } from '../stores/timeline'

/**
 * The per-row send-state fragments shared by the main timeline and the
 * thread panel (WCR-16) — they were copy-pasted and had already started
 * life as two identical blocks.
 */

/**
 * The row's timestamp. Hand-rolled rather than `toLocaleTimeString` — the
 * locale path showed up in Safari profiles on older iPhones — with the
 * format a setting (Settings → Timeline) since hardcoding 12-hour took
 * locale-appropriate 24-hour rendering away from those locales.
 */
export function formatEventTime(
  originTs: number,
  format: TimeFormat = '12h',
): string {
  const date = new Date(originTs)
  const hours = date.getHours()
  const minutes = date.getMinutes().toString().padStart(2, '0')
  if (format === '24h') {
    return `${hours.toString().padStart(2, '0')}:${minutes}`
  }
  const hour12 = hours % 12 || 12
  const period = hours >= 12 ? 'pm' : 'am'
  return `${hour12}:${minutes}${period}`
}

export function EventTime({
  event,
  format,
}: {
  event: TimelineEvent
  format?: TimeFormat
}) {
  if (event.localEcho?.status === 'pending') {
    return <span class="muted local-echo-status">Sending…</span>
  }
  return (
    <time class="muted" dateTime={new Date(event.origin_ts).toISOString()}>
      {formatEventTime(event.origin_ts, format)}
    </time>
  )
}

/** The failed-send notice with its Retry/Discard controls. */
export function FailedSend({
  event,
  timeline,
}: {
  event: TimelineEvent
  timeline: TimelineStore
}) {
  if (event.localEcho?.status !== 'failed') {
    return null
  }
  return (
    <span class="local-echo-status error">
      Failed to send
      <button
        type="button"
        class="ghost"
        onClick={() => void timeline.retrySend(event.event_id)}
      >
        Retry
      </button>
      <button
        type="button"
        class="ghost"
        onClick={() => timeline.discardSend(event.event_id)}
      >
        Discard
      </button>
    </span>
  )
}
