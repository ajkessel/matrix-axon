import type { ComponentChildren } from 'preact'
import { useLayoutEffect, useMemo, useRef } from 'preact/hooks'
import { matchUrls } from '../html/linkify'
import { resolveMatrixToRoomLink, resolveMatrixToUserLink } from '../matrix-to'
import { renderMatrixHtml, SPOILER_CLASS } from '../html/sanitize'
import type { MediaHandle } from '../media/media-service'
import { useServices } from '../services'
import type { RoomDto } from '../stores/room-list'

/**
 * Linkify bare URLs in plain text, as VNodes — never via innerHTML, so the
 * text needs no escaping. Formatted bodies get the same treatment inside
 * the sanitizer (`linkifyDomTree`).
 */
function linkify(
  text: string,
  context: {
    accountId: string
    rooms: readonly RoomDto[]
    roomTitles: ReadonlyMap<string, string>
  },
): ComponentChildren {
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
    const userLink = resolveMatrixToUserLink(url)
    const roomLink =
      userLink === null ? resolveMatrixToRoomLink(url, context) : null
    parts.push(
      userLink !== null ? (
        <a class="mention-pill" href={userLink.href}>
          {userLink.label}
        </a>
      ) : roomLink !== null ? (
        <a
          class={`room-pill${roomLink.isEventLink ? ' event-pill' : ''}`}
          href={roomLink.href}
          title={roomLink.isEventLink ? 'Jump to message' : 'Jump to room'}
        >
          {roomLink.label}
        </a>
      ) : (
        <a href={url} target="_blank" rel="noopener noreferrer">
          {url}
        </a>
      ),
    )
    last = end
  }
  if (last < text.length) {
    parts.push(text.slice(last))
  }
  return parts
}

function routeLocalRoomPillClick(event: MouseEvent): boolean {
  if (
    event.defaultPrevented ||
    event.button !== 0 ||
    event.ctrlKey ||
    event.metaKey ||
    event.altKey ||
    event.shiftKey
  ) {
    return false
  }
  const anchor = (event.target as Element | null)?.closest?.('a.room-pill')
  if (!(anchor instanceof HTMLAnchorElement)) {
    return false
  }
  if (
    anchor.target !== '' ||
    anchor.download !== '' ||
    anchor.origin !== window.location.origin
  ) {
    return false
  }
  event.preventDefault()
  const next = `${anchor.pathname}${anchor.search}${anchor.hash}`
  window.history.pushState(null, '', next)
  window.dispatchEvent(new PopStateEvent('popstate'))
  return true
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
  const { media, rooms } = useServices()
  const rootRef = useRef<HTMLSpanElement>(null)
  const roomList = rooms.rooms.value
  const roomTitles = rooms.titles.value
  const html = useMemo(() => {
    const roomLinkContext = {
      accountId,
      rooms: roomList,
      roomTitles,
    }
    const c = content as
      { format?: unknown; formatted_body?: unknown } | null | undefined
    if (
      c?.format === 'org.matrix.custom.html' &&
      typeof c.formatted_body === 'string'
    ) {
      return renderMatrixHtml(c.formatted_body, {
        resolveUserLink: resolveMatrixToUserLink,
        resolveRoomLink: (href, label) =>
          resolveMatrixToRoomLink(href, roomLinkContext, label),
      })
    }
    return null
  }, [content, accountId, roomList, roomTitles])

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
    const roomLinkContext = {
      accountId,
      rooms: roomList,
      roomTitles,
    }
    return (
      <span
        class="body-text"
        onClick={(event) => routeLocalRoomPillClick(event)}
      >
        {linkify(body ?? '', roomLinkContext)}
      </span>
    )
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
      onClick={(event) => {
        if (!routeLocalRoomPillClick(event)) {
          toggleSpoiler(event.target)
        }
      }}
      onKeyDown={(event) => {
        if (event.key === 'Enter' || event.key === ' ') {
          toggleSpoiler(event.target)
        }
      }}
    />
  )
}
