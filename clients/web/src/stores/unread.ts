import {
  computed,
  signal,
  type ReadonlySignal,
  type Signal,
} from '@preact/signals'

export interface UnreadStore {
  /**
   * The rooms currently carrying unread events. Changes only when a room
   * crosses 0 ↔ unread, never on a further event in an already-unread room —
   * so the Unread filter, which is the only consumer that needs the whole
   * set, does not re-run on every message.
   */
  unreadKeys: ReadonlySignal<ReadonlySet<string>>
  /** The count for one room; 0 when never recorded. */
  count(key: string): number
  /** Record one unseen event for a room (the M-W6 WS layer's feed point). */
  recordEvent(key: string): void
  /** Mark a room seen — opening it clears its badge. */
  markSeen(key: string): void
}

/**
 * Client-derived unread indication (ADR 0046, M-W4): the server keeps no
 * read-state per room, so unread is a client-side count of live events seen
 * for rooms the user isn't looking at — the TUI's `rooms.unread` semantics.
 * The M-W6 WebSocket layer feeds it via `recordEvent` per `timeline.event`.
 *
 * The count lives in a signal *per room* rather than one signal holding a map
 * of every room. A row reads only its own count, so a message re-renders that
 * one row instead of all of them; at 1594 rooms the shared-map shape cost
 * ~17ms and ~0.9MB of VDOM garbage per event, which starved the collector and
 * pinned the main thread. Recording an event is now O(1) and allocates
 * nothing — the map of signals is mutated, not copied.
 */
export function createUnreadStore(): UnreadStore {
  /** One count signal per room, created on first read or first event. */
  const counts = new Map<string, Signal<number>>()
  const unreadKeys = signal<ReadonlySet<string>>(new Set())

  /** This room's count signal, minting it on first use. */
  function slot(key: string): Signal<number> {
    const existing = counts.get(key)
    if (existing !== undefined) {
      return existing
    }
    const created = signal(0)
    counts.set(key, created)
    return created
  }

  return {
    unreadKeys: computed(() => unreadKeys.value),

    // Reading through `slot` subscribes the caller to this room alone. A row
    // for a room that has never had an event still gets a (zero) signal, which
    // is what lets its badge appear later without re-rendering its siblings.
    count: (key) => slot(key).value,

    recordEvent(key) {
      const count = slot(key)
      count.value += 1
      if (count.value === 1) {
        unreadKeys.value = new Set(unreadKeys.value).add(key)
      }
    },

    markSeen(key) {
      const count = counts.get(key)
      // Unknown or already-seen room: a no-op, and in particular not a write
      // that would wake the Unread filter.
      if (count === undefined || count.value === 0) {
        return
      }
      count.value = 0
      const next = new Set(unreadKeys.value)
      next.delete(key)
      unreadKeys.value = next
    },
  }
}
