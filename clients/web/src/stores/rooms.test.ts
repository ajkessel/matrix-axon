import { HttpResponse, http } from 'msw'
import { setupServer } from 'msw/node'
import { effect } from '@preact/signals'
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
import { memoryStorage } from '../test/memory-storage'
import { roomKey } from './room-list'
import { createRoomsStore } from './rooms'
import { createUnreadStore } from './unread'

const BASE_URL = 'http://axon.test'
const ACCOUNT = '6b53f7f0-0000-4000-8000-000000000001'

const NAMED = {
  account_id: ACCOUNT,
  account_user_id: '@me:example.org',
  room_id: '!ops:hs',
  name: 'Ops',
  last_activity_ts: 100,
}
const UNNAMED = {
  account_id: ACCOUNT,
  account_user_id: '@me:example.org',
  room_id: '!dm:hs',
  last_activity_ts: 200,
}

const server = setupServer()
beforeAll(() => server.listen({ onUnhandledRequest: 'error' }))
afterEach(() => server.resetHandlers())
afterAll(() => server.close())

function makeStore(storage = memoryStorage()) {
  const api = createApiClient(
    {
      getToken: () => 'tok-test',
      onAuthFailure: () => {},
      LoginBootstrap: () => null,
    },
    BASE_URL,
  )
  return createRoomsStore(api, storage)
}

