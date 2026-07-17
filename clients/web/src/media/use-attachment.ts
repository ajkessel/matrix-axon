import { useCallback, useEffect, useRef, useState } from 'preact/hooks'
import { uploadKind } from './media-service'

/** A file staged for sending, with its local preview (images only). */
export interface StagedAttachment {
  file: File
  previewUrl: string | null
  /** Files a multi-file drop left behind — only one is sent (ADR 0065). */
  skipped: number
}

/**
 * The composer's staged attachment (ADR 0065): a file picked, dropped, or
 * pasted, held until the user submits — that pause is what lets the composer
 * text become the caption.
 *
 * `scope` is the room (or thread) the attachment belongs to. Neither `RoomPage`
 * nor `ThreadPanel` remounts when the route's room changes, so without clearing
 * on a scope change a file staged in room A would still be staged — and would
 * send — in room B. This is the same hazard the composer's `key` guards for the
 * draft text.
 *
 * The preview object url is owned here: staging over an existing attachment,
 * clearing, and unmounting all revoke it. Once `send` is called the *store*
 * owns the echo's url, which it creates fresh — this hook's url never escapes.
 */
export function useAttachment(scope: string): {
  attachment: StagedAttachment | null
  stage(file: File, skipped?: number): void
  clear(): void
} {
  const [attachment, setAttachment] = useState<StagedAttachment | null>(null)
  // Mirrors the staged url for cleanup paths that must not re-run on every
  // change of it (unmount, scope change).
  const previewUrl = useRef<string | null>(null)

  const revoke = useCallback(() => {
    if (previewUrl.current !== null) {
      URL.revokeObjectURL(previewUrl.current)
      previewUrl.current = null
    }
  }, [])

  const clear = useCallback(() => {
    revoke()
    setAttachment(null)
  }, [revoke])

  const stage = useCallback(
    (file: File, skipped = 0) => {
      revoke()
      const url =
        uploadKind(file) === 'image' ? URL.createObjectURL(file) : null
      previewUrl.current = url
      setAttachment({ file, previewUrl: url, skipped })
    },
    [revoke],
  )

  useEffect(() => clear, [clear, scope])

  return { attachment, stage, clear }
}
