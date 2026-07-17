import type { EventDto } from '../stores/timeline'

/**
 * A media event reduced to what the renderer needs. A direct port of the TUI's
 * `parsed_media()` / `image_media()` (`clients/tui/src/api.rs:1157-1195`) — the
 * version that has seen production events — extended with the `info` fields the
 * web client uses for thumbnails and aspect-ratio reservation. Keep the two in
 * step: cross-client divergence in media parsing is a recurring bug source.
 */
export interface ParsedMedia {
  /**
   * `sticker` comes from the event *type* (`m.sticker`), everything else from
   * `content.msgtype` — an easy thing to get backwards.
   */
  kind: 'image' | 'sticker' | 'file' | 'audio' | 'video'
  /** `content.url` (plain) or `content.file.url` (encrypted). */
  url: string | null
  /** Sender-embedded thumbnail, if any — never a server-generated one. */
  thumbnailUrl: string | null
  /** `filename`, else `body`, else the literal `media`. */
  filename: string
  /** `body`, but only when `filename` is explicit and the two differ. */
  caption: string | null
  /** Encrypted attachment: `content.url` absent but `content.file.url` present. */
  encrypted: boolean
  mimetype?: string
  size?: number
  w?: number
  h?: number
}

function str(value: unknown): string | undefined {
  return typeof value === 'string' ? value : undefined
}

function num(value: unknown): number | undefined {
  return typeof value === 'number' ? value : undefined
}

/**
 * Extract a media descriptor from an event, or `null` when it carries no
 * media. Image and sticker events additionally require the resolved url to be
 * an `mxc://` URI — the only thing the media proxy can download; a stray
 * `http(s)` src is not one we ever fetch.
 */
export function parseMedia(event: EventDto): ParsedMedia | null {
  const content = event.content as Record<string, unknown> | null | undefined
  if (content === null || content === undefined) {
    return null
  }

  const msgtype = str(content.msgtype)
  let kind: ParsedMedia['kind']
  switch (msgtype) {
    case 'm.image':
      kind = 'image'
      break
    case 'm.file':
      kind = 'file'
      break
    case 'm.audio':
      kind = 'audio'
      break
    case 'm.video':
      kind = 'video'
      break
    default:
      if (event.type === 'm.sticker') {
        kind = 'sticker'
      } else {
        return null
      }
  }

  const explicitFilename = str(content.filename)
  const body = str(content.body)
  const filename = explicitFilename ?? body ?? 'media'
  // A caption only exists when the sender set a distinct filename *and* body;
  // otherwise `body` is just the filename echoed back.
  const caption =
    explicitFilename !== undefined && body !== undefined && body !== filename
      ? body
      : null

  const plainUrl = str(content.url)
  const file = content.file as Record<string, unknown> | undefined
  const encryptedUrl = str(file?.url)
  const url = plainUrl ?? encryptedUrl ?? null

  if (
    (kind === 'image' || kind === 'sticker') &&
    !(url !== null && url.startsWith('mxc://'))
  ) {
    return null
  }

  const info = content.info as Record<string, unknown> | undefined
  const thumbnailFile = info?.thumbnail_file as
    Record<string, unknown> | undefined
  const thumbnailUrl =
    str(info?.thumbnail_url) ?? str(thumbnailFile?.url) ?? null

  return {
    kind,
    url,
    thumbnailUrl,
    filename,
    caption,
    encrypted: plainUrl === undefined && encryptedUrl !== undefined,
    mimetype: str(info?.mimetype),
    size: num(info?.size),
    w: num(info?.w),
    h: num(info?.h),
  }
}

/**
 * Split an `mxc://server/media` URI into its raw `server_name` / `media_id`
 * path segments, or `null` when the URI is malformed. The caller percent-encodes
 * each segment when building the proxy URL, so an exotic media id cannot escape
 * its slot.
 */
export function mxcToPath(
  mxcUrl: string,
): { serverName: string; mediaId: string } | null {
  if (!mxcUrl.startsWith('mxc://')) {
    return null
  }
  const rest = mxcUrl.slice('mxc://'.length)
  const slash = rest.indexOf('/')
  if (slash <= 0 || slash === rest.length - 1) {
    return null
  }
  return {
    serverName: rest.slice(0, slash),
    mediaId: rest.slice(slash + 1),
  }
}
