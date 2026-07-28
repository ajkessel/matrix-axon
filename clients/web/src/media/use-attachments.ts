import { useCallback, useEffect, useRef, useState } from 'preact/hooks'
import { MAX_UPLOAD_BYTES, uploadKind } from './media-service'
import { downscalePreview } from './downscale-preview'

/** One file staged for sending, with its local preview (images only). */
export interface StagedAttachment {
  /**
   * A stable identity for keying and removal. A `File` is not usable as a
   * key — two identical pastes are equal enough to collide, and Preact would
   * pair the wrong chip with the wrong file (WCR-01).
   */
  id: string
  file: File
  previewUrl: string | null
}

export interface AttachmentBatch {
  items: readonly StagedAttachment[]
  /** Files the last `stage` refused, and why — surfaced by the composer. */
  skipped: number
  skippedReason: 'count' | 'size' | null
  totalBytes: number
}

/** Beyond this many, the strip stops being scannable and the wait is long. */
export const MAX_BATCH_FILES = 10

/**
 * The composer's staged attachments (ADR 0065; multi-image in ADR 0081).
 *
 * `scope` is the room or thread the files belong to. Neither `RoomPage` nor
 * `ThreadPanel` remounts when the route's room changes, so without clearing on
 * a scope change a file staged in room A would still be staged — and would
 * send — in room B.
 *
 * Staging is **additive**: pick three, paste a fourth, remove one. Every
 * preview object url is owned here and revoked on remove, clear, scope change,
 * and unmount. That bookkeeping is the most bug-prone part of this hook —
 * a leaked object url is invisible until someone reads a memory profile —
 * which is why the urls live in a ref keyed by id rather than being derived
 * from state.
 */
export function useAttachments(scope: string): {
  batch: AttachmentBatch
  stage(files: FileList | readonly File[]): void
  remove(id: string): void
  clear(): void
} {
  const [items, setItems] = useState<readonly StagedAttachment[]>([])
  const [skipped, setSkipped] = useState(0)
  const [skippedReason, setSkippedReason] = useState<'count' | 'size' | null>(
    null,
  )
  const urls = useRef(new Map<string, string>())

  const revoke = useCallback((id: string) => {
    const url = urls.current.get(id)
    if (url !== undefined) {
      URL.revokeObjectURL(url)
      urls.current.delete(id)
    }
  }, [])

  const clear = useCallback(() => {
    for (const url of urls.current.values()) {
      URL.revokeObjectURL(url)
    }
    urls.current.clear()
    setItems([])
    setSkipped(0)
    setSkippedReason(null)
  }, [])

  const remove = useCallback(
    (id: string) => {
      revoke(id)
      setItems((current) => current.filter((item) => item.id !== id))
    },
    [revoke],
  )

  /**
   * Swap an item's preview for a downscaled one, and retire the original.
   *
   * Staging shows the full-size object url immediately — ten files should
   * appear at once, not after ten decodes — and each is replaced as its
   * smaller version is ready. The original is revoked on the way, so the
   * full-size decode is transient rather than held for as long as the file
   * is staged.
   */
  const shrink = useCallback(async (id: string, file: File) => {
    const smaller = await downscalePreview(file)
    if (smaller === null) {
      return
    }
    const original = urls.current.get(id)
    if (original === undefined) {
      // Removed or cleared while we were decoding — do not resurrect it.
      URL.revokeObjectURL(smaller)
      return
    }
    urls.current.set(id, smaller)
    URL.revokeObjectURL(original)
    setItems((current) =>
      current.map((item) =>
        item.id === id ? { ...item, previewUrl: smaller } : item,
      ),
    )
  }, [])

  const stage = useCallback(
    (files: FileList | readonly File[]) => {
      const incoming = [...files]
      if (incoming.length === 0) {
        return
      }
      setItems((current) => {
        const accepted: StagedAttachment[] = []
        let refused = 0
        let reason: 'count' | 'size' | null = null
        let bytes = current.reduce((sum, item) => sum + item.file.size, 0)

        for (const file of incoming) {
          if (current.length + accepted.length >= MAX_BATCH_FILES) {
            refused += 1
            reason = 'count'
            continue
          }
          // Against the *accumulated* total, not per file: ten files under the
          // cap individually can be far over it together.
          if (bytes + file.size > MAX_UPLOAD_BYTES) {
            refused += 1
            reason ??= 'size'
            continue
          }
          bytes += file.size
          const id = crypto.randomUUID()
          const url =
            uploadKind(file) === 'image' ? URL.createObjectURL(file) : null
          if (url !== null) {
            urls.current.set(id, url)
          }
          accepted.push({ id, file, previewUrl: url })
        }

        setSkipped(refused)
        setSkippedReason(reason)
        for (const item of accepted) {
          if (item.previewUrl !== null) {
            void shrink(item.id, item.file)
          }
        }
        return accepted.length === 0 ? current : [...current, ...accepted]
      })
    },
    [shrink],
  )

  // Revoke everything on unmount and whenever the room changes.
  useEffect(() => clear, [clear, scope])

  return {
    batch: {
      items,
      skipped,
      skippedReason,
      totalBytes: items.reduce((sum, item) => sum + item.file.size, 0),
    },
    stage,
    remove,
    clear,
  }
}
