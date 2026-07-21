import { effect, signal, type Signal } from '@preact/signals'

/**
 * Schema-versioned client settings over `localStorage` (ADR 0046, M-W3).
 *
 * One key holds one JSON envelope with an explicit `version`, so future
 * shapes migrate deliberately instead of half-parsing old data. Anything
 * unparseable — missing, corrupt, an unknown future version — resets to
 * defaults rather than wedging the app: settings are preferences, not data.
 * New fields are added with defaults (an old envelope missing them still
 * parses); the version number bumps only on incompatible reshapes.
 */
const STORAGE_KEY = 'axon.settings'

export type Theme = 'system' | 'light' | 'dark'

/** Room-list sort modes (ADR 0042). */
export type RoomSort = 'recent' | 'oldest' | 'az' | 'za'

/**
 * Room-list filter categories (ADR 0042). The name filter is deliberately
 * absent: `Name(query)` is session-only and persists as `all`, since
 * restoring a stale query string is surprising.
 */
export type RoomFilter = 'all' | 'dms' | 'groups' | 'unread' | 'favorites'

/**
 * Timeline timestamp format. `12h` is the shipped default (kept from the
 * hardcoded format it replaces); `24h` restores what locale-aware rendering
 * used to give 24-hour-clock locales.
 */
export type TimeFormat = '12h' | '24h'

/** Version 1 settings envelope, the shape at rest in `localStorage`. */
export interface SettingsV1 {
  version: 1
  /** Color scheme; `system` follows `prefers-color-scheme`. */
  theme: Theme
  /**
   * The account the UI is "in" (account switch). Account-scoped routes carry
   * the account id in the URL; this is only the default for entry points
   * that don't, and it may point at an account that no longer exists —
   * consumers must treat it as a hint, not a fact.
   */
  activeAccountId: string | null
  /**
   * Pinned rooms (ADR 0038), each a room key (`accountId/roomId`, see
   * `stores/room-list.ts`), most recently pinned first. May reference rooms
   * that no longer exist; the room list simply won't match them.
   */
  pinnedRooms: string[]
  /** Persisted room-list sort mode (ADR 0042). */
  roomSort: RoomSort
  /** Persisted room-list filter category (ADR 0042). */
  roomFilter: RoomFilter
  /**
   * Whether the room-list sidebar is hidden (ADR 0062). Only consulted at
   * viewports wide enough for two panes; below that the route decides which
   * pane shows, and a stale `true` here must not hide the room list.
   */
  sidebarCollapsed: boolean
  /**
   * Whether the timeline shows state events (joins, topic changes, …). A
   * preference, not per-room view state: it used to be an ephemeral checkbox in
   * the room header that reset on every room switch and reload. Off by default
   * — the TUI hides them too.
   */
  showStateEvents: boolean
  /** Whether redacted timeline events are hidden entirely. Off by default. */
  hideRedactedEvents: boolean
  /** Whether room-list rows show a one-line latest-message preview. */
  previewRoom: boolean
  /** Timeline timestamp format (Settings → Timeline). */
  timeFormat: TimeFormat
  /** User-sized message composer height in CSS pixels; null means default. */
  messageComposerHeight: number | null
  /** Most recently used reaction keys, newest first. */
  recentReactions: string[]
  /**
   * Developer diagnostics: exposes per-event inspect actions in the timeline.
   * Off by default because event content can include decrypted message data.
   */
  developerMode: boolean
}

const DEFAULTS: SettingsV1 = {
  version: 1,
  theme: 'system',
  activeAccountId: null,
  pinnedRooms: [],
  roomSort: 'recent',
  roomFilter: 'all',
  sidebarCollapsed: false,
  showStateEvents: false,
  hideRedactedEvents: false,
  previewRoom: true,
  timeFormat: '12h',
  messageComposerHeight: null,
  recentReactions: [],
  developerMode: false,
}

const MAX_RECENT_REACTIONS = 3

const THEMES: readonly Theme[] = ['system', 'light', 'dark']

const TIME_FORMATS: readonly TimeFormat[] = ['12h', '24h']

