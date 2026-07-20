import { effect, signal, type Signal } from '@preact/signals'
import { createContext } from 'preact'
import { useContext } from 'preact/hooks'
import { createApiClient, type ApiClient } from './api/client'
import {
  deviceStateChange,
  ephemeralPassthrough,
  timelineEvent,
} from './api/frames'
import { openLiveSocket } from './api/ws'
import {
  createCompositeAuthProvider,
  type CompositeAuthProvider,
} from './auth/composite'
import { parseOAuthProviders } from './auth/oauth'
import { createAccountsStore, type AccountsStore } from './stores/accounts'
import {
  createDeviceStateStore,
  parseThreadReadMarker,
  READ_MARKERS_NAMESPACE,
  THREAD_READ_MARKERS_NAMESPACE,
  type DeviceStateStore,
} from './stores/device-state'
import { createEphemeralStore, type EphemeralStore } from './stores/ephemeral'
import {
  createEphemeralSender,
  type EphemeralSender,
} from './stores/ephemeral-sender'
import { createMediaService, type MediaService } from './media/media-service'
import {
  createLiveConnection,
  type LiveConnection,
} from './stores/live-connection'
import { roomKey, roomTitle } from './stores/room-list'
import { createRoomsStore, type RoomsStore } from './stores/rooms'
import { createSearchStore, type SearchStore } from './stores/search'
import { createSettingsStore, type SettingsStore } from './stores/settings'
import {
  createThreadUnreadStore,
  type ActiveThread,
  type ThreadUnreadStore,
} from './stores/thread-unread'
import { isRoomUnreadEvent, type EventDto } from './stores/timeline'
import { createUnreadStore, type UnreadStore } from './stores/unread'

/**
 * The app's service graph — auth seam, API client, and stores — built once at
 * startup and provided via context. Tests build the same graph over msw and
 * an in-memory storage; components never construct services themselves.
 */
export interface AppServices {
  auth: CompositeAuthProvider
  api: ApiClient
  settings: SettingsStore
  accounts: AccountsStore
  rooms: RoomsStore
  search: SearchStore
  unread: UnreadStore
  threadUnread: ThreadUnreadStore
  live: LiveConnection
  deviceState: DeviceStateStore
  ephemeral: EphemeralStore
  /** Outbound read receipts + typing notices to the homeserver (ADR 0067/0068). */
  ephemeralSender: EphemeralSender
  media: MediaService
  /**
   * The room the user is currently viewing (`accountId/roomId`), or `null`.
   * Set by `RoomPage`; the live unread feed reads it so a message arriving in
   * the open room does not raise its own badge (it is already being seen).
   */
  activeRoom: Signal<string | null>
  /** The thread panel currently open, so live replies there do not badge it. */
  activeThread: Signal<ActiveThread | null>
  /**
   * Bumped to pull focus back to the open room's composer (ADR 0063) — the
   * staged Escape's last stage, and where the room list's Escape lands. A
   * counter rather than a method so the composer can react to it as a signal;
   * the thread panel's composer deliberately ignores it.
   */
  composerFocus: Signal<number>
}

/**
 * `VITE_AXON_SERVER_URL` bakes a cross-origin server base into the bundle for
 * separately-hosted deployments (which rely on the server's CORS allow-list,
 * M-W1.5). Unset, the client is same-origin: the Vite dev proxy in
 * development, or a reverse proxy serving app and API together.
 */
function apiBaseUrl(): string {
  return import.meta.env.VITE_AXON_SERVER_URL ?? '/'
}

function oauthClientId(): string {
  return import.meta.env.VITE_AXON_OAUTH_CLIENT_ID ?? 'axon-web'
}

function roomAlreadyUnread(
  event: EventDto,
  key: string,
  unread: UnreadStore,
  rooms: RoomsStore,
  deviceState: DeviceStateStore,
): boolean {
  if (unread.count(key) > 0) {
    return true
  }
  const marker = deviceState.readMarker(event.account_id, event.room_id)
  if (marker === null) {
    return false
  }
  const room = rooms.rooms.value.find(
    (candidate) =>
      candidate.account_id === event.account_id &&
      candidate.room_id === event.room_id,
  )
  return room?.last_event_id != null && room.last_activity_ts > marker.originTs
}

/**
 * Feed live timeline events into the unread store (M-W6, ADR 0046): every
 * unread-worthy message for a room other than the open one raises that room's
 * client-derived badge. Reactions, redactions, edits, state events, and other
 * non-visible rows are skipped. When such an ignored event lands in a clean
 * room, the read marker advances so a reload does not derive a stale unread dot
 * from `last_activity_ts`; an already-unread room is left unread.
 */
