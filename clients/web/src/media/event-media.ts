import { parseMedia, type ParsedMedia } from './parse-media'
import type { TimelineEvent } from '../stores/timeline'

/**
 * An event's media, resolved the same way for every surface that renders one.
 *
 * Before this existed, "is this event media?" was answered twice: `EventBody`
 * knew that a still-uploading send needs a hand-built `ParsedMedia`, and
 * everything else called `parseMedia` and silently missed echoes. Anything new
 * that walks the timeline looking for images — galleries, a lightbox sequence
 * (ADR 0081) — needs the same answer, so it lives in one place.
 */
export interface EventMedia {
  media: ParsedMedia
  /**
   * Local object url for a send still uploading; `null` for anything the
   * server has confirmed, which is fetched through the media proxy instead.
   */
  previewUrl: string | null
  /** Whether this is a local echo with no `mxc://` url yet. */
  pending: boolean
}

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

/**
 * Resolve an event's media, or `null` when it carries none. A still-uploading
 * send resolves from its retained `File`; everything else goes through
 * `parseMedia`.
 *
 * Note this answers only "what media is here" — it deliberately does *not*
 * consider redaction or decryption failure, which are placeholder states the
 * caller renders instead of a body. `EventBody` keeps those checks ahead of
 * the confirmed-media branch so their placeholders still win.
 */
export function eventMedia(event: TimelineEvent): EventMedia | null {
  const echo = event.localEcho
  if (echo?.media !== undefined) {
    return {
      media: echoMedia(echo, echo.media),
      previewUrl: echo.media.previewUrl,
      pending: true,
    }
  }
  const media = parseMedia(event)
  return media === null ? null : { media, previewUrl: null, pending: false }
}

/**
 * Whether this media renders as a picture rather than a download card.
 * `sticker` comes from the event *type*, `image` from `content.msgtype` —
 * see `ParsedMedia.kind`.
 */
export function isImageMedia(media: ParsedMedia): boolean {
  return media.kind === 'image' || media.kind === 'sticker'
}
