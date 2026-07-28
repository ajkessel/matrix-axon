import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  createEphemeralStore,
  parseReceiptContent,
  parseTypingContent,
  TYPING_TTL_MS,
} from './ephemeral'

const ACCOUNT = '6b53f7f0-0000-4000-8000-000000000001'
const ROOM = '!room:hs'

afterEach(() => {
  vi.useRealTimers()
})

describe('parseTypingContent', () => {
  it('accepts a Matrix typing user list and deduplicates users', () => {
    expect(
      parseTypingContent({ user_ids: ['@alice:hs', '@alice:hs', '@bob:hs'] }),
    ).toEqual(['@alice:hs', '@bob:hs'])
  })

  it('rejects malformed typing content', () => {
    expect(parseTypingContent(null)).toBeNull()
    expect(parseTypingContent({})).toBeNull()
    expect(parseTypingContent({ user_ids: ['@alice:hs', 42] })).toBeNull()
  })
})

describe('parseReceiptContent', () => {
  it('extracts public and private read receipts from nested Matrix content', () => {
    const parsed = parseReceiptContent({
      $e1: {
        'm.read': {
          '@alice:hs': { ts: 100 },
          '@bob:hs': {},
        },
        'm.read.private': {
          '@me:hs': { ts: 120 },
        },
      },
      $ignored: {
        'm.fully_read': {
          '@alice:hs': { ts: 90 },
        },
      },
    })

    expect(parsed.get('$e1')?.publicRead).toEqual(
      new Map([
        ['@alice:hs', 100],
        ['@bob:hs', null],
      ]),
    )
    expect(parsed.get('$e1')?.privateRead).toEqual(new Map([['@me:hs', 120]]))
    expect(parsed.has('$ignored')).toBe(false)
  })
})

describe('createEphemeralStore', () => {
  it('replaces typing users per room, filters self, and clears on empty list', () => {
    const store = createEphemeralStore()
    store.apply(ACCOUNT, {
      roomId: ROOM,
      eventType: 'm.typing',
      content: { user_ids: ['@me:hs', '@alice:hs'] },
    })
    expect(store.typingUsers(ACCOUNT, ROOM, '@me:hs')).toEqual(['@alice:hs'])

    store.apply(ACCOUNT, {
      roomId: ROOM,
      eventType: 'm.typing',
      content: { user_ids: ['@bob:hs'] },
    })
    expect(store.typingUsers(ACCOUNT, ROOM, '@me:hs')).toEqual(['@bob:hs'])

    store.apply(ACCOUNT, {
      roomId: ROOM,
      eventType: 'm.typing',
      content: { user_ids: [] },
    })
    expect(store.typingUsers(ACCOUNT, ROOM, '@me:hs')).toEqual([])
  })

  it('expires typing users after the TTL', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(0)
    const store = createEphemeralStore()
    store.apply(ACCOUNT, {
      roomId: ROOM,
      eventType: 'm.typing',
      content: { user_ids: ['@alice:hs'] },
    })
    expect(store.typingUsers(ACCOUNT, ROOM)).toEqual(['@alice:hs'])

    vi.setSystemTime(TYPING_TTL_MS)
    await vi.advanceTimersByTimeAsync(TYPING_TTL_MS)
    expect(store.typingUsers(ACCOUNT, ROOM)).toEqual([])
  })

  it('stores public read receipts by event and filters self', () => {
    const store = createEphemeralStore()
    store.apply(ACCOUNT, {
      roomId: ROOM,
      eventType: 'm.receipt',
      content: {
        $e1: {
          'm.read': {
            '@me:hs': { ts: 50 },
            '@alice:hs': { ts: 100 },
            '@bob:hs': { ts: 80 },
          },
        },
      },
    })

    expect(store.readReceipts(ACCOUNT, ROOM, '$e1', '@me:hs')).toEqual([
      { userId: '@alice:hs', ts: 100 },
      { userId: '@bob:hs', ts: 80 },
    ])
  })

  it('moves a user receipt forward instead of showing it on old events too', () => {
    const store = createEphemeralStore()
    store.apply(ACCOUNT, {
      roomId: ROOM,
      eventType: 'm.receipt',
      content: {
        $e1: {
          'm.read': {
            '@alice:hs': { ts: 100 },
          },
        },
      },
    })
    store.apply(ACCOUNT, {
      roomId: ROOM,
      eventType: 'm.receipt',
      content: {
        $e2: {
          'm.read': {
            '@alice:hs': { ts: 200 },
          },
        },
      },
    })

    expect(store.readReceipts(ACCOUNT, ROOM, '$e1')).toEqual([])
    expect(store.readReceipts(ACCOUNT, ROOM, '$e2')).toEqual([
      { userId: '@alice:hs', ts: 200 },
    ])
  })

  it('ignores account-scoped passthrough events for room overlays', () => {
    const store = createEphemeralStore()
    store.apply(ACCOUNT, {
      roomId: null,
      eventType: 'm.typing',
      content: { user_ids: ['@alice:hs'] },
    })
    expect(store.typingUsers(ACCOUNT, ROOM)).toEqual([])
  })
})
