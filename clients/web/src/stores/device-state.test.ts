import { HttpResponse, http } from 'msw'
import { setupServer } from 'msw/node'
import {
  afterAll,
  afterEach,
  beforeAll,
  describe,
  expect,
  it,
  vi,
} from 'vitest'
import { createApiClient } from '../api/client'
import { FakeWebSocket } from '../test/fake-socket'
import { memoryStorage } from '../test/memory-storage'
import { createDeviceStateStore } from './device-state'
import { createLiveConnection } from './live-connection'

const BASE_URL = 'http://axon.test'
const ACCT = '6b53f7f0-0000-4000-8000-000000000001'
const ROOM = '!room:hs'
const ROOT = '$root:hs'
const STATE_PATH = `${BASE_URL}/v1/devices/:deviceId/state/:namespace`

const server = setupServer()
beforeAll(() => server.listen({ onUnhandledRequest: 'error' }))
afterEach(() => server.resetHandlers())
afterAll(() => server.close())

function makeApi() {
  return createApiClient(
    {
      getToken: () => 'tok',
      onAuthFailure: () => {},
      LoginBootstrap: () => null,
    },
    BASE_URL,
  )
}

function setup(storage = memoryStorage()) {
  let socket: FakeWebSocket
  const live = createLiveConnection({
    socketFactory: () => {
      socket = new FakeWebSocket()
      return socket.asWebSocket()
    },
  })
  const store = createDeviceStateStore(makeApi(), live, storage)
  return { store, live, socket: () => socket, storage }
}

const draftsFrame = (deviceId: string, entries: Record<string, unknown>) =>
  JSON.stringify({
    type: 'device_state.changed',
    account_id: ACCT,
    payload: {
      device_id: deviceId,
      namespace: 'drafts',
      entries,
      updated_at: '2026-01-01T00:00:00Z',
    },
  })

