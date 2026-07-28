import { perfActive, perfOverlayEntries } from '../perf'

/**
 * A live readout of the scroll-anchoring marks, drawn over the app when
 * `?perf=1` is on (ADR 0071's marks, but visible).
 *
 * iOS Safari has no on-device console, so reading `performance` marks from a
 * phone otherwise means tethering it to a Mac running Web Inspector. Putting
 * the numbers on the screen means an ordinary screen recording captures the
 * measurement and the behaviour it explains in the same frames, which is what
 * makes them correlatable at all.
 *
 * Deliberately `pointer-events: none` and outside the timeline's layout, so
 * watching the scroll cannot disturb it.
 */
export function PerfOverlay() {
  if (!perfActive.value) {
    return null
  }
  const entries = perfOverlayEntries.value
  return (
    <div class="perf-overlay" aria-hidden="true">
      {entries.length === 0 ? (
        <div>anchor: waiting…</div>
      ) : (
        entries.map((entry) => (
          <div key={`${entry.at}:${entry.name}`}>
            {Math.round(entry.at)} {entry.name.replace('timeline:', '')}
            {entry.detail === undefined
              ? ''
              : ` ${Object.entries(entry.detail)
                  .map(([key, value]) => `${key}=${value}`)
                  .join(' ')}`}
          </div>
        ))
      )}
    </div>
  )
}
