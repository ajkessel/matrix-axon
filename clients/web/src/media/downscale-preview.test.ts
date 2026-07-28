import { afterEach, describe, expect, it, vi } from 'vitest'
import { downscalePreview } from './downscale-preview'

afterEach(() => vi.restoreAllMocks())

const png = () => new File(['bytes'], 'cat.png', { type: 'image/png' })

/** Stand in for a decoded image of the given intrinsic size. */
function fakeBitmap(width: number, height: number) {
  return { width, height, close: vi.fn() } as unknown as ImageBitmap
}

function stubCanvas(blob: Blob | null = new Blob(['small'])) {
  const drawImage = vi.fn()
  const canvas = {
    width: 0,
    height: 0,
    getContext: () => ({ drawImage }),
    toBlob: (cb: (b: Blob | null) => void) => cb(blob),
  }
  vi.spyOn(document, 'createElement').mockReturnValue(
    canvas as unknown as HTMLElement,
  )
  return { canvas, drawImage }
}

describe('downscalePreview', () => {
  it('returns null when the platform cannot decode, so the caller falls back', async () => {
    // jsdom has no `createImageBitmap`; the composer must still show something.
    vi.stubGlobal('createImageBitmap', undefined)
    expect(await downscalePreview(png())).toBeNull()
  })

  it('returns null for an image already small enough', async () => {
    // Decoding it a second time would cost more than it saves.
    const bitmap = fakeBitmap(200, 150)
    vi.stubGlobal('createImageBitmap', vi.fn().mockResolvedValue(bitmap))
    stubCanvas()

    expect(await downscalePreview(png())).toBeNull()
    expect(bitmap.close).toHaveBeenCalled()
  })

  it('downscales a large image, preserving its aspect ratio', async () => {
    const bitmap = fakeBitmap(2400, 1800)
    vi.stubGlobal('createImageBitmap', vi.fn().mockResolvedValue(bitmap))
    const { canvas, drawImage } = stubCanvas()
    vi.spyOn(URL, 'createObjectURL').mockReturnValue('blob:small')

    expect(await downscalePreview(png())).toBe('blob:small')
    // Longest side capped; 2400x1800 is 4:3, so 512x384.
    expect(canvas.width).toBe(512)
    expect(canvas.height).toBe(384)
    expect(drawImage).toHaveBeenCalledWith(bitmap, 0, 0, 512, 384)
    expect(bitmap.close).toHaveBeenCalled()
  })

  it('returns null when a corrupt file fails to decode', async () => {
    vi.stubGlobal(
      'createImageBitmap',
      vi.fn().mockRejectedValue(new Error('bad image')),
    )
    expect(await downscalePreview(png())).toBeNull()
  })

  it('returns null when the canvas yields no blob', async () => {
    const bitmap = fakeBitmap(2400, 1800)
    vi.stubGlobal('createImageBitmap', vi.fn().mockResolvedValue(bitmap))
    stubCanvas(null)
    expect(await downscalePreview(png())).toBeNull()
    expect(bitmap.close).toHaveBeenCalled()
  })
})