describe('createRoomsStore', () => {
  it('loads rooms and resolves member-derived titles for unnamed rooms', async () => {
    let memberCalls = 0
    server.use(
      http.get(`${BASE_URL}/v1/rooms`, () =>
        HttpResponse.json({ data: [NAMED, UNNAMED] }),
      ),
      http.get(
        `${BASE_URL}/v1/accounts/${ACCOUNT}/rooms/${encodeURIComponent('!dm:hs')}/members`,
        () => {
          memberCalls += 1
          return HttpResponse.json({
            data: [
              {
                user_id: '@me:example.org',
                membership: 'join',
                display_name: 'Me',
              },
              {
                user_id: '@bob:example.org',
                membership: 'join',
                display_name: 'Bob',
              },
            ],
          })
        },
      ),
    )

    const store = makeStore()
    await store.refresh()

    expect(store.loading.value).toBe(false)
    expect(store.rooms.value).toHaveLength(2)
    await vi.waitFor(() =>
      expect(store.titles.value.get(roomKey(UNNAMED))).toBe('Bob'),
    )
    // Named rooms never trigger a members fetch.
    expect(memberCalls).toBe(1)

    // A second refresh does not re-fetch settled titles.
    await store.refresh()
    await new Promise((resolve) => setTimeout(resolve, 10))
    expect(memberCalls).toBe(1)
  })

  it('persists room titles and starts the next store from the cache', async () => {
    server.use(
      http.get(`${BASE_URL}/v1/rooms`, () =>
        HttpResponse.json({ data: [NAMED, UNNAMED] }),
      ),
      http.get(
        `${BASE_URL}/v1/accounts/${ACCOUNT}/rooms/${encodeURIComponent('!dm:hs')}/members`,
        () =>
          HttpResponse.json({
            data: [
              {
                user_id: '@me:example.org',
                membership: 'join',
                display_name: 'Me',
              },
              {
                user_id: '@bob:example.org',
                membership: 'join',
                display_name: 'Bob',
              },
            ],
          }),
      ),
    )
    const storage = memoryStorage()
    const first = makeStore(storage)
    await first.refresh()
    await vi.waitFor(() =>
      expect(first.titles.value.get(roomKey(UNNAMED))).toBe('Bob'),
    )

    const second = makeStore(storage)
    expect(second.titles.value.get(roomKey(NAMED))).toBe('Ops')
    expect(second.titles.value.get(roomKey(UNNAMED))).toBe('Bob')
  })

  it('keeps the room-id fallback when the members fetch fails', async () => {
    server.use(
      http.get(`${BASE_URL}/v1/rooms`, () =>
        HttpResponse.json({ data: [UNNAMED] }),
      ),
      http.get(
        `${BASE_URL}/v1/accounts/${ACCOUNT}/rooms/${encodeURIComponent('!dm:hs')}/members`,
        () =>
          HttpResponse.json(
            { error: { code: 'not_found', message: 'no such room' } },
            { status: 404 },
          ),
      ),
    )

    const store = makeStore()
    await store.refresh()
    await new Promise((resolve) => setTimeout(resolve, 10))

    expect(store.titles.value.has(roomKey(UNNAMED))).toBe(false)
    expect(store.error.value).toBeNull()
  })

  it('noteActivity advances last_activity_ts and ignores older stamps (WCR-08)', async () => {
    server.use(
      http.get(`${BASE_URL}/v1/rooms`, () =>
        HttpResponse.json({ data: [NAMED, UNNAMED] }),
      ),
      http.get(`${BASE_URL}/v1/accounts/:accountId/rooms/:roomId/members`, () =>
        HttpResponse.json({ data: [] }),
      ),
    )
    const store = makeStore()
    await store.refresh()

    // NAMED (ts 100) is not the freshest; a newer stamp re-publishes the list.
    const before = store.rooms.value
    store.noteActivity(ACCOUNT, NAMED.room_id, 500)
    expect(store.rooms.value).not.toBe(before)
    expect(
      store.rooms.value.find((r) => r.room_id === NAMED.room_id)!
        .last_activity_ts,
    ).toBe(500)

    // An older or equal stamp is inert.
    const after = store.rooms.value
    store.noteActivity(ACCOUNT, NAMED.room_id, 400)
    store.noteActivity(ACCOUNT, NAMED.room_id, 500)
    expect(store.rooms.value).toBe(after)
  })

  it('a burst in the already-freshest room does not re-publish the list (WCR-08)', async () => {
    server.use(
      http.get(`${BASE_URL}/v1/rooms`, () =>
        HttpResponse.json({ data: [NAMED, UNNAMED] }),
      ),
      http.get(`${BASE_URL}/v1/accounts/:accountId/rooms/:roomId/members`, () =>
        HttpResponse.json({ data: [] }),
      ),
    )
    const store = makeStore()
    await store.refresh()

    // Make NAMED the freshest, then burst it: the first bump re-sorts, the
    // rest must not trigger an O(rooms) copy + full list re-render each.
    store.noteActivity(ACCOUNT, NAMED.room_id, 500)
    const published = store.rooms.value
    store.noteActivity(ACCOUNT, NAMED.room_id, 600)
    store.noteActivity(ACCOUNT, NAMED.room_id, 700)
    expect(store.rooms.value).toBe(published)
    // The stamp still advanced (in place) so ordering stays correct.
    expect(
      store.rooms.value.find((r) => r.room_id === NAMED.room_id)!
        .last_activity_ts,
    ).toBe(700)
  })

  it('updates a room preview from a live message without a timeline refetch', async () => {
    let timelineCalls = 0
    server.use(
      http.get(`${BASE_URL}/v1/rooms`, () =>
        HttpResponse.json({ data: [NAMED] }),
      ),
      http.get(
        `${BASE_URL}/v1/accounts/${ACCOUNT}/rooms/${encodeURIComponent(NAMED.room_id)}/members`,
        () =>
          HttpResponse.json({
            data: [
              {
                user_id: '@me:example.org',
                membership: 'join',
                display_name: 'Me',
              },
              {
                user_id: '@adam:example.org',
                membership: 'join',
                display_name: 'Adam',
              },
            ],
          }),
      ),
      http.get(
        `${BASE_URL}/v1/accounts/${ACCOUNT}/rooms/${encodeURIComponent(NAMED.room_id)}/timeline`,
        () => {
          timelineCalls += 1
          return HttpResponse.json({ data: { events: [], next_cursor: null } })
        },
      ),
    )
    const store = makeStore()
    await store.refresh()

    store.noteTimelineEvent({
      account_id: ACCOUNT,
      room_id: NAMED.room_id,
      event_id: '$live',
      origin_ts: 500,
      sender: '@adam:example.org',
      type: 'm.room.message',
      state_key: null,
      body: '**Hello** from [live](https://example.org)',
      content: { msgtype: 'm.text', body: 'Hello from live' } as never,
      redacted: false,
      edited: false,
      edit_count: 0,
    })

    expect(store.preview(roomKey(NAMED))?.body).toBe('Hello from live')
    expect(store.preview(roomKey(NAMED))?.senderDisplay).toBe(
      '@adam:example.org',
    )
    await vi.waitFor(() =>
      expect(store.preview(roomKey(NAMED))?.senderDisplay).toBe('Adam'),
    )
    expect(timelineCalls).toBe(0)
  })

  it('a live event tying last_activity_ts still updates the preview', async () => {
    server.use(
      http.get(`${BASE_URL}/v1/rooms`, () =>
        HttpResponse.json({ data: [NAMED] }),
      ),
      http.get(`${BASE_URL}/v1/accounts/:accountId/rooms/:roomId/members`, () =>
        HttpResponse.json({ data: [] }),
      ),
    )
    const store = makeStore()
    await store.refresh()

    // Same millisecond as the refreshed last_activity_ts (a send burst, or a
    // frame racing a refresh that already recorded its ts): the newer event
    // must still become the preview.
    store.noteTimelineEvent({
      account_id: ACCOUNT,
      room_id: NAMED.room_id,
      event_id: '$tie',
      origin_ts: NAMED.last_activity_ts,
      sender: '@adam:example.org',
      type: 'm.room.message',
      state_key: null,
      body: 'Tied timestamp',
      content: { msgtype: 'm.text', body: 'Tied timestamp' } as never,
      redacted: false,
      edited: false,
      edit_count: 0,
    })

    expect(store.preview(roomKey(NAMED))?.body).toBe('Tied timestamp')
  })

  it('a live preview update wakes only that room preview subscriber', async () => {
    server.use(
      http.get(`${BASE_URL}/v1/rooms`, () =>
        HttpResponse.json({ data: [NAMED, UNNAMED] }),
      ),
      http.get(`${BASE_URL}/v1/accounts/:accountId/rooms/:roomId/members`, () =>
        HttpResponse.json({ data: [] }),
      ),
    )
    const store = makeStore()
    await store.refresh()
    const namedKey = roomKey(NAMED)
    const unnamedKey = roomKey(UNNAMED)
    let namedRuns = 0
    let unnamedRuns = 0
    const stopNamed = effect(() => {
      store.preview(namedKey)
      namedRuns += 1
    })
    const stopUnnamed = effect(() => {
      store.preview(unnamedKey)
      unnamedRuns += 1
    })
    const namedBefore = namedRuns
    const unnamedBefore = unnamedRuns

    store.noteTimelineEvent({
      account_id: ACCOUNT,
      room_id: NAMED.room_id,
      event_id: '$live',
      origin_ts: 500,
      sender: '@adam:example.org',
      type: 'm.room.message',
      state_key: null,
      body: 'Only Ops changes',
      content: { msgtype: 'm.text', body: 'Only Ops changes' } as never,
      redacted: false,
      edited: false,
      edit_count: 0,
    })

    expect(namedRuns).toBe(namedBefore + 1)
    expect(unnamedRuns).toBe(unnamedBefore)
    stopNamed()
    stopUnnamed()
  })

  it('an unknown room triggers one coalesced refresh (WCR-08)', async () => {
    let listCalls = 0
    server.use(
      http.get(`${BASE_URL}/v1/rooms`, () => {
        listCalls += 1
        return HttpResponse.json({ data: [NAMED] })
      }),
    )
    const store = makeStore()
    await store.refresh()
    expect(listCalls).toBe(1)

    // A frame burst for a freshly joined room re-reads the list once.
    store.noteActivity(ACCOUNT, '!new:hs', 900)
    store.noteActivity(ACCOUNT, '!new:hs', 901)
    store.noteActivity(ACCOUNT, '!new:hs', 902)
    await vi.waitFor(() => expect(listCalls).toBe(2))
    expect(listCalls).toBe(2)
  })

  it('surfaces a list-fetch error', async () => {
    server.use(
      http.get(`${BASE_URL}/v1/rooms`, () =>
        HttpResponse.json(
          { error: { code: 'internal', message: 'database unavailable' } },
          { status: 500 },
        ),
      ),
    )
    const store = makeStore()
    await store.refresh()

    expect(store.error.value).toBe('database unavailable')
    expect(store.loading.value).toBe(false)
  })
})

