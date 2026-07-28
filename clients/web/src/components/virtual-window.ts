/**
 * Fixed-pitch list windowing: which slice of a long, uniform-height list
 * overlaps the scroll viewport, and how much blank space stands in for the
 * rows on either side.
 *
 * The room list renders one row per joined room, and a power user is in
 * thousands. Mounting them all costs ~9,600 DOM elements and re-renders the
 * whole set on every signal change that any row reads. Windowing caps both at
 * the viewport.
 *
 * Rows must share a pitch (their height plus whatever separates them), which
 * `RoomList` measures from the live DOM rather than assuming — font size and
 * browser zoom both move it.
 */
export interface RowWindow {
  /** First rendered row (inclusive). */
  start: number
  /** One past the last rendered row. */
  end: number
  /** Pixels of padding standing in for rows before `start`. */
  padTop: number
  /** Pixels of padding standing in for rows after `end`. */
  padBottom: number
}

/**
 * `scrollTop` is measured from the list's own top edge, not the scroll
 * container's — the list sits below the filter controls.
 *
 * A zero-height viewport means the list's scroll container is *hidden*, not
 * that it is immeasurable: at single-pane widths the sidebar is `display:none`
 * while a room is open. Nothing is rendered then. Conflating that with "cannot
 * measure" is what made a phone mount every row of the list into a pane nobody
 * could see. The caller decides which case it is — a list with no scroll
 * container at all (jsdom, or the list outside the sidebar) never reaches here.
 *
 * A non-positive pitch is a genuine measurement failure and still degrades to
 * rendering every row: correct, just not windowed.
 */
export function rowWindow(
  count: number,
  pitch: number,
  scrollTop: number,
  viewportHeight: number,
  overscan: number,
): RowWindow {
  if (count === 0) {
    return { start: 0, end: 0, padTop: 0, padBottom: 0 }
  }
  if (!(pitch > 0)) {
    return { start: 0, end: count, padTop: 0, padBottom: 0 }
  }
  if (!(viewportHeight > 0)) {
    // Hidden: render nothing, but keep the list's height so that showing it
    // again restores the same scroll offset.
    return { start: 0, end: 0, padTop: 0, padBottom: count * pitch }
  }
  const top = Math.max(0, scrollTop)
  const first = Math.floor(top / pitch)
  const last = Math.ceil((top + viewportHeight) / pitch)
  const start = Math.max(0, first - overscan)
  const end = Math.min(count, last + overscan)
  return {
    start,
    end,
    padTop: start * pitch,
    padBottom: (count - end) * pitch,
  }
}

/**
 * The scroll offset that brings row `index` fully into view, or `null` when it
 * already is. Returned in the same list-relative coordinates `rowWindow` takes.
 */
export function scrollToReveal(
  index: number,
  pitch: number,
  scrollTop: number,
  viewportHeight: number,
): number | null {
  if (!(viewportHeight > 0) || !(pitch > 0)) {
    return null
  }
  const rowTop = index * pitch
  const rowBottom = rowTop + pitch
  if (rowTop < scrollTop) {
    return rowTop
  }
  if (rowBottom > scrollTop + viewportHeight) {
    return rowBottom - viewportHeight
  }
  return null
}
