import {
  computed,
  signal,
  type ReadonlySignal,
  type Signal,
} from '@preact/signals'
import { apiErrorMessage, type ApiClient } from '../api/client'
import { markdownToPlainText } from '../markdown/markdown'
import {
  dmTitleFromMembers,
  isLikelyDm,
  memberDisplay,
  roomKey,
  roomTitle,
  type MemberDto,
  type RoomDto,
} from './room-list'
import type { EventDto } from './timeline'

export interface RoomPreview {
  eventId: string
  senderDisplay: string
  body: string
}

export interface RoomUnreadCounts {
  notificationCount: number
  highlightCount: number
}

export type RoomMembershipResult = { ok: true } | { ok: false; message: string }

export interface RoomsStore {
  rooms: ReadonlySignal<RoomDto[]>
  /** Rooms whose server-derived notification count is nonzero. */
  unreadKeys: ReadonlySignal<ReadonlySet<string>>
  /** True until the first load settles. */
  loading: ReadonlySignal<boolean>
  error: Signal<string | null>
  /**
   * Member-derived titles for unnamed rooms, keyed by room key. Read through
   * `roomTitle(room, titles.value)`.
   */
  titles: ReadonlySignal<ReadonlyMap<string, string>>

  /** Fetch the room list (server-side membership filtering per ADR 0037). */
  refresh(): Promise<void>
  /** Leave one room through M19b, then refresh authoritative room state. */
  leaveRoom(accountId: string, roomId: string): Promise<RoomMembershipResult>
  /** Forget one left/banned room through M19b, then refresh room state. */
  forgetRoom(accountId: string, roomId: string): Promise<RoomMembershipResult>
  /** Latest-message preview for one room. */
  preview(key: string): RoomPreview | undefined
  /** Server-derived unread notification count for one room. */
  unreadCount(key: string): number
  /** Fetch the latest-message preview for one room, if it is not cached. */
  hydratePreview(room: RoomDto): void
  /**
   * A live `timeline.event` for `(accountId, roomId)` at `ts` (M-W6 follow-up,
   * WCR-08): advance the room's `last_activity_ts` so the default "recent"
   * sort stays recent through a session, or — for a room not in the list —
   * trigger a refresh, which is how a newly joined room appears without a
   * reload.
   */
  noteActivity(accountId: string, roomId: string, ts: number): void
  /** Apply a live timeline event to room activity and preview state. */
  noteTimelineEvent(event: EventDto): void
  /** Apply a live or optimistic unread-count update for one room. */
  noteUnreadCounts(
    accountId: string,
    roomId: string,
    notificationCount: number,
    highlightCount: number,
  ): void
}

/** How many member-list fetches run at once when resolving DM titles. */
const TITLE_FETCH_CONCURRENCY = 4
const PREVIEW_PAGE_LIMIT = 50
const PREVIEW_MAX_PAGES = 3
const ROOM_TITLE_CACHE_KEY = 'axon.room_titles.v1'

/**
 * The cross-account room list (ADR 0046, M-W4). After each refresh, rooms
 * with no name/alias (likely DMs, ADR 0042 heuristic) get their titles
 * resolved from their member lists in the background — the TUI's
 * `request_unnamed_room_titles` behavior. Once cached, a title is not
 * re-fetched; the cache lives for the store's lifetime.
 */
