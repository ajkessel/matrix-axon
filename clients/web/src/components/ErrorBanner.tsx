import type { Signal } from '@preact/signals'

/**
 * The dismissable store-error banner (WCR-16) — one implementation for the
 * four surfaces that each hand-rolled it (room timeline, thread panel, room
 * list, accounts page). Renders nothing while the signal is null; Dismiss
 * writes the null back.
 */
export function ErrorBanner({ error }: { error: Signal<string | null> }) {
  if (error.value === null) {
    return null
  }
  return (
    <div class="banner error" role="alert">
      <span>{error.value}</span>
      <button type="button" class="ghost" onClick={() => (error.value = null)}>
        Dismiss
      </button>
    </div>
  )
}
