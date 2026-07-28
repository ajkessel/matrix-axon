/**
 * One module-level `IntersectionObserver` for the whole timeline (ADR 0064).
 *
 * A per-image observer would allocate one observer per mounted `<img>`; a
 * single shared observer with a `WeakMap` of callbacks scales to a long,
 * un-windowed timeline. `rootMargin: 200px` starts a fetch just before an image
 * scrolls into view, so it is usually ready by the time it is visible.
 *
 * Callers must guard the `typeof IntersectionObserver === 'undefined'` case
 * (jsdom) themselves and fall back to eager loading — this module is only
 * reached where the API exists.
 */

type OnVisible = () => void

let observer: IntersectionObserver | null = null
const callbacks = new WeakMap<Element, OnVisible>()

function ensureObserver(): IntersectionObserver {
  if (observer === null) {
    observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (!entry.isIntersecting) {
            continue
          }
          const onVisible = callbacks.get(entry.target)
          if (onVisible !== undefined) {
            // Fire once: unobserve before calling so a re-intersection during a
            // fast scroll cannot double-enqueue.
            observer?.unobserve(entry.target)
            callbacks.delete(entry.target)
            onVisible()
          }
        }
      },
      { rootMargin: '200px' },
    )
  }
  return observer
}

/**
 * Call `onVisible` once, the first time `el` intersects the viewport (plus the
 * 200px margin). Returns an unsubscribe that stops observing if `el` never
 * became visible.
 */
export function observeVisible(el: Element, onVisible: OnVisible): () => void {
  const obs = ensureObserver()
  callbacks.set(el, onVisible)
  obs.observe(el)
  return () => {
    callbacks.delete(el)
    obs.unobserve(el)
  }
}