/**
 * Cycle order for the sort shortcut, matching the TUI's `RoomSort::next`
 * (`clients/tui/src/app.rs`) so the two clients step in the same sequence.
 */
export const ROOM_SORTS: readonly RoomSort[] = ['recent', 'oldest', 'az', 'za']

/**
 * Cycle order for the filter shortcut, matching the TUI's `RoomFilter::CYCLE`
 * (`clients/tui/src/app.rs`). The name filter is deliberately outside the
 * cycle, exactly as in the TUI (ADR 0042).
 */
export const ROOM_FILTERS: readonly RoomFilter[] = [
  'all',
  'dms',
  'groups',
  'unread',
  'favorites',
]

/** The next value after `current`, wrapping. Unknown values restart the cycle. */
export function nextIn<T>(cycle: readonly T[], current: T): T {
  const index = cycle.indexOf(current)
  return cycle[(index + 1) % cycle.length]
}

/** Keep `value` when it is one of `allowed`, else the default. */
function oneOf<T extends string>(
  allowed: readonly T[],
  value: unknown,
  fallback: T,
): T {
  return allowed.includes(value as T) ? (value as T) : fallback
}

/** Parse a stored envelope, falling back to defaults on any irregularity. */
function parse(raw: string | null): SettingsV1 {
  if (raw === null) {
    return DEFAULTS
  }
  let value: unknown
  try {
    value = JSON.parse(raw)
  } catch {
    return DEFAULTS
  }
  if (
    typeof value !== 'object' ||
    value === null ||
    (value as { version?: unknown }).version !== 1
  ) {
    return DEFAULTS
  }
  const v1 = value as Partial<SettingsV1>
  return {
    version: 1,
    theme: oneOf(THEMES, v1.theme, DEFAULTS.theme),
    activeAccountId:
      typeof v1.activeAccountId === 'string' ? v1.activeAccountId : null,
    pinnedRooms: Array.isArray(v1.pinnedRooms)
      ? v1.pinnedRooms.filter((key): key is string => typeof key === 'string')
      : [],
    roomSort: oneOf(ROOM_SORTS, v1.roomSort, DEFAULTS.roomSort),
    roomFilter: oneOf(ROOM_FILTERS, v1.roomFilter, DEFAULTS.roomFilter),
    sidebarCollapsed:
      typeof v1.sidebarCollapsed === 'boolean'
        ? v1.sidebarCollapsed
        : DEFAULTS.sidebarCollapsed,
    showStateEvents:
      typeof v1.showStateEvents === 'boolean'
        ? v1.showStateEvents
        : DEFAULTS.showStateEvents,
    hideRedactedEvents:
      typeof v1.hideRedactedEvents === 'boolean'
        ? v1.hideRedactedEvents
        : DEFAULTS.hideRedactedEvents,
    previewRoom:
      typeof v1.previewRoom === 'boolean'
        ? v1.previewRoom
        : DEFAULTS.previewRoom,
    timeFormat: oneOf(TIME_FORMATS, v1.timeFormat, DEFAULTS.timeFormat),
    messageComposerHeight:
      typeof v1.messageComposerHeight === 'number' &&
      Number.isFinite(v1.messageComposerHeight) &&
      v1.messageComposerHeight >= 38
        ? Math.round(v1.messageComposerHeight)
        : DEFAULTS.messageComposerHeight,
    recentReactions: Array.isArray(v1.recentReactions)
      ? v1.recentReactions
          .filter((key): key is string => typeof key === 'string')
          .filter((key) => key.trim() !== '')
          .slice(0, MAX_RECENT_REACTIONS)
      : [],
    developerMode:
      typeof v1.developerMode === 'boolean'
        ? v1.developerMode
        : DEFAULTS.developerMode,
  }
}

