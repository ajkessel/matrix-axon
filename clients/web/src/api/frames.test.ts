import { describe, expect, it } from 'vitest'
import {
  decodeFrame,
  deviceStateChange,
  ephemeralPassthrough,
  EPHEMERAL_PASSTHROUGH,
  timelineEvent,
  TIMELINE_EVENT,
  unreadCountsChange,
  UNREAD_COUNTS_CHANGED,
} from './frames'

const envelope = (type: string, accountId: string, payload: unknown) =>
  JSON.stringify({ type, account_id: accountId, payload })

describe('decodeFrame', () => {
  it('decodes a well-formed envelope, surfacing account_id as accountId', () => {
    const frame = decodeFrame(
      envelope(TIMELINE_EVENT, 'acct-1', { event_id: '$e' }),
    )
    expect(frame).toEqual({
      type: TIMELINE_EVENT,
      accountId: 'acct-1',
      payload: { event_id: '$e' },
    })
  })

  it('keeps an unknown tag as an opaque string (router ignores it later)', () => {
    const frame = decodeFrame(envelope('ephemeral.passthrough', 'acct-1', {}))
    expect(frame?.type).toBe('ephemeral.passthrough')
  })

  it('drops non-JSON', () => {
    expect(decodeFrame('not json')).toBeNull()
  })

  it('drops JSON that is not an object', () => {
    expect(decodeFrame('42')).toBeNull()
    expect(decodeFrame('[]')).toBeNull()
    expect(decodeFrame('null')).toBeNull()
  })

  it('drops an envelope missing a string type or account_id', () => {
    expect(
      decodeFrame(JSON.stringify({ account_id: 'a', payload: {} })),
    ).toBeNull()
    expect(decodeFrame(JSON.stringify({ type: 't', payload: {} }))).toBeNull()
    expect(
      decodeFrame(JSON.stringify({ type: 1, account_id: 'a', payload: {} })),
    ).toBeNull()
  })
})

describe('timelineEvent', () => {
  it('returns the payload for a timeline.event frame', () => {
    const frame = decodeFrame(
      envelope(TIMELINE_EVENT, 'acct-1', { event_id: '$e', room_id: '!r' }),
    )!
    expect(timelineEvent(frame)).toEqual({ event_id: '$e', room_id: '!r' })
  })

  it('returns null for any other tag', () => {
    const frame = decodeFrame(envelope('device_state.changed', 'acct-1', {}))!
    expect(timelineEvent(frame)).toBeNull()
  })

  it('returns null when the payload is not an object', () => {
    expect(
      timelineEvent({ type: TIMELINE_EVENT, accountId: 'a', payload: null }),
    ).toBeNull()
  })
})

describe('deviceStateChange', () => {
  const changeFrame = (payload: unknown) =>
    decodeFrame(envelope('device_state.changed', 'acct-1', payload))!

  it('extracts device_id, namespace, and entries', () => {
    const frame = changeFrame({
      device_id: 'dev-1',
      namespace: 'drafts',
      entries: { '!r:hs': { text: 'hi' } },
      updated_at: '2026-01-01T00:00:00Z',
    })
    expect(deviceStateChange(frame)).toEqual({
      deviceId: 'dev-1',
      namespace: 'drafts',
      entries: { '!r:hs': { text: 'hi' } },
    })
  })

  it('returns null for a non-device_state tag', () => {
    const frame = decodeFrame(envelope(TIMELINE_EVENT, 'acct-1', {}))!
    expect(deviceStateChange(frame)).toBeNull()
  })

  it('returns null when required fields are missing or mistyped', () => {
    expect(
      deviceStateChange(changeFrame({ namespace: 'drafts', entries: {} })),
    ).toBeNull()
    expect(
      deviceStateChange(
        changeFrame({ device_id: 1, namespace: 'drafts', entries: {} }),
      ),
    ).toBeNull()
    expect(
      deviceStateChange(
        changeFrame({ device_id: 'd', namespace: 'drafts', entries: null }),
      ),
    ).toBeNull()
  })
})

describe('ephemeralPassthrough', () => {
  const ephemeralFrame = (payload: unknown) =>
    decodeFrame(envelope(EPHEMERAL_PASSTHROUGH, 'acct-1', payload))!

  it('extracts a room-scoped passthrough payload', () => {
    const frame = ephemeralFrame({
      room_id: '!r:hs',
      event_type: 'm.typing',
      content: { user_ids: ['@alice:hs'] },
    })
    expect(ephemeralPassthrough(frame)).toEqual({
      roomId: '!r:hs',
      eventType: 'm.typing',
      content: { user_ids: ['@alice:hs'] },
    })
  })

  it('allows an omitted room_id for future account-scoped events', () => {
    const frame = ephemeralFrame({
      event_type: 'm.presence',
      content: { presence: 'online' },
    })
    expect(ephemeralPassthrough(frame)).toEqual({
      roomId: null,
      eventType: 'm.presence',
      content: { presence: 'online' },
    })
  })

  it('returns null for other tags and malformed payloads', () => {
    expect(
      ephemeralPassthrough(decodeFrame(envelope(TIMELINE_EVENT, 'a', {}))!),
    ).toBeNull()
    expect(ephemeralPassthrough(ephemeralFrame(null))).toBeNull()
    expect(
      ephemeralPassthrough(
        ephemeralFrame({ room_id: 1, event_type: 'm.typing', content: {} }),
      ),
    ).toBeNull()
    expect(
      ephemeralPassthrough(ephemeralFrame({ room_id: '!r:hs', content: {} })),
    ).toBeNull()
  })
})

describe('unreadCountsChange', () => {
  const unreadFrame = (payload: unknown) =>
    decodeFrame(envelope(UNREAD_COUNTS_CHANGED, 'acct-1', payload))!

  it('extracts server-derived room unread counts', () => {
    const frame = unreadFrame({
      room_id: '!r:hs',
      notification_count: 3,
      highlight_count: 1,
    })
    expect(unreadCountsChange(frame)).toEqual({
      roomId: '!r:hs',
      notificationCount: 3,
      highlightCount: 1,
    })
  })

  it('returns null for other tags and malformed payloads', () => {
    expect(
      unreadCountsChange(decodeFrame(envelope(TIMELINE_EVENT, 'a', {}))!),
    ).toBeNull()
    expect(unreadCountsChange(unreadFrame(null))).toBeNull()
    expect(
      unreadCountsChange(
        unreadFrame({
          room_id: '!r:hs',
          notification_count: -1,
          highlight_count: 0,
        }),
      ),
    ).toBeNull()
    expect(
      unreadCountsChange(
        unreadFrame({
          room_id: '!r:hs',
          notification_count: 1.5,
          highlight_count: 0,
        }),
      ),
    ).toBeNull()
    expect(
      unreadCountsChange(
        unreadFrame({ room_id: 1, notification_count: 1, highlight_count: 0 }),
      ),
    ).toBeNull()
  })
})
