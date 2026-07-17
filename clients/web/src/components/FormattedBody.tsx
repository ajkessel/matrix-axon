import type { ComponentChildren } from 'preact'
import { useLayoutEffect, useMemo, useRef } from 'preact/hooks'
import { matchUrls } from '../html/linkify'
import { renderMatrixHtml, SPOILER_CLASS } from '../html/sanitize'
import type { MediaHandle } from '../media/media-service'
import { useServices } from '../services'

/**
 * Linkify bare URLs in plain text, as VNodes — never via innerHTML, so the
 * text needs no escaping. Formatted bodies get the same treatment inside
 * the sanitizer (`linkifyDomTree`).
 */
function linkify(text: string): ComponentChildren {
  const matches = matchUrls(text)
  if (matches.length === 0) {
    return text
  }
  const parts: ComponentChildren[] = []
  let last = 0
  for (const { url, start, end } of matches) {
    if (start > last) {
      parts.push(text.slice(last, start))
    }
    parts.push(
      <a href={url} target="_blank" rel="noopener noreferrer">
        {url}
      </a>,
    )
    last = end
  }
  if (last < text.length) {
    parts.push(text.slice(last))
  }
  return parts
}

/**
 * A message body: sanitized Matrix HTML when the event carries
 * `format: "org.matrix.custom.html"`, plain text (with bare URLs linkified)
 * otherwise. Spoilers render hidden and reveal on click / Enter (ADR 0046,
 * M-W5).
 */
export function FormattedBody({
  accountId,
  body,
  content,
}: {
  accountId: string
  body: string | null | undefined
  content: unknown
}) {
  const { media } = useServices()
  const rootRef = useRef<HTMLSpanElement>(null)
  const html = useMemo(() => {
    const c = content as
      { format?: unknown; formatted_body?: unknown } | null | undefined
    if (
      c?.format === 'org.matrix.custom.html' &&
      typeof c.formatted_body === 'string'
    ) {
      return renderMatrixHtml(c.formatted_body)
    }
    return null
  }, [content])

  // Resolve inline `<img data-mxc>` (the sanitizer moved a safe `mxc://` src
  // here) to authenticated blob URLs after mount. This is imperative DOM
  // because the subtree came from `dangerouslySetInnerHTML`; each handle is
  // released on cleanup so the blob can be revoked. A failed acquire leaves the
  // `<img>` srcless, so its `alt` renders.
  useLayoutEffect(() => {
    const root = rootRef.current
    if (root === null || html === null) {
      return
    }
    const handles: MediaHandle[] = []
    let cancelled = false
    for (const img of root.querySelectorAll<HTMLImageElement>(
      'img[data-mxc]',
    )) {
      const mxc = img.getAttribute('data-mxc')
      if (mxc === null) {
        continue
      }
      void media.acquire(accountId, mxc).then((handle) => {
        if (cancelled) {
          handle.release()
          return
        }
        handles.push(handle)
        if (handle.result.ok) {
          img.src = handle.result.url
        }
      })
    }
    return () => {
      cancelled = true
      for (const handle of handles) {
        handle.release()
      }
    }
  }, [media, accountId, html])

  if (html === null) {
    return <span class="body-text">{linkify(body ?? '')}</span>
  }

  const toggleSpoiler = (target: EventTarget | null) => {
    const spoiler = (target as Element | null)?.closest?.(`.${SPOILER_CLASS}`)
    if (spoiler !== null && spoiler !== undefined) {
      spoiler.classList.toggle('spoiler-revealed')
    }
  }

  return (
    <span
      class="body-html"
      ref={rootRef}
      // Sanitized by renderMatrixHtml (DOMPurify + Matrix allowlist).
      dangerouslySetInnerHTML={{ __html: html }}
      onClick={(event) => toggleSpoiler(event.target)}
      onKeyDown={(event) => {
        if (event.key === 'Enter' || event.key === ' ') {
          toggleSpoiler(event.target)
        }
      }}
    />
  )
}
