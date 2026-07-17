import { describe, expect, it } from 'vitest'
import {
  formatMessageBody,
  roomReferenceSuggestions,
  userMentionSuggestions,
} from './mentions'
import type { MemberDto, RoomDto } from './stores/room-list'

const ACCOUNT = 'acct-1'

function member(userId: string, displayName: string | null = null): MemberDto {
  return {
    user_id: userId,
    display_name: displayName,
    membership: 'join',
  } as MemberDto
}

function room(overrides: Partial<RoomDto>): RoomDto {
  return {
    account_id: ACCOUNT,
    account_user_id: '@me:hs',
    room_id: '!room:hs',
    name: null,
    canonical_alias: null,
    topic: null,
    last_activity_ts: 0,
    last_event_id: null,
    membership: 'join',
    ...overrides,
  } as RoomDto
}

describe('mention and room suggestions', () => {
  it('matches users by display name, localpart, and full Matrix id', () => {
    const members = [member('@alice:hs', 'Alice Cooper')]

    expect(userMentionSuggestions(members, 'ali')[0].value).toBe('@alice')
    expect(userMentionSuggestions(members, 'Alice')[0].value).toBe('@alice')
    expect(userMentionSuggestions(members, '@alice:hs')[0].value).toBe('@alice')
  })

  it('matches room references by title and alias localpart', () => {
    const rooms = [
      room({
        room_id: '!ops:hs',
        name: 'Ops Team',
        canonical_alias: '#ops:hs',
      }),
    ]

    expect(roomReferenceSuggestions(rooms, new Map(), 'op')[0].value).toBe(
      '#ops:hs',
    )
    expect(
      roomReferenceSuggestions(rooms, new Map(), 'Ops Team')[0].value,
    ).toBe('#ops:hs')
  })
})

describe('formatMessageBody', () => {
  it('adds pill links for exact user and room tokens', () => {
    const formatted = formatMessageBody('hi @alice in #Ops', {
      accountId: ACCOUNT,
      members: [member('@alice:hs', 'Alice')],
      rooms: [room({ room_id: '!ops:hs', name: 'Ops' })],
      roomTitles: new Map(),
    })

    expect(formatted.body).toBe('hi @alice in #Ops')
    expect(formatted.format).toBe('org.matrix.custom.html')
    expect(formatted.formatted_body).toContain('class="mention-pill"')
    expect(formatted.formatted_body).toContain(
      'href="https://matrix.to/#/%40alice%3Ahs"',
    )
    expect(formatted.formatted_body).toContain('class="room-pill"')
    expect(formatted.formatted_body).toContain('href="/acct-1/rooms/!ops%3Ahs"')
  })

  it('expands emoji shortcodes before formatting', () => {
    const formatted = formatMessageBody('ship it :+1:', {
      accountId: ACCOUNT,
      members: [],
      rooms: [],
      roomTitles: new Map(),
    })

    expect(formatted).toEqual({ body: 'ship it 👍' })
  })

  it('honors escaped emoji shortcodes', () => {
    const formatted = formatMessageBody('\\:+1:', {
      accountId: ACCOUNT,
      members: [],
      rooms: [],
      roomTitles: new Map(),
    })

    expect(formatted).toEqual({ body: ':+1:' })
  })

  it('leaves unknown emoji shortcodes literal', () => {
    const formatted = formatMessageBody(':not_an_emoji:', {
      accountId: ACCOUNT,
      members: [],
      rooms: [],
      roomTitles: new Map(),
    })

    expect(formatted).toEqual({ body: ':not_an_emoji:' })
  })

  it('keeps markdown formatting while adding pills', () => {
    const formatted = formatMessageBody('**hi** @alice', {
      accountId: ACCOUNT,
      members: [member('@alice:hs')],
      rooms: [],
      roomTitles: new Map(),
    })

    expect(formatted.formatted_body).toContain('<strong>hi</strong>')
    expect(formatted.formatted_body).toContain('class="mention-pill"')
  })

  it('leaves unknown and ambiguous tokens as plain text', () => {
    const formatted = formatMessageBody('hi @alice #Ops', {
      accountId: ACCOUNT,
      members: [member('@alice:one'), member('@alice:two')],
      rooms: [
        room({ room_id: '!one:hs', name: 'Ops' }),
        room({ room_id: '!two:hs', name: 'Ops' }),
      ],
      roomTitles: new Map(),
    })

    expect(formatted).toEqual({ body: 'hi @alice #Ops' })
  })

  it('does not link mid-word tokens or email addresses', () => {
    const formatted = formatMessageBody('mail a@alice and x#Ops', {
      accountId: ACCOUNT,
      members: [member('@alice:hs')],
      rooms: [room({ room_id: '!ops:hs', name: 'Ops' })],
      roomTitles: new Map(),
    })

    expect(formatted).toEqual({ body: 'mail a@alice and x#Ops' })
  })

  it('escapes raw HTML before sending formatted mention output', () => {
    const formatted = formatMessageBody('<script>x</script> @alice', {
      accountId: ACCOUNT,
      members: [member('@alice:hs')],
      rooms: [],
      roomTitles: new Map(),
    })

    expect(formatted.formatted_body).not.toContain('<script>')
    expect(formatted.formatted_body).toContain('&lt;script&gt;')
  })
})