export function connectLiveUnread(
  live: LiveConnection,
  unread: UnreadStore,
  activeRoom: Signal<string | null>,
  rooms: RoomsStore,
  accounts: AccountsStore,
  deviceState: DeviceStateStore,
): () => void {
  return live.subscribe((frame) => {
    const event = timelineEvent(frame)
    if (event === null) {
      return
    }
    const key = roomKey(event)
    if (key === activeRoom.value) {
      return
    }
    if (!isRoomUnreadEvent(event)) {
      if (!roomAlreadyUnread(event, key, unread, rooms, deviceState)) {
        deviceState.advanceReadMarker(
          event.account_id,
          event.room_id,
          event.event_id,
          event.origin_ts,
        )
      }
      return
    }
    const ownUserId =
      accounts.accounts.value.find(
        (account) => account.account_id === event.account_id,
      )?.user_id ??
      rooms.rooms.value.find(
        (room) =>
          room.account_id === event.account_id &&
          room.room_id === event.room_id,
      )?.account_user_id ??
      null
    if (ownUserId !== null && event.sender === ownUserId) {
      unread.markSeen(key)
      deviceState.advanceReadMarker(
        event.account_id,
        event.room_id,
        event.event_id,
        event.origin_ts,
      )
      return
    }
    unread.recordEvent(key)
  })
}

/**
 * Feed live thread replies into the unread-thread store. The open thread is
 * skipped: `ThreadPanel` is the read surface for hidden replies, matching the
 * TUI's unread-thread semantics.
 */
export function connectLiveThreadUnread(
  live: LiveConnection,
  rooms: RoomsStore,
  accounts: AccountsStore,
  threadUnread: ThreadUnreadStore,
  activeThread: Signal<ActiveThread | null>,
): () => void {
  return live.subscribe((frame) => {
    const event = timelineEvent(frame)
    if (event === null) {
      return
    }
    const room = rooms.rooms.value.find(
      (candidate) =>
        candidate.account_id === event.account_id &&
        candidate.room_id === event.room_id,
    )
    // Two sources for "which user is me on this account": the accounts store
    // (keyed by the frame's account_id) with the room row as fallback. With
    // neither loaded yet the sender can't be attributed, and an unattributable
    // reply must not badge — it could be the user's own send from another
    // device; a genuinely-unread thread resurfaces on the next reply or via
    // summary reconciliation.
    const ownUserId =
      accounts.accounts.value.find((a) => a.account_id === event.account_id)
        ?.user_id ??
      room?.account_user_id ??
      null
    if (ownUserId === null) {
      return
    }
    threadUnread.recordLiveEvent(event, {
      roomTitle:
        room === undefined
          ? event.room_id
          : roomTitle(room, rooms.titles.value),
      ownUserId,
      activeThread: activeThread.value,
    })
  })
}

/**
 * Clear a room's client-derived unread badge when a *sibling* device reports
 * reading it (M-W6 step 5c, ADR 0048): a `read_markers` `device_state.changed`
 * frame from another device means the user has seen that room elsewhere. Our
 * own reads already `markSeen` locally, so our echoes are skipped. Returns the
 * unsubscribe; the app graph keeps the subscription for its life.
 */
export function connectReadMarkers(
  live: LiveConnection,
  unread: UnreadStore,
  deviceState: DeviceStateStore,
): () => void {
  return live.subscribe((frame) => {
    const change = deviceStateChange(frame)
    if (
      change === null ||
      change.namespace !== READ_MARKERS_NAMESPACE ||
      change.deviceId === deviceState.deviceId
    ) {
      return
    }
    for (const [roomId, value] of Object.entries(change.entries)) {
      if (value !== null) {
        unread.markSeen(
          roomKey({ account_id: frame.accountId, room_id: roomId }),
        )
      }
    }
  })
}

/** Apply sibling devices' thread-read markers to local unread-thread state. */
export function connectThreadReadMarkers(
  live: LiveConnection,
  threadUnread: ThreadUnreadStore,
  deviceState: DeviceStateStore,
): () => void {
  return live.subscribe((frame) => {
    const change = deviceStateChange(frame)
    if (
      change === null ||
      change.namespace !== THREAD_READ_MARKERS_NAMESPACE ||
      change.deviceId === deviceState.deviceId
    ) {
      return
    }
    for (const value of Object.values(change.entries)) {
      const marker = parseThreadReadMarker(value)
      if (marker !== null) {
        threadUnread.markThreadRead(
          frame.accountId,
          marker.roomId,
          marker.rootEventId,
        )
      }
    }
  })
}

