import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { perfMark, perfOverlayEntries, setPerfEnabled } from './perf'

/** The detail of the one `transition:back` mark, or `null` if none was made. */
function backSummary(): Record<string, unknown> | null {
  const entry = perfOverlayEntries.value.findLast(
    (candidate) => candidate.name === 'transition:back',
  )
  return entry?.detail ?? null
}

describe('back-transition summary', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    performance.clearMarks()
    perfOverlayEntries.value = []
    setPerfEnabled(true)
  })
  afterEach(() => {
    vi.useRealTimers()
    setPerfEnabled(false)
    performance.clearMarks()
  })

  /** The marks a real back-transition lays down, in order. */
  function layTransitionMarks() {
    perfMark('room-page:mobile-back', { target: 'room-list' })
    perfMark('room-list:visible-compute:start', { rooms: 150 })
    vi.advanceTimersByTime(5)
    perfMark('room-list:visible-compute:end')
    perfMark('room-list:measure:start')
    vi.advanceTimersByTime(3)
    perfMark('room-list:measure:end')
    perfMark('room-list:render')
    perfMark('room-list:post-render:now')
    vi.advanceTimersByTime(16)
    perfMark('room-list:post-render:raf2')
  }

  it('reduces a transition to the phases the e2e lane reports', () => {
    layTransitionMarks()
    expect(backSummary()).toBeNull() // not until it has settled

    vi.advanceTimersByTime(800)

    const summary = backSummary()
    // `total` runs from the gesture to the last list render; `list` is the
    // room-list phase (compute + measure) the harness attributes separately,
    // which is what distinguishes a slow list from a slow teardown.
    expect(summary).toMatchObject({
      list: 8,
      renders: 1,
      frames: 16,
      rooms: 150,
    })
    expect(summary?.total).toBeGreaterThanOrEqual(8)
  })

  it('counts every render pass, so a re-render storm is visible', () => {
    layTransitionMarks()
    perfMark('room-list:render')
    perfMark('room-list:render')
    vi.advanceTimersByTime(800)

    expect(backSummary()).toMatchObject({ renders: 3 })
  })

  it('says nothing when the gesture never reached the list', () => {
    // Closing a thread panel emits the same start mark.
    perfMark('room-page:mobile-back', { target: 'thread-close' })
    vi.advanceTimersByTime(800)

    expect(backSummary()).toBeNull()
  })

  it('emits nothing at all while instrumentation is off', () => {
    setPerfEnabled(false)
    layTransitionMarks()
    vi.advanceTimersByTime(800)

    expect(perfOverlayEntries.value).toHaveLength(0)
  })
})