export function createRoomsStore(
  api: ApiClient,
  storage: Storage = window.localStorage,
): RoomsStore {
  const rooms = signal<RoomDto[]>([])
  const unreadKeys = signal<ReadonlySet<string>>(new Set())
  const loading = signal(true)
  const error = signal<string | null>(null)
  const titles = signal<ReadonlyMap<string, string>>(loadTitleCache(storage))
  /** Keys with a fetch in flight or already settled (including "no title"). */
  const requested = new Set<string>()
  /** Preview cache keys: `${roomKey}\0${last_event_id}`. */
  const requestedPreviews = new Set<string>()
  /** One preview signal per room, so a live event wakes only its row. */
  const previewSlots = new Map<string, Signal<RoomPreview | undefined>>()
  /** One unread-count signal per room, so a count update wakes only its row. */
  const unreadSlots = new Map<string, Signal<RoomUnreadCounts>>()
  /** Member lists by `roomKey(room)`, shared by every preview for that room. */
  const members = new Map<string, Promise<MemberDto[]>>()
  /** Rooms this session successfully left/forgot before sync caught up. */
  const locallyHiddenRooms = new Set<string>()

  async function fetchTitle(room: RoomDto): Promise<void> {
    let data
    try {
      ;({ data } = await api.GET(
        '/v1/accounts/{account_id}/rooms/{room_id}/members',
        {
          params: {
            path: { account_id: room.account_id, room_id: room.room_id },
          },
        },
      ))
    } catch {
      // Network-level failure: a rejection here would otherwise fall out of
      // `resolveUnnamedTitles`'s fire-and-forget `Promise.all` unhandled
      // (WCR-02). Same policy as the envelope-error path below.
      return
    }
    if (data === undefined) {
      // Error envelope. Leave `requested` set: a failing room is not retried
      // this session (matching the TUI's per-room rate limiting in spirit);
      // the room id remains the fallback title.
      return
    }
    const title = dmTitleFromMembers(room.account_user_id, data.data)
    if (title !== null) {
      setCachedTitle(roomKey(room), title)
    }
  }

  function setCachedTitle(key: string, title: string): void {
    const trimmed = title.trim()
    if (trimmed === '') {
      return
    }
    const next = new Map(titles.value).set(key, trimmed)
    titles.value = next
    saveTitleCache(storage, next)
  }

  function cacheRoomListTitles(current: RoomDto[]): void {
    const next = new Map(titles.value)
    let changed = false
    for (const room of current) {
      if (!isLikelyDm(room)) {
        const title = roomTitle(room, next)
        if (title !== room.room_id && next.get(roomKey(room)) !== title) {
          next.set(roomKey(room), title)
          changed = true
        }
      }
    }
    if (changed) {
      titles.value = next
      saveTitleCache(storage, next)
    }
  }

  async function resolveUnnamedTitles(current: RoomDto[]): Promise<void> {
    const queue = current.filter(
      (room) => isLikelyDm(room) && !requested.has(roomKey(room)),
    )
    for (const room of queue) {
      requested.add(roomKey(room))
    }
    // Bounded parallelism: N workers draining one shared queue.
    let next = 0
    const worker = async () => {
      while (next < queue.length) {
        const room = queue[next]
        next += 1
        await fetchTitle(room)
      }
    }
    await Promise.all(
      Array.from({ length: TITLE_FETCH_CONCURRENCY }, () => worker()),
    )
  }

  /**
   * The room's member list, fetched once and cached for the store's lifetime.
   * Previews re-resolve on every new message; the names behind them do not, so
   * this must not become a `/members` round trip per inbound event.
   */
  function roomMembers(room: RoomDto): Promise<MemberDto[]> {
    const key = roomKey(room)
    const cached = members.get(key)
    if (cached !== undefined) {
      return cached
    }
    const pending = api
      .GET('/v1/accounts/{account_id}/rooms/{room_id}/members', {
        params: {
          path: { account_id: room.account_id, room_id: room.room_id },
        },
      })
      .then((result) => result.data?.data ?? [])
      .catch(() => {
        // Don't cache a failure: the next preview may as well try again.
        members.delete(key)
        return []
      })
    members.set(key, pending)
    return pending
  }

  function previewSlot(key: string): Signal<RoomPreview | undefined> {
    const existing = previewSlots.get(key)
    if (existing !== undefined) {
      return existing
    }
    const created = signal<RoomPreview | undefined>(undefined)
    previewSlots.set(key, created)
    return created
  }

  function unreadSlot(key: string): Signal<RoomUnreadCounts> {
    const existing = unreadSlots.get(key)
    if (existing !== undefined) {
      return existing
    }
    const created = signal<RoomUnreadCounts>({
      notificationCount: 0,
      highlightCount: 0,
    })
    unreadSlots.set(key, created)
    return created
  }

  function setUnreadCounts(
    key: string,
    notificationCount: number,
    highlightCount: number,
    nextUnreadKeys?: Set<string>,
  ): void {
    const slot = unreadSlot(key)
    const current = slot.value
    if (
      current.notificationCount !== notificationCount ||
      current.highlightCount !== highlightCount
    ) {
      slot.value = { notificationCount, highlightCount }
    }
    if (nextUnreadKeys !== undefined) {
      if (notificationCount > 0) {
        nextUnreadKeys.add(key)
      }
      return
    }
    const isUnread = unreadKeys.value.has(key)
    if (notificationCount > 0 === isUnread) {
      return
    }
    const next = new Set(unreadKeys.value)
    if (notificationCount > 0) {
      next.add(key)
    } else {
      next.delete(key)
    }
    unreadKeys.value = next
  }

  function syncUnreadCounts(current: RoomDto[]): void {
    const nextUnreadKeys = new Set<string>()
    const liveKeys = new Set<string>()
    for (const room of current) {
      const key = roomKey(room)
      liveKeys.add(key)
      setUnreadCounts(
        key,
        countFromRoom(room.notification_count),
        countFromRoom(room.highlight_count),
        nextUnreadKeys,
      )
    }
    for (const key of unreadSlots.keys()) {
      if (!liveKeys.has(key)) {
        setUnreadCounts(key, 0, 0, nextUnreadKeys)
      }
    }
    unreadKeys.value = nextUnreadKeys
  }

  function setPreview(key: string, preview: RoomPreview): void {
    const slot = previewSlot(key)
    const current = slot.value
    if (
      current !== undefined &&
      current.eventId === preview.eventId &&
      current.senderDisplay === preview.senderDisplay &&
      current.body === preview.body
    ) {
      return
    }
    slot.value = preview
  }

  async function fetchPreview(room: RoomDto, eventId: string): Promise<void> {
    let event: EventDto | undefined
    let roster: MemberDto[]
    try {
      ;[event, roster] = await Promise.all([
        fetchLatestPreviewEvent(room),
        roomMembers(room),
      ])
    } catch {
      return
    }
    const body = previewBody(event?.body)
    if (event === undefined || body === undefined || body === '') {
      return
    }
    const sender = event.sender
    const member = roster.find((candidate) => candidate.user_id === sender)
    const preview: RoomPreview = {
      eventId,
      senderDisplay: member === undefined ? sender : memberDisplay(member),
      body,
    }
    setPreview(roomKey(room), preview)
  }

  function setLivePreview(room: RoomDto, event: EventDto): void {
    const body = previewBody(event.body)
    if (!isPreviewEvent(event) || body === undefined || body === '') {
      return
    }
    const key = roomKey(room)
    setPreview(key, {
      eventId: event.event_id,
      senderDisplay: event.sender,
      body,
    })
    void roomMembers(room).then((roster) => {
      const member = roster.find(
        (candidate) => candidate.user_id === event.sender,
      )
      if (member === undefined) {
        return
      }
      setPreview(key, {
        eventId: event.event_id,
        senderDisplay: memberDisplay(member),
        body,
      })
    })
  }

  async function fetchLatestPreviewEvent(
    room: RoomDto,
  ): Promise<EventDto | undefined> {
    let cursor: string | undefined
    for (let page = 0; page < PREVIEW_MAX_PAGES; page += 1) {
      const { data } = await api.GET(
        '/v1/accounts/{account_id}/rooms/{room_id}/timeline',
        {
          params: {
            path: { account_id: room.account_id, room_id: room.room_id },
            query: { limit: PREVIEW_PAGE_LIMIT, cursor },
          },
        },
      )
      if (data === undefined) {
        return undefined
      }
      const event = data.data.events.find(isPreviewEvent)
      if (event !== undefined) {
        return event
      }
      cursor = data.data.next_cursor ?? undefined
      if (cursor === undefined) {
        return undefined
      }
    }
    return undefined
  }

  function hydratePreview(room: RoomDto): void {
    const eventId = room.last_event_id
    if (eventId === null || eventId === undefined || eventId === '') {
      return
    }
    const key = `${roomKey(room)}\0${eventId}`
    if (requestedPreviews.has(key)) {
      return
    }
    requestedPreviews.add(key)
    void fetchPreview(room, eventId)
  }

  /** The freshest row, so `noteActivity` can skip the common burst case. */
  let newestKey: string | null = null
  let newestTs = -Infinity

  function recomputeNewest(current: RoomDto[]): void {
    newestKey = null
    newestTs = -Infinity
    for (const room of current) {
      if (room.last_activity_ts > newestTs) {
        newestTs = room.last_activity_ts
        newestKey = roomKey(room)
      }
    }
  }

  async function doRefresh(): Promise<void> {
    try {
      const { data, error: apiError } = await api.GET('/v1/rooms')
      if (apiError !== undefined) {
        error.value = apiErrorMessage(apiError)
        return
      }
      const visibleRooms = data.data.filter(
        (room) => !locallyHiddenRooms.has(roomKey(room)),
      )
      rooms.value = visibleRooms
      syncUnreadCounts(visibleRooms)
      cacheRoomListTitles(visibleRooms)
      recomputeNewest(visibleRooms)
      error.value = null
      // Fire-and-forget: titles arrive incrementally; the list re-renders as
      // the titles signal updates.
      void resolveUnnamedTitles(visibleRooms)
    } catch (cause) {
      error.value = cause instanceof Error ? cause.message : String(cause)
    } finally {
      loading.value = false
    }
  }

  /** In-flight refresh, shared: concurrent callers (mount + a burst of
   *  unknown-room frames) coalesce onto one request. */
  let refreshing: Promise<void> | null = null

  function refresh(): Promise<void> {
    refreshing ??= doRefresh().finally(() => {
      refreshing = null
    })
    return refreshing
  }

  async function membershipMutation(
    accountId: string,
    roomId: string,
    call: () => Promise<{ error?: unknown }>,
  ): Promise<RoomMembershipResult> {
    try {
      const { error: apiError } = await call()
      if (apiError !== undefined) {
        const message = apiErrorMessage(apiError)
        error.value = message
        return { ok: false, message }
      }
      const hiddenKey = hideRoomLocally(accountId, roomId)
      try {
        await refresh()
      } finally {
        locallyHiddenRooms.delete(hiddenKey)
      }
      return { ok: true }
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause)
      error.value = message
      return { ok: false, message }
    }
  }

  function leaveRoom(
    accountId: string,
    roomId: string,
  ): Promise<RoomMembershipResult> {
    return membershipMutation(accountId, roomId, () =>
      api.POST('/v1/accounts/{account_id}/rooms/{room_id}/leave', {
        params: { path: { account_id: accountId, room_id: roomId } },
      }),
    )
  }

  function forgetRoom(
    accountId: string,
    roomId: string,
  ): Promise<RoomMembershipResult> {
    return membershipMutation(accountId, roomId, () =>
      api.POST('/v1/accounts/{account_id}/rooms/{room_id}/forget', {
        params: { path: { account_id: accountId, room_id: roomId } },
      }),
    )
  }

  function hideRoomLocally(accountId: string, roomId: string): string {
    const key = roomKey({ account_id: accountId, room_id: roomId })
    locallyHiddenRooms.add(key)
    const next = rooms.value.filter((room) => roomKey(room) !== key)
    if (next.length !== rooms.value.length) {
      rooms.value = next
      syncUnreadCounts(next)
      recomputeNewest(next)
    }
    return key
  }

  function noteActivityRoom(
    accountId: string,
    roomId: string,
    ts: number,
    eventId?: string,
  ): RoomDto | null {
    const key = roomKey({ account_id: accountId, room_id: roomId })
    const current = rooms.value
    const index = current.findIndex(
      (room) => room.account_id === accountId && room.room_id === roomId,
    )
    if (index === -1) {
      // A room we don't know: freshly joined (or the list predates it).
      // Re-read the list; the in-flight guard coalesces frame bursts.
      void refresh()
      return null
    }
    const room = current[index]
    // `<`, not `<=`: an event whose origin_ts ties the recorded activity (a
    // same-millisecond burst, or a frame racing a refresh that already
    // recorded its ts) must still update the preview/last_event_id.
    if (ts < room.last_activity_ts) {
      return null
    }
    if (key === newestKey) {
      // Already the freshest row: the sort order cannot change, so mutate in
      // place without a signal write — a burst in the active room must not
      // re-render and re-sort the whole list per message (the room-list-perf
      // rule). The row's relative-time label catches up on the next render.
      room.last_activity_ts = ts
      if (eventId !== undefined) {
        room.last_event_id = eventId
      }
      newestTs = ts
      return room
    }
    const nextRoom =
      eventId === undefined
        ? { ...room, last_activity_ts: ts }
        : { ...room, last_activity_ts: ts, last_event_id: eventId }
    rooms.value = current.map((r, i) => (i === index ? nextRoom : r))
    if (ts > newestTs) {
      newestTs = ts
      newestKey = key
    }
    return nextRoom
  }

  function noteActivity(accountId: string, roomId: string, ts: number): void {
    noteActivityRoom(accountId, roomId, ts)
  }

  function noteTimelineEvent(event: EventDto): void {
    const room = noteActivityRoom(
      event.account_id,
      event.room_id,
      event.origin_ts,
      event.event_id,
    )
    if (room !== null) {
      setLivePreview(room, event)
    }
  }

  function noteUnreadCounts(
    accountId: string,
    roomId: string,
    notificationCount: number,
    highlightCount: number,
  ): void {
    const key = roomKey({ account_id: accountId, room_id: roomId })
    const known = rooms.value.some(
      (room) => room.account_id === accountId && room.room_id === roomId,
    )
    if (!known) {
      void refresh()
    }
    setUnreadCounts(
      key,
      countFromRoom(notificationCount),
      countFromRoom(highlightCount),
    )
  }

  return {
    rooms: computed(() => rooms.value),
    unreadKeys: computed(() => unreadKeys.value),
    loading: computed(() => loading.value),
    error,
    titles: computed(() => titles.value),
    refresh,
    leaveRoom,
    forgetRoom,
    preview: (key) => previewSlot(key).value,
    unreadCount: (key) => unreadSlot(key).value.notificationCount,
    hydratePreview,
    noteActivity,
    noteTimelineEvent,
    noteUnreadCounts,
  }
}