describe('createUnreadStore', () => {
  it('counts events per room and clears on markSeen', () => {
    const unread = createUnreadStore()
    expect(unread.count('a/x')).toBe(0)

    unread.recordEvent('a/x')
    unread.recordEvent('a/x')
    unread.recordEvent('a/y')

    expect(unread.count('a/x')).toBe(2)
    expect(unread.count('a/y')).toBe(1)

    unread.markSeen('a/x')
    expect(unread.count('a/x')).toBe(0)
    expect(unread.count('a/y')).toBe(1)

    // markSeen on an unknown key is a no-op, not an entry.
    const before = unread.unreadKeys.value
    unread.markSeen('a/z')
    expect(unread.unreadKeys.value).toBe(before)
  })

  it('moves unreadKeys only when a room crosses 0 <-> unread', () => {
    const unread = createUnreadStore()
    expect(unread.unreadKeys.value.size).toBe(0)

    unread.recordEvent('a/x')
    const afterFirst = unread.unreadKeys.value
    expect([...afterFirst]).toEqual(['a/x'])

    // A second event in an already-unread room bumps the count but must not
    // rewrite the set — the Unread filter re-runs off it.
    unread.recordEvent('a/x')
    expect(unread.count('a/x')).toBe(2)
    expect(unread.unreadKeys.value).toBe(afterFirst)

    unread.markSeen('a/x')
    expect(unread.unreadKeys.value.size).toBe(0)

    // Seeing an already-seen room is likewise inert.
    const afterSeen = unread.unreadKeys.value
    unread.markSeen('a/x')
    expect(unread.unreadKeys.value).toBe(afterSeen)
  })
})