describe('createDeviceStateStore', () => {
  it('mints a device id once and persists it across instances', () => {
    const storage = memoryStorage()
    const first = setup(storage).store.deviceId
    const second = setup(storage).store.deviceId
    expect(first).toBe(second)
    expect(storage.getItem('axon.device_id')).toBe(first)
  })

  it('hydrates a draft from the merged GET (account-scoped)', async () => {
    server.use(
      http.get(STATE_PATH, ({ params, request }) => {
        expect(params.namespace).toBe('drafts')
        expect(new URL(request.url).searchParams.get('account_id')).toBe(ACCT)
        return HttpResponse.json({
          data: {
            namespace: 'drafts',
            entries: {
              [ROOM]: {
                value: { text: 'hello' },
                device_id: 'sibling',
                updated_at: '2026-01-01T00:00:00Z',
              },
            },
          },
        })
      }),
    )
    const { store } = setup()
    expect(store.draft(ACCT, ROOM)).toBe('')
    store.hydrateDrafts(ACCT)
    await vi.waitFor(() => expect(store.draft(ACCT, ROOM)).toBe('hello'))
  })

  it('setDraft updates locally at once, then PUTs the merge after debounce', async () => {
    vi.useFakeTimers()
    let body: unknown
    server.use(
      http.put(STATE_PATH, async ({ request }) => {
        body = await request.json()
        return HttpResponse.json({
          data: { updated_at: '2026-01-01T00:00:00Z' },
        })
      }),
    )
    const { store } = setup()
    store.setDraft(ACCT, ROOM, 'draft text')
    expect(store.draft(ACCT, ROOM)).toBe('draft text')
    expect(body).toBeUndefined()

    await vi.advanceTimersByTimeAsync(800)
    expect(body).toEqual({ entries: { [ROOM]: { text: 'draft text' } } })
    vi.useRealTimers()
  })

  it('clearing a draft writes a null tombstone', async () => {
    vi.useFakeTimers()
    let body: unknown
    server.use(
      http.put(STATE_PATH, async ({ request }) => {
        body = await request.json()
        return HttpResponse.json({
          data: { updated_at: '2026-01-01T00:00:00Z' },
        })
      }),
    )
    const { store } = setup()
    store.setDraft(ACCT, ROOM, '')
    expect(store.draft(ACCT, ROOM)).toBe('')
    await vi.advanceTimersByTimeAsync(800)
    expect(body).toEqual({ entries: { [ROOM]: null } })
    vi.useRealTimers()
  })

  it('applies a sibling frame and suppresses our own echo', () => {
    const { store, live, socket } = setup()
    live.start()

    socket().emitMessage(
      draftsFrame(store.deviceId, { [ROOM]: { text: 'echo' } }),
    )
    expect(store.draft(ACCT, ROOM)).toBe('')

    socket().emitMessage(
      draftsFrame('sibling', { [ROOM]: { text: 'from sibling' } }),
    )
    expect(store.draft(ACCT, ROOM)).toBe('from sibling')

    socket().emitMessage(draftsFrame('sibling', { [ROOM]: null }))
    expect(store.draft(ACCT, ROOM)).toBe('')
  })

  it('re-reads hydrated scopes on reconnect (lossy bus)', async () => {
    vi.useFakeTimers()
    let gets = 0
    server.use(
      http.get(STATE_PATH, () => {
        gets += 1
        return HttpResponse.json({ data: { namespace: 'drafts', entries: {} } })
      }),
    )
    const { store, live, socket } = setup()
    store.hydrateDrafts(ACCT)
    await vi.advanceTimersByTimeAsync(0)
    expect(gets).toBe(1)

    live.start()
    socket().emitOpen()
    socket().emitClose() // → reconnecting, schedules retry
    await vi.advanceTimersByTimeAsync(1000)
    socket().emitOpen() // reopened → reconnects bumps → re-read
    await vi.advanceTimersByTimeAsync(0)
    expect(gets).toBe(2)
    vi.useRealTimers()
  })

  it('advances the read marker forward only', () => {
    // Fake timers keep the debounced PUT from firing (no msw handler needed).
    vi.useFakeTimers()
    const { store } = setup()
    store.advanceReadMarker(ACCT, ROOM, '$e1', 100)
    expect(store.readMarker(ACCT, ROOM)).toEqual({
      eventId: '$e1',
      originTs: 100,
    })

    store.advanceReadMarker(ACCT, ROOM, '$older', 50)
    store.advanceReadMarker(ACCT, ROOM, '$same', 100)
    expect(store.readMarker(ACCT, ROOM)).toEqual({
      eventId: '$e1',
      originTs: 100,
    })

    store.advanceReadMarker(ACCT, ROOM, '$e2', 200)
    expect(store.readMarker(ACCT, ROOM)).toEqual({
      eventId: '$e2',
      originTs: 200,
    })
    vi.useRealTimers()
  })

  it('hydrates and advances thread read markers forward only', async () => {
    vi.useFakeTimers()
    let body: unknown
    server.use(
      http.get(STATE_PATH, ({ params }) => {
        expect(params.namespace).toBe('thread_read_markers')
        return HttpResponse.json({
          data: {
            namespace: 'thread_read_markers',
            entries: {
              [`${encodeURIComponent(ROOM)}:${encodeURIComponent(ROOT)}`]: {
                value: {
                  room_id: ROOM,
                  root_event_id: ROOT,
                  event_id: '$reply1',
                  origin_ts: 100,
                },
                device_id: 'sibling',
                updated_at: '2026-01-01T00:00:00Z',
              },
            },
          },
        })
      }),
      http.put(STATE_PATH, async ({ request }) => {
        body = await request.json()
        return HttpResponse.json({
          data: { updated_at: '2026-01-01T00:00:00Z' },
        })
      }),
    )
    const { store } = setup()
    store.hydrateThreadReadMarkers(ACCT)
    await vi.advanceTimersByTimeAsync(0)
    expect(store.threadReadMarker(ACCT, ROOM, ROOT)).toEqual({
      roomId: ROOM,
      rootEventId: ROOT,
      eventId: '$reply1',
      originTs: 100,
    })

    store.advanceThreadReadMarker(ACCT, ROOM, ROOT, '$older', 50)
    expect(store.threadReadMarker(ACCT, ROOM, ROOT)?.eventId).toBe('$reply1')
    store.advanceThreadReadMarker(ACCT, ROOM, ROOT, '$reply2', 200)
    expect(store.threadReadMarker(ACCT, ROOM, ROOT)).toEqual({
      roomId: ROOM,
      rootEventId: ROOT,
      eventId: '$reply2',
      originTs: 200,
    })

    await vi.advanceTimersByTimeAsync(800)
    expect(body).toEqual({
      entries: {
        [`${encodeURIComponent(ROOM)}:${encodeURIComponent(ROOT)}`]: {
          room_id: ROOM,
          root_event_id: ROOT,
          event_id: '$reply2',
          origin_ts: 200,
        },
      },
    })
    vi.useRealTimers()
  })

  it('re-queues a network-failed PUT and retries after the next debounce (WCR-12)', async () => {
    vi.useFakeTimers()
    const bodies: unknown[] = []
    let failNext = true
    server.use(
      http.put(STATE_PATH, async ({ request }) => {
        bodies.push(await request.json())
        if (failNext) {
          failNext = false
          return HttpResponse.error()
        }
        return HttpResponse.json({
          data: { updated_at: '2026-01-01T00:00:00Z' },
        })
      }),
    )
    const { store } = setup()
    store.setDraft(ACCT, ROOM, 'unsent draft')

    // First flush: the PUT never reaches the server; the batch re-queues.
    await vi.advanceTimersByTimeAsync(800)
    expect(bodies).toHaveLength(1)
    // The local cache still shows the write while the retry is pending.
    expect(store.draft(ACCT, ROOM)).toBe('unsent draft')

    // Second debounce period: the same batch is retried and lands.
    await vi.advanceTimersByTimeAsync(800)
    expect(bodies).toHaveLength(2)
    expect(bodies[1]).toEqual({ entries: { [ROOM]: { text: 'unsent draft' } } })

    // Settled: no further PUTs fire.
    await vi.advanceTimersByTimeAsync(2000)
    expect(bodies).toHaveLength(2)
    vi.useRealTimers()
  })

  it('a write during the failed PUT wins over the re-queued value', async () => {
    vi.useFakeTimers()
    const bodies: unknown[] = []
    let fail = true
    server.use(
      http.put(STATE_PATH, async ({ request }) => {
        bodies.push(await request.json())
        if (fail) {
          fail = false
          return HttpResponse.error()
        }
        return HttpResponse.json({
          data: { updated_at: '2026-01-01T00:00:00Z' },
        })
      }),
    )
    const { store } = setup()
    store.setDraft(ACCT, ROOM, 'first')
    await vi.advanceTimersByTimeAsync(800) // fails, re-queues 'first'
    store.setDraft(ACCT, ROOM, 'second') // newer write for the same key
    await vi.advanceTimersByTimeAsync(800)
    expect(bodies.at(-1)).toEqual({ entries: { [ROOM]: { text: 'second' } } })
    vi.useRealTimers()
  })

  it('re-queues a network-failed batch read-marker PUT', async () => {
    vi.useFakeTimers()
    const bodies: unknown[] = []
    let failNext = true
    server.use(
      http.put(STATE_PATH, async ({ request }) => {
        bodies.push(await request.json())
        if (failNext) {
          failNext = false
          return HttpResponse.error()
        }
        return HttpResponse.json({
          data: { updated_at: '2026-01-01T00:00:00Z' },
        })
      }),
    )
    const { store } = setup()

    await store.markRoomSummariesRead(ACCT, [
      {
        account_id: ACCT,
        room_id: ROOM,
        last_event_id: '$latest',
        last_activity_ts: 200,
      } as never,
    ])

    expect(bodies).toHaveLength(1)
    expect(store.readMarker(ACCT, ROOM)).toEqual({
      eventId: '$latest',
      originTs: 200,
    })

    await vi.advanceTimersByTimeAsync(800)
    expect(bodies).toHaveLength(2)
    expect(bodies[1]).toEqual({
      entries: { [ROOM]: { event_id: '$latest', origin_ts: 200 } },
    })
    vi.useRealTimers()
  })
})
