/** The longest side a staged preview is downscaled to, in device pixels. */
const PREVIEW_MAX = 512

/**
 * A small preview for a staged image, as an object url.
 *
 * The composer shows staged files at thumbnail size, but the file itself is
 * the original — a phone camera photo is thousands of pixels wide. CSS bounds
 * what is *drawn*; the browser still decodes the full image into memory, and
 * ten staged photos is real pressure on a phone.
 *
 * Only the preview is downscaled. The upload still sends the original bytes:
 * this is a display concern, not a compression feature. (Generating a real
 * upload thumbnail is issue #384, and needs a server field to carry it.)
 *
 * Returns `null` when the platform cannot do it — jsdom has no canvas, and a
 * decode can fail on a corrupt file — so the caller falls back to the
 * original object url rather than showing nothing.
 */
export async function downscalePreview(file: File): Promise<string | null> {
  if (
    typeof createImageBitmap !== 'function' ||
    typeof document.createElement('canvas').getContext !== 'function'
  ) {
    return null
  }
  let bitmap: ImageBitmap
  try {
    bitmap = await createImageBitmap(file)
  } catch {
    return null
  }
  try {
    const scale = Math.min(
      1,
      PREVIEW_MAX / Math.max(bitmap.width, bitmap.height),
    )
    // Already small enough: decoding it again would cost more than it saves.
    if (scale === 1) {
      return null
    }
    const canvas = document.createElement('canvas')
    canvas.width = Math.max(1, Math.round(bitmap.width * scale))
    canvas.height = Math.max(1, Math.round(bitmap.height * scale))
    const context = canvas.getContext('2d')
    if (context === null) {
      return null
    }
    context.drawImage(bitmap, 0, 0, canvas.width, canvas.height)
    const blob = await new Promise<Blob | null>((resolve) => {
      canvas.toBlob(resolve, 'image/jpeg', 0.8)
    })
    return blob === null ? null : URL.createObjectURL(blob)
  } catch {
    return null
  } finally {
    bitmap.close()
  }
}
