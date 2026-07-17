import { computed, signal } from '@preact/signals'
import { describe, expect, it, vi } from 'vitest'
import { createApiClient } from './api/client'
import { TIMELINE_EVENT } from './api/frames'
import {
  connectLiveRooms,
  connectLiveUnread,
  connectReadMarkers,
} from './services'
import { createDeviceStateStore } from './stores/device-state'
import { createLiveConnection } from './stores/live-connection'
import { createUnreadStore } from './stores/unread'
import { FakeWebSocket } from './test/fake-socket'
import { memoryStorage } from './test/memory-storage'

const ACCT = '11111111-1111-1111-1111-111111111111'
const ROOM = '!room:server'
const KEY = `${ACCT}/${ROOM}`

function harness() {
  let socket: FakeWebSocket | undefined
  const live = createLiveConnection({
    socketFactory: () => {
      socket = new FakeWebSocket()
      return socket.asWebSocket()
    },
  })
  const unread = createUnreadStore()
  const activeRoom = signal<string | null>(null)
  connectLiveUnread(live, unread, activeRoom)
  live.start()
  return { live, unread, activeRoom, socket: () => socket! }
}

const timelineFrame = (accountId: string, roomId: string) =>
  JSON.stringify({
    type: TIMELINE_EVENT,
    account_id: accountId,
    payload: { event_id: '$e', account_id: accountId, room_id: roomId },
  })

describe('connectLiveUnread', () => {
  it('raises the badge for a live event in a room that is not open', () => {
    const { unread, socket } = harness()
    socket().emitMessage(timelineFrame(ACCT, ROOM))
    expect(unread.count(KEY)).toBe(1)
  })

  it('does not badge the room the user is currently viewing', () => {
    const { unread, activeRoom, socket } = harness()
    activeRoom.value = KEY
    socket().emitMessage(timelineFrame(ACCT, ROOM))
    expect(unread.count(KEY)).toBe(0)
  })

  it('ignores non-timeline frames', () => {
    const { unread, socket } = harness()
    socket().emitMessage(
      JSON.stringify({
        type: 'device_state.changed',
        account_id: ACCT,
        payload: {},
      }),
    )
    expect(unread.count(KEY)).toBe(0)
  })
})

function readMarkerHarness() {
  let socket: FakeWebSocket | undefined
  const live = createLiveConnection({
    socketFactory: () => {
      socket = new FakeWebSocket()
      return socket.asWebSocket()
    },
  })
  const unread = createUnreadStore()
  const api = createApiClient(
    {
      getToken: () => 't',
      onAuthFailure: () => {},
      LoginBootstrap: () => null,
    },
    'http://axon.test',
  )
  const deviceState = createDeviceStateStore(api, live, memoryStorage())
  connectReadMarkers(live, unread, deviceState)
  live.start()
  return { live, unread, deviceState, socket: () => socket! }
}

const readMarkerFrame = (deviceId: string, entries: Record<string, unknown>) =>
  JSON.stringify({
    type: 'device_state.changed',
    account_id: ACCT,
    payload: {
      device_id: deviceId,
      namespace: 'read_markers',
      entries,
      updated_at: '2026-01-01T00:00:00Z',
    },
  })

describe('connectReadMarkers', () => {
  it('clears unread when a sibling device reads a room', () => {
    const { unread, socket } = readMarkerHarness()
    unread.recordEvent(KEY)
    expect(unread.count(KEY)).toBe(1)
    socket().emitMessage(
      readMarkerFrame('sibling', { [ROOM]: { event_id: '$e', origin_ts: 1 } }),
    )
    expect(unread.count(KEY)).toBe(0)
  })

  it('ignores our own read-marker echo', () => {
    const { unread, deviceState, socket } = readMarkerHarness()
    unread.recordEvent(KEY)
    socket().emitMessage(
      readMarkerFrame(deviceState.deviceId, {
        [ROOM]: { event_id: '$e', origin_ts: 1 },
      }),
    )
    expect(unread.count(KEY)).toBe(1)
  })

  it('ignores drafts frames', () => {
    const { unread, socket } = readMarkerHarness()
    unread.recordEvent(KEY)
    socket().emitMessage(
      JSON.stringify({
        type: 'device_state.changed',
        account_id: ACCT,
        payload: {
          device_id: 'sibling',
          namespace: 'drafts',
          entries: { [ROOM]: { text: 'hi' } },
          updated_at: '2026-01-01T00:00:00Z',
        },
      }),
    )
    expect(unread.count(KEY)).toBe(1)
  })
})

function roomsStub() {
  const noted: [string, string, number][] = []
  const liveEvents: unknown[] = []
  let refreshes = 0
  const empty = signal<never[]>([])
  const stub = {
    rooms: computed(() => empty.value),
    loading: computed(() => false),
    error: signal<string | null>(null),
    titles: computed(() => new Map<string, string>()),
    refresh: () => {
      refreshes += 1
      return Promise.resolve()
    },
    preview: () => undefined,
    hydratePreview: () => {},
    noteActivity: (accountId: string, roomId: string, ts: number) => {
      noted.push([accountId, roomId, ts])
    },
    noteTimelineEvent: (event: unknown) => {
      liveEvents.push(event)
      if (
        typeof event === 'object' &&
        event !== null &&
        'account_id' in event &&
        'room_id' in event &&
        'origin_ts' in event
      ) {
        noted.push([
          (event as { account_id: string }).account_id,
          (event as { room_id: string }).room_id,
          (event as { origin_ts: number }).origin_ts,
        ])
      }
    },
  }
  return { stub, noted, liveEvents, refreshCount: () => refreshes }
}

describe('connectLiveRooms', () => {
  it('feeds timeline frames into noteActivity and ignores other kinds', () => {
    let socket: FakeWebSocket | undefined
    const live = createLiveConnection({
      socketFactory: () => {
        socket = new FakeWebSocket()
        return socket.asWebSocket()
      },
    })
    const { stub, noted, liveEvents } = roomsStub()
    connectLiveRooms(live, stub)
    live.start()

    socket!.emitMessage(
      JSON.stringify({
        type: TIMELINE_EVENT,
        account_id: ACCT,
        payload: {
          event_id: '$e',
          account_id: ACCT,
          room_id: ROOM,
          origin_ts: 123,
        },
      }),
    )
    expect(noted).toEqual([[ACCT, ROOM, 123]])
    expect(liveEvents).toHaveLength(1)

    socket!.emitMessage(
      JSON.stringify({
        type: 'device_state.changed',
        account_id: ACCT,
        payload: {},
      }),
    )
    expect(noted).toHaveLength(1)
  })

  it('re-reads the list on reconnect, not on the first connect (WCR-08)', () => {
    vi.useFakeTimers()
    const sockets: FakeWebSocket[] = []
    const live = createLiveConnection({
      socketFactory: () => {
        const s = new FakeWebSocket()
        sockets.push(s)
        return s.asWebSocket()
      },
    })
    const { stub, refreshCount } = roomsStub()
    connectLiveRooms(live, stub)
    live.start()

    sockets[0].emitOpen()
    expect(refreshCount()).toBe(0) // first connect is not a reconnect

    sockets[0].emitClose()
    vi.advanceTimersByTime(1000) // initial backoff
    sockets[1].emitOpen()
    expect(refreshCount()).toBe(1)
    vi.useRealTimers()
  })
})
