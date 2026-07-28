import { useCallback, useState } from 'preact/hooks'

/**
 * Which galleries the reader has ungrouped, for this page load only.
 *
 * Module-level rather than a store or `localStorage`, deliberately. Switching
 * rooms unmounts the timeline, so component state cannot survive it — but
 * "which galleries I expanded" is a transient view preference, not something
 * to carry across sessions or sync between devices. A module-level set dies on
 * reload, which is exactly the intended lifetime.
 *
 * Keyed by event id rather than by the gallery's key, and matched against
 * *every* event in the run. A run's first event is not stable: back-paginating
 * can pull in an older image that joins it, which would change a
 * first-event-derived key and silently re-group a gallery the reader had
 * opened. Matching any member survives that, and survives a run splitting.
 */
const ungrouped = new Set<string>()

/** Test seam — jsdom keeps one module instance across a file's tests. */
export function resetGalleryExpansion(): void {
  ungrouped.clear()
}

export function isGalleryUngrouped(eventIds: readonly string[]): boolean {
  return eventIds.some((id) => ungrouped.has(id))
}

export function setGalleryUngrouped(
  eventIds: readonly string[],
  next: boolean,
): void {
  if (next) {
    // Every member, so the run can gain or lose images and still be found.
    for (const id of eventIds) {
      ungrouped.add(id)
    }
  } else {
    for (const id of eventIds) {
      ungrouped.delete(id)
    }
  }
}

/**
 * `useState` for a gallery's expanded flag, backed by the session-scoped set
 * above so leaving a room and coming back preserves the choice.
 */
export function useGalleryExpansion(
  eventIds: readonly string[],
): [boolean, (next: boolean) => void] {
  const [expanded, setExpandedState] = useState(() =>
    isGalleryUngrouped(eventIds),
  )
  const setExpanded = useCallback(
    (next: boolean) => {
      setGalleryUngrouped(eventIds, next)
      setExpandedState(next)
    },
    [eventIds],
  )
  return [expanded, setExpanded]
}
