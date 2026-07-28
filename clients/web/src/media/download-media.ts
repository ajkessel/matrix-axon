import type { MediaService } from './media-service'
import type { ParsedMedia } from './parse-media'

/**
 * How long the object URL is held after the anchor is clicked. Revoking
 * immediately races the browser's own read of the blob, and the download then
 * silently produces an empty file.
 */
const REVOKE_DELAY_MS = 60_000

export type DownloadOutcome = 'saved' | 'shared' | 'cancelled' | 'failed'

/**
 * Whether this media can be saved at all: only an `mxc://` object has bytes
 * the proxy can hand back.
 */
export function isDownloadable(media: ParsedMedia): boolean {
  return media.url !== null && media.url.startsWith('mxc://')
}

/**
 * Offer the file to the platform share sheet. Returns `null` when there is no
 * sheet that takes files, so the caller falls back to the anchor.
 */
async function shareFile(
  blob: Blob,
  media: ParsedMedia,
): Promise<DownloadOutcome | null> {
  const share = navigator.share?.bind(navigator)
  const canShare = navigator.canShare?.bind(navigator)
  if (share === undefined || canShare === undefined) {
    return null
  }
  const file = new File([blob], media.filename, {
    type: media.mimetype ?? blob.type ?? 'application/octet-stream',
  })
  if (!canShare({ files: [file] })) {
    return null
  }
  try {
    await share({ files: [file] })
    return 'shared'
  } catch (error) {
    // Dismissing the sheet throws `AbortError`. That is a deliberate cancel,
    // not a failure — surfacing it as one would show an error for a user who
    // simply changed their mind. Anything else means the sheet could not take
    // the file, so fall back to the anchor rather than leave them with
    // nothing.
    if (error instanceof DOMException && error.name === 'AbortError') {
      return 'cancelled'
    }
    return null
  }
}

/**
 * Save a piece of media to the device.
 *
 * The transient anchor is the sanctioned path: `window.open` is banned
 * repo-wide for the Tauri shell (M-W12), so there is no "open it in a tab and
 * let the user save it from there" fallback to lean on.
 *
 * On a phone that anchor lands the file in Files rather than Photos, which
 * reads as the save having failed — so where the platform can share files, the
 * sheet is offered first and gives "Save Image" and AirDrop. Detected by
 * capability, never by user agent, and the anchor stays the path that must
 * keep working.
 *
 * Deliberately re-fetches instead of reusing an object URL the caller may
 * already be displaying: those are owned by `useMediaBlob`'s reference
 * counting and can be evicted by the LRU while the browser is still writing
 * the file. The URL acquired here is owned here, and released on our own
 * schedule.
 */
export async function downloadMedia(
  service: MediaService,
  accountId: string,
  media: ParsedMedia,
): Promise<DownloadOutcome> {
  if (media.url === null) {
    return 'failed'
  }

  const blob = await service.fetchBlob(accountId, media.url)
  if (!blob.ok) {
    return 'failed'
  }
  const shared = await shareFile(blob.blob, media)
  if (shared !== null) {
    return shared
  }

  const url = URL.createObjectURL(blob.blob)
  const anchor = document.createElement('a')
  anchor.href = url
  anchor.download = media.filename
  document.body.appendChild(anchor)
  anchor.click()
  anchor.remove()
  setTimeout(() => URL.revokeObjectURL(url), REVOKE_DELAY_MS)
  return 'saved'
}
