import type { ComponentChildren } from 'preact'
import { createPortal } from 'preact/compat'
import { useLayoutEffect, useMemo, useState } from 'preact/hooks'

/**
 * Render modal/popover DOM under `document.body` while preserving the caller's
 * Preact context. This keeps fixed-position surfaces out of timeline row
 * containment such as `content-visibility`.
 */
export function BodyPortal({ children }: { children: ComponentChildren }) {
  const host = useMemo(() => document.createElement('div'), [])
  const [mounted, setMounted] = useState(false)

  useLayoutEffect(() => {
    document.body.append(host)
    setMounted(true)
    return () => {
      setMounted(false)
      host.remove()
    }
  }, [host])

  return mounted ? createPortal(children, host) : null
}
