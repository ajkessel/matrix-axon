import { useEffect, useState } from 'preact/hooks'
import type { ParsedMedia } from '../media/parse-media'
import { useMediaBlob } from '../media/use-media-blob'
import { Lightbox, LightboxImage } from './Lightbox'

/** The longest side an inline thumbnail is allowed, in CSS px (Element-ish). */
const THUMBNAIL_MAX = 320

/**
 * The displayed width for an image of intrinsic `w`×`h`, scaled down so its
 * longest side is at most `THUMBNAIL_MAX`. Never upscales, so a small image
 * keeps its natural size.
 */
function thumbnailWidth(w: number, h: number): number {
  const scale = Math.min(1, THUMBNAIL_MAX / w, THUMBNAIL_MAX / h)
  return Math.round(w * scale)
}

/**
 * An inline image or sticker (ADR 0064). Shows the sender-embedded thumbnail
 * when present, else a homeserver-generated thumbnail for plaintext media,
 * else the full-size image; a click opens the full-size `Lightbox`. The
 * wrapper reserves the image's aspect ratio *before* the blob resolves — the
 * timeline is not re-anchored after mount, so an image that grew on load would
 * shove scrolled-back content around.
 *
 * The media proxy returns raw ciphertext with a 200 when it lacks the
 * decryption key, so the fetch succeeds but the bytes are not an image; that
 * surfaces only at `<img>` decode, caught by `onError` (matching the TUI).
 */
export function MediaImage({
  accountId,
  media,
  previewUrl,
}: {
  accountId: string
  media: ParsedMedia
  /**
   * A local object url for an image still uploading (ADR 0065) — rendered
   * directly, since there is no mxc to fetch yet. Its presence is what makes
   * this component the *only* one that needs to know a send can be in flight.
   */
  previewUrl?: string | null
}) {
  const [thumbnailFailed, setThumbnailFailed] = useState(false)
  const generatedThumbnailUrl =
    media.thumbnailUrl === null && !media.encrypted ? media.url : null
  const thumbnailUrl = media.thumbnailUrl ?? generatedThumbnailUrl
  const usingThumbnail = !thumbnailFailed && thumbnailUrl !== null
  const displayUrl = usingThumbnail ? thumbnailUrl : media.url
  const thumbnailRequest =
    usingThumbnail && media.thumbnailUrl === null
      ? {
          width: THUMBNAIL_MAX,
          height: THUMBNAIL_MAX,
          method: 'scale' as const,
        }
      : undefined
  // A null url makes the hook a no-op, so a local preview skips the proxy fetch
  // entirely while keeping the hook call unconditional.
  const { ref, state } = useMediaBlob<HTMLDivElement>(
    accountId,
    previewUrl === undefined || previewUrl === null ? displayUrl : null,
    { thumbnail: thumbnailRequest },
  )
  const [lightboxOpen, setLightboxOpen] = useState(false)
  const [decodeFailed, setDecodeFailed] = useState(false)

  // A broken thumbnail (malformed/purged sender thumbnail, or a homeserver
  // that cannot generate one) must not hide an image whose full-size object
  // would load fine — fall back to it instead of the error placeholder.
  useEffect(() => {
    if (
      state.status === 'error' &&
      !thumbnailFailed &&
      usingThumbnail &&
      media.url !== null
    ) {
      setThumbnailFailed(true)
    }
  }, [state.status, thumbnailFailed, usingThumbnail, media.url])

  const hasDimensions = media.w !== undefined && media.h !== undefined
  // Cap the inline thumbnail to a modest box (never upscaling), so a large
  // photo renders small; `max-width: 100%` still lets it shrink on a narrow
  // pane while `aspect-ratio` keeps its shape and reserves scroll space.
  const boxStyle = hasDimensions
    ? {
        aspectRatio: `${media.w} / ${media.h}`,
        width: `${thumbnailWidth(media.w!, media.h!)}px`,
        maxWidth: '100%',
      }
    : {
        width: `${THUMBNAIL_MAX}px`,
        maxWidth: '100%',
        maxHeight: `${THUMBNAIL_MAX}px`,
      }

  const alt = media.caption ?? media.filename
  const canOpen =
    state.status === 'ready' && !decodeFailed && media.url !== null

  const figure = (
    <figure class="media-figure">
      <div ref={ref} class="media-image" style={boxStyle}>
        <div
          class={`media-thumbnail${hasDimensions ? '' : ' media-thumbnail-unsized'}`}
        >
          {previewUrl !== undefined && previewUrl !== null ? (
            // Still uploading: the local file, not the proxy. No open/lightbox —
            // there is nothing on the server to open yet.
            <img class="media-preview" src={previewUrl} alt={alt} />
          ) : decodeFailed ? (
            <p class="muted placeholder">
              Encrypted media — server could not decrypt
            </p>
          ) : state.status === 'error' ? (
            <p class="muted placeholder">Could not load image</p>
          ) : state.status === 'ready' && state.url !== undefined ? (
            <button
              type="button"
              class="media-open"
              aria-label={`Open ${media.kind}: ${media.filename}`}
              disabled={!canOpen}
              onClick={() => setLightboxOpen(true)}
            >
              <img
                src={state.url}
                alt={alt}
                onError={() => setDecodeFailed(true)}
              />
            </button>
          ) : (
            <div class="media-skeleton" aria-hidden="true" />
          )}
        </div>
      </div>
      {media.caption !== null && (
        <figcaption class="media-caption">{media.caption}</figcaption>
      )}
    </figure>
  )

  return (
    <>
      {figure}
      {lightboxOpen && media.url !== null && (
        <Lightbox
          label={alt}
          caption={media.caption}
          onClose={() => setLightboxOpen(false)}
        >
          <LightboxImage accountId={accountId} mxcUrl={media.url} alt={alt} />
        </Lightbox>
      )}
    </>
  )
}
