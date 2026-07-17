import { signal } from '@preact/signals'
import { createApiClient } from '../api/client'
import { createCompositeAuthProvider } from '../auth/composite'
import type { OAuthProviderConfig } from '../auth/oauth'
import {
  connectLiveRooms,
  connectLiveThreadUnread,
  connectLiveUnread,
  connectEphemeralPassthrough,
  connectReadMarkers,
  connectThreadReadMarkers,
  type AppServices,
} from '../services'
import { createAccountsStore } from '../stores/accounts'
import { createDeviceStateStore } from '../stores/device-state'
import { createEphemeralStore } from '../stores/ephemeral'
import { createMediaService } from '../media/media-service'
import { createLiveConnection } from '../stores/live-connection'
import { createRoomsStore } from '../stores/rooms'
import { createSearchStore } from '../stores/search'
import { createSettingsStore } from '../stores/settings'
import {
  createThreadUnreadStore,
  type ActiveThread,
} from '../stores/thread-unread'
import { createUnreadStore } from '../stores/unread'
import { FakeWebSocket } from './fake-socket'
import { memoryStorage } from './memory-storage'

/** msw handlers in component tests register against this origin. */
export const TEST_BASE_URL = 'http://axon.test'

/**
 * The real service graph over an in-memory storage and the msw base URL —
 * what `createServices` builds in production, minus the browser globals.
 */
export function testServices(
  options: {
    token?: string | null
    oauthProviders?: readonly OAuthProviderConfig[]
    pendingStorage?: Storage
    storage?: Storage
  } = {},
): AppServices & { sockets: FakeWebSocket[] } {
  const storage =
    options.storage ??
    memoryStorage(
      options.token === null
        ? {}
        : { 'axon.token': options.token ?? 'tok-test' },
    )
  const auth = createCompositeAuthProvider({
    providers: options.oauthProviders ?? [],
    baseUrl: TEST_BASE_URL,
    storage,
    // Isolate the session tier too: without this, session-mode token writes
    // (rememberMe=false) land in jsdom's real shared `window.sessionStorage`
    // and leak signed-in state across tests.
    sessionStorage: memoryStorage(),
    pendingStorage: options.pendingStorage ?? memoryStorage(),
  })
  const api = createApiClient(auth, TEST_BASE_URL)
  const media = createMediaService({ auth, baseUrl: TEST_BASE_URL })
  const settings = createSettingsStore(storage)
  const accounts = createAccountsStore(api)
  const rooms = createRoomsStore(api, storage)
  const search = createSearchStore(api)
  const unread = createUnreadStore()
  const threadUnread = createThreadUnreadStore()
  const ephemeral = createEphemeralStore()
  // An inert socket: `start()` builds one but nothing drives its handshake, so
  // no frames flow unless a test reaches for the fake and emits them. jsdom has
  // no real `WebSocket`, so this keeps the graph constructible everywhere.
  // Created sockets are collected on `sockets` so a test can drive frames:
  // `services.live.start(); services.sockets[0].emitOpen(); …emitMessage(…)`.
  const sockets: FakeWebSocket[] = []
  const live = createLiveConnection({
    socketFactory: () => {
      const socket = new FakeWebSocket()
      sockets.push(socket)
      return socket.asWebSocket()
    },
  })
  const activeRoom = signal<string | null>(null)
  const activeThread = signal<ActiveThread | null>(null)
  const composerFocus = signal(0)
  connectLiveUnread(live, unread, activeRoom)
  connectLiveRooms(live, rooms)
  connectLiveThreadUnread(live, rooms, accounts, threadUnread, activeThread)
  connectEphemeralPassthrough(live, ephemeral)
  const deviceState = createDeviceStateStore(api, live, storage)
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
    activeRoom,
    activeThread,
    composerFocus,
    sockets,
  }
}
