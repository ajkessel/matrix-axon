import { describe, expect, it } from 'vitest'
import type { RoomDto } from './room-list'
import { resolveRoomTarget, roomCommandSuggestions } from './room-command'

function room(
  room_id: string,
  canonical_alias: string | null,
  name: string | null,
): RoomDto {
  return {
    account_id: 'acct',
    account_user_id: '@me:example.com',
    room_id,
    name,
    canonical_alias,
    topic: null,
    last_activity_ts: 0,
  } as RoomDto
}

describe('room command matching', () => {
  it('completes a unique display-name match to the canonical alias', () => {
    const suggestions = roomCommandSuggestions(
      [
        room('!one:example.com', '#axontest:bostoncoop.net', 'axontest'),
        room('!two:example.com', '#axondev:bostoncoop.net', 'axondev'),
      ],
      'axont',
    )

    expect(suggestions.map((suggestion) => suggestion.completion)).toEqual([
      '#axontest:bostoncoop.net',
    ])
  })

  it('matches shortened Matrix aliases with or without a leading hash', () => {
    const rooms = [
      room('!one:example.com', '#test:example.com', 'Test'),
      room('!two:example.com', '#testing:example.com', 'Testing'),
    ]

    expect(
      roomCommandSuggestions(rooms, 'test:ex').map(
        (suggestion) => suggestion.completion,
      ),
    ).toEqual(['#test:example.com'])
    expect(resolveRoomTarget(rooms, '#testing').kind).toBe('match')
  })

  it('reports ambiguous prefix matches instead of guessing', () => {
    const resolution = resolveRoomTarget(
      [
        room('!one:example.com', null, 'axontest'),
        room('!two:example.com', null, 'axondev'),
      ],
      'axon',
    )

    expect(resolution).toEqual({
      kind: 'ambiguous',
      options: ['test', 'dev'],
    })
  })
})