function countFromRoom(value: number | null | undefined): number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value > 0
    ? value
    : 0
}

function loadTitleCache(storage: Storage): ReadonlyMap<string, string> {
  const raw = storage.getItem(ROOM_TITLE_CACHE_KEY)
  if (raw === null) {
    return new Map()
  }
  try {
    const parsed = JSON.parse(raw) as unknown
    if (
      parsed === null ||
      typeof parsed !== 'object' ||
      !Array.isArray((parsed as { titles?: unknown }).titles)
    ) {
      return new Map()
    }
    const entries = (parsed as { titles: unknown[] }).titles.flatMap(
      (entry): [string, string][] => {
        if (
          Array.isArray(entry) &&
          entry.length === 2 &&
          typeof entry[0] === 'string' &&
          typeof entry[1] === 'string' &&
          entry[1].trim() !== ''
        ) {
          return [[entry[0], entry[1].trim()]]
        }
        return []
      },
    )
    return new Map(entries)
  } catch {
    return new Map()
  }
}

function saveTitleCache(
  storage: Storage,
  titleCache: ReadonlyMap<string, string>,
): void {
  try {
    storage.setItem(
      ROOM_TITLE_CACHE_KEY,
      JSON.stringify({ version: 1, titles: [...titleCache] }),
    )
  } catch {
    // Quota or storage-denied: the cache is an optimization — losing a write
    // must not reject the background title resolution (WCR-02) or paint an
    // error banner over a successfully loaded room list.
  }
}

/**
 * A message the preview can actually show. A redacted or bodiless row (a
 * removed message, an image) is *skipped*, not previewed as blank: `body` is
 * null there, and `body?.trim() !== ''` would wave it through as the newest
 * message and leave the room with no preview at all.
 */
function isPreviewEvent(event: EventDto): boolean {
  return (
    event.type === 'm.room.message' &&
    event.state_key == null &&
    !event.redacted &&
    typeof event.body === 'string' &&
    event.body.trim() !== ''
  )
}

function previewBody(body: string | null | undefined): string | undefined {
  if (body === null || body === undefined) {
    return undefined
  }
  const text = markdownToPlainText(body)
  return text === '' ? undefined : text
}
