import { signal } from '@preact/signals'

const STORAGE_KEY = 'axon.perf'

let enabled: boolean | null = null

/**
 * Turn instrumentation on or off from the settings store, which is the
 * ordinary way to reach it — `?perf=1` remains for the e2e harness and for a
 * one-off session. Cleared state also clears the readout, so switching it off
 * does not leave a stale tail on screen.
 */
export function setPerfEnabled(on: boolean): void {
  if (enabled === on) {
    return
  }
  enabled = on
  perfActive.value = on
  try {
    if (on) {
      window.sessionStorage.setItem(STORAGE_KEY, '1')
    } else {
      window.sessionStorage.removeItem(STORAGE_KEY)
    }
  } catch {
    // Private-mode storage failures should not break the app.
  }
  if (!on) {
    perfOverlayEntries.value = []
    recentMarks = []
  }
}

/**
 * Marks mirrored to the on-screen readout — the ones worth watching live.
 * Both the scroll anchor and the paging that feeds it, since a jump is usually
 * the interaction between the two; the chain's own start/end and per-fetch
 * marks are left to the full timeline, which they would otherwise crowd out.
 */
const OVERLAY_PREFIXES = [
  'timeline:anchor',
  'timeline:auto-page:fetched',
  'timeline:auto-page:stop',
  'transition:',
]

/**
 * Marks that begin a "back to the room list" transition — the one ADR 0071
 * built its harness around, and the one a phone reports as slow.
 */
const TRANSITION_STARTS = new Set([
  'room-page:mobile-back',
  'room-page:swipe-right-accepted',
])
/**
 * How long after the start to read the marks back. Long enough for the list to
 * have rendered and settled on a slow device; short enough to still belong to
 * the gesture that caused it.
 */
const TRANSITION_SETTLE_MS = 800
/** How many of those to keep; enough to cover a gesture, short enough to read. */
const OVERLAY_MAX = 10

export interface PerfOverlayEntry {
  at: number
  name: string
  detail?: Record<string, string | number | boolean | null>
}

/**
 * The tail of the overlay-worthy marks. iOS Safari has no on-device console —
 * reading marks there otherwise means tethering the phone to a Mac — so the
 * numbers go on the screen instead, where a screen recording captures them in
 * the same frames as the behaviour they explain.
 */
export const perfOverlayEntries = signal<readonly PerfOverlayEntry[]>([])

/** Reactive mirror of `perfEnabled()`, so the readout can mount and unmount. */
export const perfActive = signal(false)

export function perfEnabled(): boolean {
  if (enabled !== null) {
    return enabled
  }
  const params = new URLSearchParams(window.location.search)
  if (params.get('perf') === '1') {
    enabled = true
    perfActive.value = true
    try {
      window.sessionStorage.setItem(STORAGE_KEY, '1')
    } catch {
      // Private-mode storage failures should not break the app.
    }
    return true
  }
  try {
    enabled = window.sessionStorage.getItem(STORAGE_KEY) === '1'
  } catch {
    enabled = false
  }
  perfActive.value = enabled
  return enabled
}

export function perfMark(
  name: string,
  detail?: Record<string, string | number | boolean | null>,
): void {
  if (!perfEnabled()) {
    return
  }
  const markName = `axon:${name}`
  try {
    if (detail === undefined) {
      performance.mark(markName)
    } else {
      performance.mark(markName, { detail })
    }
  } catch {
    performance.mark(markName)
  }
  const at = performance.now()
  recentMarks.push({ name, t: at, detail })
  if (recentMarks.length > RECENT_MARKS_MAX) {
    recentMarks = recentMarks.slice(-RECENT_MARKS_MAX)
  }
  if (OVERLAY_PREFIXES.some((prefix) => name.startsWith(prefix))) {
    perfOverlayEntries.value = [
      ...perfOverlayEntries.value,
      { at, name, detail },
    ].slice(-OVERLAY_MAX)
  }
  if (TRANSITION_STARTS.has(name)) {
    scheduleTransitionSummary(at)
  }
}

interface AxonMark {
  name: string
  t: number
  detail: unknown
}

/**
 * A bounded log of recent marks, kept alongside the `performance` timeline
 * rather than read back from it: engines cap and evict entries there, and a
 * summary that silently loses its inputs is worse than no summary. Sized to
 * comfortably span one transition.
 */
const RECENT_MARKS_MAX = 400
let recentMarks: AxonMark[] = []

/** This app's marks laid down since `from`, oldest first. */
function axonMarksSince(from: number): AxonMark[] {
  return recentMarks.filter((mark) => mark.t >= from)
}

/** Total time inside `start`/`end` pairs, ignoring an unclosed trailing one. */
function pairedTotal(marks: AxonMark[], start: string, end: string): number {
  let open: number | null = null
  let total = 0
  for (const mark of marks) {
    if (mark.name === start && open === null) {
      open = mark.t
    } else if (mark.name === end && open !== null) {
      total += mark.t - open
      open = null
    }
  }
  return total
}

function detailNumber(mark: AxonMark | undefined, key: string): number | null {
  const value = (mark?.detail as Record<string, unknown> | undefined)?.[key]
  return typeof value === 'number' ? value : null
}

let transitionTimer: ReturnType<typeof setTimeout> | null = null

function scheduleTransitionSummary(startedAt: number): void {
  if (transitionTimer !== null) {
    clearTimeout(transitionTimer)
  }
  transitionTimer = setTimeout(() => {
    transitionTimer = null
    summariseTransition(startedAt)
  }, TRANSITION_SETTLE_MS)
}

/**
 * Reduce a back-transition's marks to the same phase breakdown the e2e perf
 * lane reports (`e2e/perf-helpers.ts`), and emit it as one mark.
 *
 * The raw marks are unreadable on a phone — thousands of them, scrolling past
 * faster than a screen recording can capture — but this is four numbers that
 * sit still. It is what makes a report from a device with no console into an
 * actionable measurement, which is how this session's scrolling bug was
 * eventually pinned down.
 */
function summariseTransition(startedAt: number): void {
  const marks = axonMarksSince(startedAt)
  const renders = marks.filter((mark) => mark.name === 'room-list:render')
  if (renders.length === 0) {
    // The gesture closed a thread, or never reached the list. Nothing to say.
    return
  }
  const frames = marks.filter((mark) =>
    mark.name.startsWith('room-list:post-render'),
  )
  const firstFrame = frames.find((mark) => mark.name.endsWith(':now'))
  const lastFrame = frames.findLast((mark) => mark.name.endsWith(':raf2'))
  perfMark('transition:back', {
    total: Math.round(renders[renders.length - 1].t - startedAt),
    list: Math.round(
      pairedTotal(
        marks,
        'room-list:visible-compute:start',
        'room-list:visible-compute:end',
      ) +
        pairedTotal(marks, 'room-list:measure:start', 'room-list:measure:end'),
    ),
    renders: renders.length,
    frames:
      firstFrame === undefined || lastFrame === undefined
        ? null
        : Math.round(lastFrame.t - firstFrame.t),
    rooms: detailNumber(
      marks.find((mark) => mark.name === 'room-list:visible-compute:start'),
      'rooms',
    ),
  })
}

export function perfMarkFrames(name: string): void {
  if (!perfEnabled()) {
    return
  }
  perfMark(`${name}:now`)
  requestAnimationFrame(() => {
    perfMark(`${name}:raf1`)
    requestAnimationFrame(() => perfMark(`${name}:raf2`))
  })
}
