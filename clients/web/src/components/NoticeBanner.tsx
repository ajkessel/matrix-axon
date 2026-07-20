import type { Signal } from '@preact/signals'

/** A dismissable non-error notice: `success` (green) or neutral `info`. */
export interface Notice {
  tone: 'success' | 'info'
  message: string
}

/**
 * The success/info counterpart to `ErrorBanner` (WCR-16) — one dismissable
 * banner any surface can drive off a `Signal<Notice | null>`, whether that
 * signal lives in a store or is a per-component signal for locally-scoped
 * feedback (the recover form uses the latter). Renders nothing while null; a
 * `role="status"` region so it is announced without stealing focus. Dismiss
 * writes the null back, exactly like `ErrorBanner`.
 */
export function NoticeBanner({ notice }: { notice: Signal<Notice | null> }) {
  const value = notice.value
  if (value === null) {
    return null
  }
  return (
    <div class={`banner notice ${value.tone}`} role="status">
      <span>{value.message}</span>
      <button type="button" class="ghost" onClick={() => (notice.value = null)}>
        Dismiss
      </button>
    </div>
  )
}
