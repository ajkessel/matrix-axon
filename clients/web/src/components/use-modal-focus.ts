import type { RefObject } from 'preact'
import { useEffect, useRef } from 'preact/hooks'

/** What a modal considers reachable by Tab. */
const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), ' +
  'select:not([disabled]), textarea:not([disabled]), ' +
  '[tabindex]:not([tabindex="-1"])'

function isVisible(element: HTMLElement): boolean {
  return (
    !element.hasAttribute('hidden') &&
    element.getAttribute('aria-hidden') !== 'true'
  )
}

function collectFocusable(root: ParentNode): HTMLElement[] {
  const focusables: HTMLElement[] = []
  const visit = (node: ParentNode) => {
    if (
      node instanceof HTMLElement &&
      node.matches(FOCUSABLE) &&
      isVisible(node)
    ) {
      focusables.push(node)
    }
    for (const child of node.querySelectorAll<HTMLElement>('*')) {
      if (child.matches(FOCUSABLE) && isVisible(child)) {
        focusables.push(child)
      }
      if (child.shadowRoot !== null) {
        visit(child.shadowRoot)
      }
    }
  }
  visit(root)
  return focusables
}

function activeElementDeep(
  root: Document | ShadowRoot = document,
): HTMLElement | null {
  const active = root.activeElement
  if (active === null) {
    return null
  }
  if (active.shadowRoot?.activeElement instanceof HTMLElement) {
    return activeElementDeep(active.shadowRoot)
  }
  return active instanceof HTMLElement ? active : null
}

/**
 * The modal focus contract (ADR 0063 pattern; WCR-14), shared by every
 * overlay: on mount, remember the focused element and move focus to the
 * first focusable inside the container; while mounted, trap Tab inside it
 * (wrapping at both ends, and pulling focus back in if it ever ends up
 * outside); on unmount, restore focus to where it was.
 *
 * Escape stays each modal's own binding (`useShortcuts` with `capture`), so
 * the staged-Escape ordering is unchanged. Attach the returned ref to the
 * modal's outermost element.
 */
export function useModalFocus<T extends HTMLElement = HTMLElement>(): {
  containerRef: RefObject<T>
} {
  const containerRef = useRef<T>(null)

  useEffect(() => {
    const previouslyFocused = document.activeElement as HTMLElement | null
    const container = containerRef.current
    collectFocusable(container ?? document.createElement('div'))[0]?.focus()

    // Document-level and capture-phase, so the trap holds no matter where
    // inside (or outside) the modal the keydown lands.
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Tab') {
        return
      }
      const root = containerRef.current
      if (root === null) {
        return
      }
      const focusables = collectFocusable(root)
      if (focusables.length === 0) {
        return
      }
      const active = activeElementDeep()
      const index = active === null ? -1 : focusables.indexOf(active)
      const nextIndex =
        index === -1
          ? event.shiftKey
            ? focusables.length - 1
            : 0
          : (index + (event.shiftKey ? -1 : 1) + focusables.length) %
            focusables.length
      event.preventDefault()
      focusables[nextIndex]?.focus()
    }
    document.addEventListener('keydown', onKeyDown, true)
    return () => {
      document.removeEventListener('keydown', onKeyDown, true)
      previouslyFocused?.focus?.()
    }
  }, [])

  return { containerRef }
}
