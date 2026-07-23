import type { ComponentChildren } from 'preact'
import { useMediaBlob } from '../media/use-media-blob'
import { useShortcuts } from '../shortcuts'
import { BodyPortal } from './BodyPortal'
import { useModalFocus } from './use-modal-focus'

/**
 * Elements a click must *not* dismiss on: the media being viewed, its caption,
 * and any control. Everything else in the overlay counts as backdrop.
 */
const DISMISS_EXEMPT = 'video, audio, img, iframe, figcaption, button, a'

/**
 * A full-viewport view of one piece of media (ADR 0064; generalised beyond
 * images in ADR 0072). Reuses the app's `.overlay` modal shell and follows the
 * shared modal contract (`useModalFocus` + capture-phase Escape), so there are
 * always three ways back to the timeline: Escape, the ✕, and a tap anywhere
 * that is not the media itself.
 *
 * The shell owns presentation only — loading is the caller's, because an image,
 * a video and a PDF want different elements and different failure text.
 */
export function Lightbox({
  label,
  caption,
  onClose,
  children,
}: {
  /** Accessible name for the dialog — the caption or filename. */
  label: string
  caption: string | null
  onClose: () => void
  children: ComponentChildren
}) {
  const { containerRef } = useModalFocus<HTMLDivElement>()

  // Topmost surface: claim Escape first via capture, like the other modals.
  useShortcuts(
    {
      Escape: (event) => {
        event.preventDefault()
        onClose()
      },
    },
    { whileTyping: true, capture: true },
  )

  return (
    <BodyPortal>
      <div
        ref={containerRef}
        class="overlay lightbox"
        role="dialog"
        aria-modal="true"
        aria-label={label}
        onClick={(event) => {
          // Anything that is not the media itself is backdrop. Testing
          // `target === currentTarget` instead would make only the literal
          // overlay dismiss, and a letterboxed video leaves most of the screen
          // covered by the figure and its wrapper — tapping the dark area
          // beside or below the player would then do nothing, which reads as
          // broken. `closest` also keeps a click on a `<video>`'s own controls
          // from closing the thing it is scrubbing.
          if (
            !(event.target instanceof Element) ||
            event.target.closest(DISMISS_EXEMPT) === null
          ) {
            onClose()
          }
        }}
      >
        <button
          type="button"
          class="ghost lightbox-close"
          aria-label="Close"
          onClick={onClose}
        >
          ✕
        </button>
        <figure class="lightbox-figure">
          {children}
          {caption !== null && (
            <figcaption class="lightbox-caption">{caption}</figcaption>
          )}
        </figure>
      </div>
    </BodyPortal>
  )
}

/**
 * The lightbox's image body: the full-size object URL, loaded eagerly and never
 * a thumbnail.
 */
export function LightboxImage({
  accountId,
  mxcUrl,
  alt,
}: {
  accountId: string
  mxcUrl: string
  alt: string
}) {
  const { state } = useMediaBlob(accountId, mxcUrl, { eager: true })

  return (
    <div tabindex={0} class="lightbox-image">
      {state.status === 'ready' && state.url !== undefined ? (
        <img src={state.url} alt={alt} />
      ) : state.status === 'error' ? (
        <p class="muted placeholder">Could not load image</p>
      ) : (
        <div class="media-skeleton" aria-hidden="true" />
      )}
    </div>
  )
}
