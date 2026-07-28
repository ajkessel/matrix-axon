import { act, cleanup, render } from '@testing-library/preact'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { MAX_BATCH_FILES, useAttachments } from './use-attachments'
import { MAX_UPLOAD_BYTES } from './media-service'

afterEach(cleanup)

function png(name = 'cat.png', size = 10): File {
  const file = new File(['x'], name, { type: 'image/png' })
  Object.defineProperty(file, 'size', { value: size })
  return file
}

type Api = ReturnType<typeof useAttachments>
function harness(scope = 'room-a') {
  const api: { current: Api | null } = { current: null }
  function Probe({ scope: s }: { scope: string }) {
    api.current = useAttachments(s)
    return null
  }
  const view = render(<Probe scope={scope} />)
  return {
    api,
    /** Flush the render, so `batch` reflects the call just made. */
    run: (fn: (api: Api) => void) =>
      act(() => {
        fn(api.current!)
      }),
    setScope: (next: string) =>
      act(() => {
        view.rerender(<Probe scope={next} />)
      }),
    unmount: () =>
      act(() => {
        view.unmount()
      }),
  }
}

describe('useAttachments', () => {
  it('stages additively, so a paste can join a picked batch', () => {
    const { api, run } = harness()
    run((a) => a.stage([png('a.png')]))
    expect(api.current!.batch.items).toHaveLength(1)

    run((a) => a.stage([png('b.png'), png('c.png')]))
    expect(api.current!.batch.items.map((i) => i.file.name)).toEqual([
      'a.png',
      'b.png',
      'c.png',
    ])
  })

  it('gives each item a stable id, because a File is not a usable key', () => {
    // Two identical pastes are equal enough to collide; Preact would then pair
    // the wrong chip with the wrong file (WCR-01).
    const { api, run } = harness()
    run((a) => a.stage([png('same.png'), png('same.png')]))
    const [first, second] = api.current!.batch.items
    expect(first.id).not.toBe(second.id)
  })

  it('removes one without disturbing the others', () => {
    const { api, run } = harness()
    run((a) => a.stage([png('a.png'), png('b.png'), png('c.png')]))
    const target = api.current!.batch.items[1]

    run((a) => a.remove(target.id))

    expect(api.current!.batch.items.map((i) => i.file.name)).toEqual([
      'a.png',
      'c.png',
    ])
  })

  it('reports the accumulated size', () => {
    const { api, run } = harness()
    run((a) => a.stage([png('a.png', 100), png('b.png', 250)]))
    expect(api.current!.batch.totalBytes).toBe(350)
  })

  describe('caps', () => {
    it('refuses past the file count, and says so', () => {
      const { api, run } = harness()
      const many = Array.from({ length: MAX_BATCH_FILES + 3 }, (_, i) =>
        png(`${i}.png`),
      )
      run((a) => a.stage(many))

      expect(api.current!.batch.items).toHaveLength(MAX_BATCH_FILES)
      expect(api.current!.batch.skipped).toBe(3)
      expect(api.current!.batch.skippedReason).toBe('count')
    })

    it('counts against what is already staged, not just this call', () => {
      const { api, run } = harness()
      api.current!.stage(
        Array.from({ length: MAX_BATCH_FILES }, (_, i) => png(`${i}.png`)),
      )
      run((a) => a.stage([png('one-more.png')]))

      expect(api.current!.batch.items).toHaveLength(MAX_BATCH_FILES)
      expect(api.current!.batch.skipped).toBe(1)
    })

    it('refuses on the accumulated size, not per file', () => {
      // Two files each under the cap can be far over it together.
      const { api, run } = harness()
      const half = Math.floor(MAX_UPLOAD_BYTES * 0.6)
      run((a) => a.stage([png('a.png', half)]))
      run((a) => a.stage([png('b.png', half)]))

      expect(api.current!.batch.items.map((i) => i.file.name)).toEqual([
        'a.png',
      ])
      expect(api.current!.batch.skippedReason).toBe('size')
    })
  })

  describe('object url ownership', () => {
    it('revokes on remove', () => {
      const revoke = vi.spyOn(URL, 'revokeObjectURL')
      const { api, run } = harness()
      run((a) => a.stage([png('a.png')]))
      const url = api.current!.batch.items[0].previewUrl!

      run((a) => a.remove(api.current!.batch.items[0].id))

      expect(revoke).toHaveBeenCalledWith(url)
      revoke.mockRestore()
    })

    it('revokes every url on clear', () => {
      const revoke = vi.spyOn(URL, 'revokeObjectURL')
      const { api, run } = harness()
      run((a) => a.stage([png('a.png'), png('b.png'), png('c.png')]))
      const urls = api.current!.batch.items.map((i) => i.previewUrl!)

      run((a) => a.clear())

      for (const url of urls) {
        expect(revoke).toHaveBeenCalledWith(url)
      }
      revoke.mockRestore()
    })

    it('revokes when the room changes', () => {
      // Neither RoomPage nor ThreadPanel remounts on a route change, so a file
      // staged in room A would otherwise still be staged — and sent — in B.
      const revoke = vi.spyOn(URL, 'revokeObjectURL')
      const { api, run, setScope } = harness('room-a')
      run((a) => a.stage([png('a.png')]))
      const url = api.current!.batch.items[0].previewUrl!

      setScope('room-b')

      expect(revoke).toHaveBeenCalledWith(url)
      expect(api.current!.batch.items).toHaveLength(0)
      revoke.mockRestore()
    })

    it('revokes on unmount', () => {
      const revoke = vi.spyOn(URL, 'revokeObjectURL')
      const { api, run, unmount } = harness()
      run((a) => a.stage([png('a.png')]))
      const url = api.current!.batch.items[0].previewUrl!

      unmount()

      expect(revoke).toHaveBeenCalledWith(url)
      revoke.mockRestore()
    })

    it('makes no preview for a non-image, and nothing to revoke', () => {
      const { api, run } = harness()
      const pdf = new File(['x'], 'notes.pdf', { type: 'application/pdf' })
      run((a) => a.stage([pdf]))
      expect(api.current!.batch.items[0].previewUrl).toBeNull()
    })
  })
})