/**
 * Keep the room list live (M-W6 follow-up, WCR-08): every `timeline.event`
 * frame advances its room's `last_activity_ts` and latest-message preview (so
 * the default "recent" sort and optional preview stay recent through a
 * session; an unknown room triggers a list re-read — how a newly joined room
 * appears), and a reconnect re-reads the list outright — the bus is lossy, and
 * joins/renames missed while disconnected have no other repair path. Returns
 * the unsubscribe; the app graph keeps the subscription for its life.
 */
export function connectLiveRooms(
  live: LiveConnection,
  rooms: RoomsStore,
): () => void {
  const unsubscribe = live.subscribe((frame) => {
    const event = timelineEvent(frame)
    if (event === null) {
      return
    }
    rooms.noteTimelineEvent(event)
  })
  const dispose = effect(() => {
    if (live.reconnects.value === 0) {
      return
    }
    void rooms.refresh()
  })
  return () => {
    unsubscribe()
    dispose()
  }
}

/** Route raw Matrix ephemeral passthrough frames into the web overlay store. */
export function connectEphemeralPassthrough(
  live: LiveConnection,
  ephemeral: EphemeralStore,
): () => void {
  const unsubscribe = live.subscribe((frame) => {
    const event = ephemeralPassthrough(frame)
    if (event !== null) {
      ephemeral.apply(frame.accountId, event)
    }
  })
  const dispose = effect(() => {
    if (live.reconnects.value > 0 || live.connection.value !== 'live') {
      ephemeral.clearTyping()
    }
  })
  return () => {
    unsubscribe()
    dispose()
  }
}

export function createServices(
  storage: Storage = window.localStorage,
  sessionStorage: Storage = window.sessionStorage,
): AppServices {
  const auth = createCompositeAuthProvider({
    providers: parseOAuthProviders(import.meta.env.VITE_AXON_OAUTH_PROVIDERS),
    baseUrl: apiBaseUrl(),
    clientId: oauthClientId(),
    storage,
    sessionStorage,
  })
  const api = createApiClient(auth, apiBaseUrl())
  const media = createMediaService({ auth, baseUrl: apiBaseUrl() })
  const settings = createSettingsStore(storage)
  const accounts = createAccountsStore(api)
  const rooms = createRoomsStore(api, storage)
  const search = createSearchStore(api)
  const unread = createUnreadStore()
  const threadUnread = createThreadUnreadStore()
  const ephemeral = createEphemeralStore()
  const ephemeralSender = createEphemeralSender(api)
  // Same-origin by default (the dev/reverse proxy); a cross-origin base is set
  // for separately-hosted deployments, exactly as the HTTP client resolves it.
  const live = createLiveConnection({
    socketFactory: () => {
      // A WebSocket bakes the token into its subprotocols at construction, so
      // the token must be available synchronously. Token-paste (M-W6) is sync;
      // an async provider (the Tauri keychain, M-W12) needs an async open path
      // that does not exist yet — until then it degrades to `offline` here.
      const token = auth.getToken()
      if (typeof token !== 'string') {
        throw new Error('live socket requires a synchronously available token')
      }
      return openLiveSocket(token, import.meta.env.VITE_AXON_SERVER_URL)
    },
  })
  const activeRoom = signal<string | null>(null)
  const activeThread = signal<ActiveThread | null>(null)
  const composerFocus = signal(0)
  const deviceState = createDeviceStateStore(api, live, storage)
  connectLiveUnread(live, unread, activeRoom, rooms, accounts, deviceState)
  connectLiveRooms(live, rooms)
  connectLiveThreadUnread(live, rooms, accounts, threadUnread, activeThread)
  connectEphemeralPassthrough(live, ephemeral)
  connectReadMarkers(live, unread, deviceState)
  connectThreadReadMarkers(live, threadUnread, deviceState)
  return {
    auth,
    api,
    media,
    settings,
    accounts,
    rooms,
    search,
    unread,
    threadUnread,
    live,
    deviceState,
    ephemeral,
    ephemeralSender,
    activeRoom,
    activeThread,
    composerFocus,
  }
}

export const ServicesContext = createContext<AppServices | null>(null)

export function useServices(): AppServices {
  const services = useContext(ServicesContext)
  if (services === null) {
    throw new Error('useServices called outside <ServicesContext.Provider>')
  }
  return services
}
