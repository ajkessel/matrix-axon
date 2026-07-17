import { describe, expect, it } from 'vitest'
import { rowWindow, scrollToReveal } from './virtual-window'

const PITCH = 64
const VIEWPORT = 640 // ten rows

describe('rowWindow', () => {
  it('windows a long list to the viewport plus overscan', () => {
    const { start, end } = rowWindow(1594, PITCH, 0, VIEWPORT, 6)
    expect(start).toBe(0)
    // Ten rows on screen, rounded up, plus six of overscan below.
    expect(end).toBe(16)
  })

  it('pads for the rows it does not render, preserving total height', () => {
    const { start, end, padTop, padBottom } = rowWindow(
      1594,
      PITCH,
      100 * PITCH,
      VIEWPORT,
      6,
    )
    expect(start).toBe(94)
    expect(end).toBe(116)
    expect(padTop).toBe(94 * PITCH)
    expect(padBottom).toBe((1594 - 116) * PITCH)
    // The scrollbar must not move when the window does.
    expect(padTop + (end - start) * PITCH + padBottom).toBe(1594 * PITCH)
  })

  it('clamps at both ends rather than overscanning past them', () => {
    expect(rowWindow(1594, PITCH, 0, VIEWPORT, 6).padTop).toBe(0)
    const bottom = rowWindow(1594, PITCH, 1594 * PITCH, VIEWPORT, 6)
    expect(bottom.end).toBe(1594)
    expect(bottom.padBottom).toBe(0)
  })

  /**
   * A hidden scroll container, not an immeasurable one. At single-pane widths
   * the sidebar is `display:none` while a room is open; rendering every row
   * into it put ~9,000 elements on a phone for a pane nobody could see.
   */
  it('renders nothing while its scroll container is hidden', () => {
    expect(rowWindow(1594, PITCH, 0, 0, 6)).toEqual({
      start: 0,
      end: 0,
      padTop: 0,
      // The list keeps its height, so showing it again restores the offset.
      padBottom: 1594 * PITCH,
    })
  })

  it('renders everything when the pitch cannot be measured', () => {
    expect(rowWindow(1594, 0, 0, VIEWPORT, 6).end).toBe(1594)
  })

  it('handles an empty list', () => {
    expect(rowWindow(0, PITCH, 0, VIEWPORT, 6)).toEqual({
      start: 0,
      end: 0,
      padTop: 0,
      padBottom: 0,
    })
  })

  it('ignores a negative scroll offset (overscroll bounce)', () => {
    expect(rowWindow(1594, PITCH, -200, VIEWPORT, 6).start).toBe(0)
  })
})

describe('scrollToReveal', () => {
  it('scrolls up to a row above the viewport', () => {
    expect(scrollToReveal(10, PITCH, 20 * PITCH, VIEWPORT)).toBe(10 * PITCH)
  })

  it('scrolls down just far enough to expose a row below it', () => {
    expect(scrollToReveal(20, PITCH, 0, VIEWPORT)).toBe(21 * PITCH - VIEWPORT)
  })

  it('leaves a row that is already visible alone', () => {
    expect(scrollToReveal(5, PITCH, 0, VIEWPORT)).toBeNull()
  })

  it('is inert without measurable geometry', () => {
    expect(scrollToReveal(5, PITCH, 0, 0)).toBeNull()
  })
})
