import { useMediaBlob } from '../media/use-media-blob'
import { useShortcuts } from '../shortcuts'
import { BodyPortal } from './BodyPortal'
import { useModalFocus } from './use-modal-focus'

/**
 * A full-viewport view of one image (ADR 0064). Reuses the app's `.overlay`
 * modal shell, loads the full-size object URL eagerly (never a thumbnail), and
 * follows the shared modal contract (`useModalFocus` + capture-phase Escape).
 */
export function Lightbox({
  accountId,
  mxcUrl,
  alt,
  caption,
  onClose,
}: {
  accountId: string
  mxcUrl: string
  alt: string
  caption: string | null
  onClose: () => void
}) {
  const { state } = useMediaBlob(accountId, mxcUrl, { eager: true })
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
        aria-label={alt}
        onClick={(event) => {
          // A click on the backdrop (not the image) dismisses.
          if (event.target === event.currentTarget) {
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
          <div tabindex={0} class="lightbox-image">
            {state.status === 'ready' && state.url !== undefined ? (
              <img src={state.url} alt={alt} />
            ) : state.status === 'error' ? (
              <p class="muted placeholder">Could not load image</p>
            ) : (
              <div class="media-skeleton" aria-hidden="true" />
            )}
          </div>
          {caption !== null && (
            <figcaption class="lightbox-caption">{caption}</figcaption>
          )}
        </figure>
      </div>
    </BodyPortal>
  )
}
