import { parseMedia, type ParsedMedia } from '../media/parse-media'
import type { TimelineEvent } from '../stores/timeline'
import { FormattedBody } from './FormattedBody'
import { MediaAttachment } from './MediaAttachment'
import { MediaImage } from './MediaImage'

/**
 * The `ParsedMedia` for a media send still uploading (ADR 0065). Built from the
 * retained `File` rather than parsed off the event, because `parseMedia`
 * deliberately rejects an image whose url is not `mxc://` (ADR 0064) — and an
 * echo has no mxc until the upload lands. Without this branch the echo would
 * fall through to `FormattedBody` and render as its bare filename.
 */
function echoMedia(
  echo: NonNullable<TimelineEvent['localEcho']>,
  media: NonNullable<NonNullable<TimelineEvent['localEcho']>['media']>,
): ParsedMedia {
  return {
    kind: media.kind,
    url: null,
    thumbnailUrl: null,
    filename: media.file.name,
    // The echo's body is `caption ?? filename` (the server's own rule), so a
    // body that is just the filename is not a caption.
    caption: echo.body === media.file.name ? null : echo.body,
    encrypted: false,
    mimetype: media.file.type === '' ? undefined : media.file.type,
    size: media.file.size,
    w: undefined,
    h: undefined,
  }
}

function contentMsgtype(content: unknown): string | null {
  const msgtype = (content as { msgtype?: unknown } | null | undefined)?.msgtype
  return typeof msgtype === 'string' ? msgtype : null
}

function unsupportedEventLabel(event: TimelineEvent): string {
  const msgtype = contentMsgtype(event.content)
  if (event.type === 'm.room.message' && msgtype !== null) {
    return `unsupported message (${msgtype})`
  }
  if (msgtype !== null) {
    return `unsupported event: ${event.type} (${msgtype})`
  }
  return `unsupported event: ${event.type}`
}

function hasVisibleText(body: string | null | undefined): boolean {
  return body !== null && body !== undefined && body.trim() !== ''
}

/**
 * The app's single message-body dispatch (ADR 0046; media in ADR 0064):
 * redactions, state events, UTDs, then message kinds — image/sticker inline,
 * file/audio/video as a download card, and everything else through
 * `FormattedBody`. Used by both the main timeline (`RoomPage`) and the thread
 * panel, so media renders identically in both.
 */
export function EventBody({
  event,
  senderDisplay,
}: {
  event: TimelineEvent
  /** Resolved sender name for the `m.emote` prefix; falls back to the raw
   *  Matrix id when the caller has no members store (WCR-19). */
  senderDisplay?: string
}) {
  const echo = event.localEcho
  if (echo?.media !== undefined) {
    const media = echoMedia(echo, echo.media)
    return media.kind === 'image' ? (
      <MediaImage
        accountId={event.account_id}
        media={media}
        previewUrl={echo.media.previewUrl}
      />
    ) : (
      <MediaAttachment accountId={event.account_id} media={media} />
    )
  }
  if (event.redacted) {
    return <span class="muted placeholder">message deleted</span>
  }
  const isState = event.state_key !== null && event.state_key !== undefined
  if (isState) {
    return (
      <span class="muted">
        {event.type}
        {event.state_key !== '' && ` (${event.state_key})`}
        {event.body !== null && event.body !== undefined && `: ${event.body}`}
      </span>
    )
  }
  if (event.content === null || event.content === undefined) {
    // The decrypted content never reached the store — a UTD (ADR 0015).
    return <span class="muted placeholder">unable to decrypt</span>
  }

  const media = parseMedia(event)
  if (media !== null) {
    return media.kind === 'image' || media.kind === 'sticker' ? (
      <MediaImage accountId={event.account_id} media={media} />
    ) : (
      <MediaAttachment accountId={event.account_id} media={media} />
    )
  }

  const msgtype = contentMsgtype(event.content)
  if (msgtype === 'm.emote') {
    return (
      <em>
        * {senderDisplay ?? event.sender}{' '}
        <FormattedBody
          accountId={event.account_id}
          body={event.body}
          content={event.content}
        />
      </em>
    )
  }
  const body = (
    <FormattedBody
      accountId={event.account_id}
      body={event.body}
      content={event.content}
    />
  )
  if (!hasVisibleText(event.body)) {
    return <span class="muted placeholder">{unsupportedEventLabel(event)}</span>
  }
  return msgtype === 'm.notice' ? <span class="muted">{body}</span> : body
}
