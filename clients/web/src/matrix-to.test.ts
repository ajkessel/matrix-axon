import { describe, expect, it } from 'vitest'
import {
  matrixToRoomReferenceLink,
  parseMatrixRoomReference,
} from './matrix-to'

describe('matrixToRoomReferenceLink', () => {
  it('adds via hints for room IDs without an embedded server name', () => {
    const href = matrixToRoomReferenceLink(
      '!Oha01wS8odafKACi1LVh_QLxBRJVaMYRCEbISYUJYKk',
      null,
      { via: ['bostoncoop.net'] },
    )

    expect(href).toBe(
      'https://matrix.to/#/!Oha01wS8odafKACi1LVh_QLxBRJVaMYRCEbISYUJYKk?via=bostoncoop.net',
    )
    expect(parseMatrixRoomReference(href)).toEqual({
      roomIdOrAlias: '!Oha01wS8odafKACi1LVh_QLxBRJVaMYRCEbISYUJYKk',
      serverNames: ['bostoncoop.net'],
      eventId: null,
    })
  })

  it('prefers canonical aliases over room-ID via hints', () => {
    expect(
      matrixToRoomReferenceLink(
        '!Oha01wS8odafKACi1LVh_QLxBRJVaMYRCEbISYUJYKk',
        '#ops:bostoncoop.net',
        { via: ['bostoncoop.net'] },
      ),
    ).toBe('https://matrix.to/#/%23ops%3Abostoncoop.net')
  })
})

describe('parseMatrixRoomReference', () => {
  it('parses Matrix.to aliases with via hints', () => {
    expect(
      parseMatrixRoomReference(
        'https://matrix.to/#/%23ops%3Aexample.org?via=example.org&via=backup.example',
      ),
    ).toEqual({
      roomIdOrAlias: '#ops:example.org',
      serverNames: ['example.org', 'backup.example'],
      eventId: null,
    })
  })

  it('parses Matrix.to room event links', () => {
    expect(
      parseMatrixRoomReference(
        'https://matrix.to/#/!room%3Aexample.org/%24event?via=example.org',
      ),
    ).toEqual({
      roomIdOrAlias: '!room:example.org',
      serverNames: ['example.org'],
      eventId: '$event',
    })
  })

  it('parses Matrix.to room IDs that rely on via hints', () => {
    expect(
      parseMatrixRoomReference(
        'https://matrix.to/#/!Oha01wS8odafKACi1LVh_QLxBRJVaMYRCEbISYUJYKk?via=bostoncoop.net',
      ),
    ).toEqual({
      roomIdOrAlias: '!Oha01wS8odafKACi1LVh_QLxBRJVaMYRCEbISYUJYKk',
      serverNames: ['bostoncoop.net'],
      eventId: null,
    })
  })

  it('rejects Matrix.to room IDs without a room server or via hints', () => {
    expect(
      parseMatrixRoomReference(
        'https://matrix.to/#/!Oha01wS8odafKACi1LVh_QLxBRJVaMYRCEbISYUJYKk',
      ),
    ).toBeNull()
  })

  it('parses raw room IDs with the room server as a via hint', () => {
    expect(parseMatrixRoomReference('!room:example.org')).toEqual({
      roomIdOrAlias: '!room:example.org',
      serverNames: ['example.org'],
      eventId: null,
    })
  })

  it('parses raw aliases without inventing via hints', () => {
    expect(parseMatrixRoomReference('#ops:example.org')).toEqual({
      roomIdOrAlias: '#ops:example.org',
      serverNames: [],
      eventId: null,
    })
  })

  it('parses typed alias shorthands when explicitly enabled', () => {
    const options = {
      allowAliasShorthand: true,
      defaultAliasServerName: 'example.org',
    }
    expect(parseMatrixRoomReference('ops:example.org', options)).toEqual({
      roomIdOrAlias: '#ops:example.org',
      serverNames: [],
      eventId: null,
    })
    expect(parseMatrixRoomReference('ops@example.org', options)).toEqual({
      roomIdOrAlias: '#ops:example.org',
      serverNames: [],
      eventId: null,
    })
    expect(parseMatrixRoomReference('#ops', options)).toEqual({
      roomIdOrAlias: '#ops:example.org',
      serverNames: [],
      eventId: null,
    })
    expect(parseMatrixRoomReference('ops', options)).toEqual({
      roomIdOrAlias: '#ops:example.org',
      serverNames: [],
      eventId: null,
    })
  })

  it('keeps shorthand aliases out of strict Matrix link parsing', () => {
    expect(parseMatrixRoomReference('ops:example.org')).toBeNull()
    expect(parseMatrixRoomReference('ops@example.org')).toBeNull()
    expect(parseMatrixRoomReference('ops')).toBeNull()
    expect(parseMatrixRoomReference('#ops')).toBeNull()
    expect(
      parseMatrixRoomReference('#ops', { allowAliasShorthand: true }),
    ).toBeNull()
    expect(
      parseMatrixRoomReference('matrix:u/alice:example.org', {
        allowAliasShorthand: true,
        defaultAliasServerName: 'example.org',
      }),
    ).toBeNull()
  })

  it('parses matrix scheme room aliases', () => {
    expect(
      parseMatrixRoomReference(
        'matrix:r/ops%3Aexample.org/e/%24event?via=example.org',
      ),
    ).toEqual({
      roomIdOrAlias: '#ops:example.org',
      serverNames: ['example.org'],
      eventId: '$event',
    })
  })

  it('parses matrix scheme room aliases using the room sigil spelling', () => {
    expect(parseMatrixRoomReference('matrix:room/a:bostoncoop.net')).toEqual({
      roomIdOrAlias: '#a:bostoncoop.net',
      serverNames: [],
      eventId: null,
    })
  })

  it('parses matrix scheme room IDs', () => {
    expect(
      parseMatrixRoomReference(
        'matrix:roomid/!room%3Aexample.org?via=example.org',
      ),
    ).toEqual({
      roomIdOrAlias: '!room:example.org',
      serverNames: ['example.org'],
      eventId: null,
    })
  })

  it('parses matrix scheme room IDs that rely on via hints', () => {
    expect(
      parseMatrixRoomReference(
        'matrix:roomid/!Oha01wS8odafKACi1LVh_QLxBRJVaMYRCEbISYUJYKk?via=bostoncoop.net',
      ),
    ).toEqual({
      roomIdOrAlias: '!Oha01wS8odafKACi1LVh_QLxBRJVaMYRCEbISYUJYKk',
      serverNames: ['bostoncoop.net'],
      eventId: null,
    })
  })

  it('rejects user links and malformed values', () => {
    expect(
      parseMatrixRoomReference('https://matrix.to/#/%40alice%3Aexample.org'),
    ).toBeNull()
    expect(parseMatrixRoomReference('matrix:u/alice:example.org')).toBeNull()
    expect(parseMatrixRoomReference('not a room')).toBeNull()
  })
})
