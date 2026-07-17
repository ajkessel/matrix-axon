/**
 * Global test-environment shims. jsdom here implements neither `matchMedia`
 * nor a working `localStorage` (the latter is injected per-store instead, see
 * `memory-storage.ts`).
 *
 * `matches: false` makes every media query report "not matching", which is the
 * wide two-pane layout (ADR 0062) — the right default for component tests.
 * A test that needs the narrow branch stubs `window.matchMedia` itself.
 */
if (typeof window.matchMedia !== 'function') {
  window.matchMedia = (query: string): MediaQueryList =>
    ({
      media: query,
      matches: false,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    }) as MediaQueryList
}

/**
 * jsdom implements neither `URL.createObjectURL` nor `revokeObjectURL`, which
 * the media layer (ADR 0064) depends on. A counter-backed stub is enough for
 * component tests; the media-service test replaces these with spies to assert
 * the revocation bookkeeping. Deliberately no `IntersectionObserver` stub — its
 * absence drives components down the eager-load fallback the timeline uses.
 */
if (typeof URL.createObjectURL !== 'function') {
  let counter = 0
  URL.createObjectURL = () => `blob:mock/${counter++}`
  URL.revokeObjectURL = () => {}
}