export interface SettingsStore {
  theme: Signal<Theme>
  activeAccountId: Signal<string | null>
  pinnedRooms: Signal<string[]>
  roomSort: Signal<RoomSort>
  roomFilter: Signal<RoomFilter>
  sidebarCollapsed: Signal<boolean>
  showStateEvents: Signal<boolean>
  hideRedactedEvents: Signal<boolean>
  previewRoom: Signal<boolean>
  timeFormat: Signal<TimeFormat>
  messageComposerHeight: Signal<number | null>
  recentReactions: Signal<string[]>
  developerMode: Signal<boolean>
  /**
   * Pin a room key, or re-pin an already-pinned one to the top — most
   * recently pinned first (ADR 0038).
   */
  pinRoom(key: string): void
  /** Unpin a room key; a no-op when it isn't pinned. */
  unpinRoom(key: string): void
  /** Record a reaction key as recently used, newest first. */
  recordRecentReaction(key: string): void
}

/**
 * Load settings and keep every change persisted. Storage is injectable for
 * tests (jsdom under Node 25 has no working `localStorage`).
 */
export function createSettingsStore(
  storage: Storage = window.localStorage,
): SettingsStore {
  const initial = parse(storage.getItem(STORAGE_KEY))
  const theme = signal<Theme>(initial.theme)
  const activeAccountId = signal<string | null>(initial.activeAccountId)
  const pinnedRooms = signal<string[]>(initial.pinnedRooms)
  const roomSort = signal<RoomSort>(initial.roomSort)
  const roomFilter = signal<RoomFilter>(initial.roomFilter)
  const sidebarCollapsed = signal<boolean>(initial.sidebarCollapsed)
  const showStateEvents = signal<boolean>(initial.showStateEvents)
  const hideRedactedEvents = signal<boolean>(initial.hideRedactedEvents)
  const previewRoom = signal<boolean>(initial.previewRoom)
  const timeFormat = signal<TimeFormat>(initial.timeFormat)
  const messageComposerHeight = signal<number | null>(
    initial.messageComposerHeight,
  )
  const recentReactions = signal<string[]>(initial.recentReactions)
  const developerMode = signal<boolean>(initial.developerMode)

  effect(() => {
    const envelope: SettingsV1 = {
      version: 1,
      theme: theme.value,
      activeAccountId: activeAccountId.value,
      pinnedRooms: pinnedRooms.value,
      roomSort: roomSort.value,
      roomFilter: roomFilter.value,
      sidebarCollapsed: sidebarCollapsed.value,
      showStateEvents: showStateEvents.value,
      hideRedactedEvents: hideRedactedEvents.value,
      previewRoom: previewRoom.value,
      timeFormat: timeFormat.value,
      messageComposerHeight: messageComposerHeight.value,
      recentReactions: recentReactions.value,
      developerMode: developerMode.value,
    }
    try {
      storage.setItem(STORAGE_KEY, JSON.stringify(envelope))
    } catch {
      // Quota or storage-denied: settings are preferences — losing a persist
      // must not throw into whatever signal write triggered this effect.
    }
  })

  return {
    theme,
    activeAccountId,
    pinnedRooms,
    roomSort,
    roomFilter,
    sidebarCollapsed,
    showStateEvents,
    hideRedactedEvents,
    previewRoom,
    timeFormat,
    messageComposerHeight,
    recentReactions,
    developerMode,
    pinRoom(key: string) {
      pinnedRooms.value = [key, ...pinnedRooms.value.filter((k) => k !== key)]
    },
    unpinRoom(key: string) {
      pinnedRooms.value = pinnedRooms.value.filter((k) => k !== key)
    },
    recordRecentReaction(key: string) {
      const trimmed = key.trim()
      if (trimmed === '') {
        return
      }
      recentReactions.value = [
        trimmed,
        ...recentReactions.value.filter((k) => k !== trimmed),
      ].slice(0, MAX_RECENT_REACTIONS)
    },
  }
}

/**
 * Reflect the theme onto `<html data-theme="…">`, where the CSS lives.
 * `system` removes the attribute so `prefers-color-scheme` decides.
 */
export function applyTheme(
  store: SettingsStore,
  root: HTMLElement,
): () => void {
  return effect(() => {
    if (store.theme.value === 'system') {
      root.removeAttribute('data-theme')
    } else {
      root.setAttribute('data-theme', store.theme.value)
    }
  })
}
