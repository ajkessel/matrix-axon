import { localpart, roomTitle, type RoomDto } from './stores/room-list'

const MATRIX_TO_ORIGIN = 'https://matrix.to'

export interface ResolvedMatrixToRoomLink {
  href: string
  label: string
  isEventLink: boolean
}

export interface ResolvedMatrixToUserLink {
  href: string
  label: string
}

interface MatrixToTarget {
  mxid: string
  eventId: string | null
}

export function matrixToLink(
  mxid: string,
  via: readonly string[] = [],
): string {
  let href = `${MATRIX_TO_ORIGIN}/#/${encodeURIComponent(mxid)}`
  const viaParams = via
    .map((server) => server.trim())
    .filter((server) => server !== '')
    .map((server) => `via=${encodeURIComponent(server)}`)
  if (viaParams.length > 0) {
    href += `?${viaParams.join('&')}`
  }
  return href
}

export function matrixToRoomLink(
  room: Pick<RoomDto, 'room_id' | 'canonical_alias'>,
): string {
  return matrixToRoomReferenceLink(room.room_id, room.canonical_alias)
}

export function matrixToRoomReferenceLink(
  roomId: string,
  canonicalAlias: string | null | undefined,
): string {
  const alias = canonicalAlias?.trim()
  if (alias !== undefined && alias !== '') {
    return matrixToLink(alias)
  }
  const server = serverNameFromRoomId(roomId)
  return matrixToLink(roomId, server === null ? [] : [server])
}

export function matrixToEventLink(roomId: string, eventId: string): string {
  let href = `${MATRIX_TO_ORIGIN}/#/${encodeURIComponent(roomId)}/${encodeURIComponent(eventId)}`
  const server = serverNameFromRoomId(roomId)
  if (server !== null) {
    href += `?via=${encodeURIComponent(server)}`
  }
  return href
}

export function resolveMatrixToRoomLink(
  href: string,
  context: {
    accountId: string
    rooms: readonly RoomDto[]
    roomTitles: ReadonlyMap<string, string>
  },
  label: string = '',
): ResolvedMatrixToRoomLink | null {
  const target = parseMatrixToTarget(href)
  if (target === null || !isRoomMxid(target.mxid)) {
    return null
  }
  const candidates = context.rooms.filter((room) => {
    if (room.account_id !== context.accountId) {
      return false
    }
    if (target.mxid.startsWith('!')) {
      return room.room_id === target.mxid
    }
    return room.canonical_alias === target.mxid
  })
  const ids = new Set(candidates.map((room) => room.room_id))
  if (ids.size !== 1) {
    return null
  }
  const room = candidates[0]
  return {
    href: localRoomHref(context.accountId, room.room_id, target.eventId),
    label: linkLabel(label, href, roomTitle(room, context.roomTitles)),
    isEventLink: target.eventId !== null,
  }
}

export function resolveMatrixToUserLink(
  href: string,
  label: string = '',
): ResolvedMatrixToUserLink | null {
  const target = parseMatrixToTarget(href)
  if (target === null || !target.mxid.startsWith('@')) {
    return null
  }
  return {
    href: matrixToLink(target.mxid),
    label: linkLabel(label, href, localpart(target.mxid)),
  }
}

function parseMatrixToTarget(href: string): MatrixToTarget | null {
  let url: URL
  try {
    url = new URL(href)
  } catch {
    return null
  }
  if (url.origin !== MATRIX_TO_ORIGIN || !url.hash.startsWith('#/')) {
    return null
  }
  const path = url.hash.slice(2)
  const firstSegment = path.split(/[/?]/, 1)[0]
  if (firstSegment === '') {
    return null
  }
  let mxid: string
  try {
    mxid = decodeURIComponent(firstSegment)
  } catch {
    return null
  }
  if (!mxid.startsWith('!') && !mxid.startsWith('#') && !mxid.startsWith('@')) {
    return null
  }
  const eventId = parseMatrixToEventId(path)
  return { mxid, eventId }
}

function isRoomMxid(mxid: string): boolean {
  return mxid.startsWith('!') || mxid.startsWith('#')
}

function parseMatrixToEventId(path: string): string | null {
  const pathOnly = path.split('?', 1)[0]
  const secondSegment = pathOnly.split('/')[1]
  if (secondSegment === undefined || secondSegment === '') {
    return null
  }
  let eventId: string
  try {
    eventId = decodeURIComponent(secondSegment)
  } catch {
    return null
  }
  return eventId.startsWith('$') ? eventId : null
}

function localRoomHref(
  accountId: string,
  roomId: string,
  eventId: string | null,
): string {
  const roomHref = `/${encodeURIComponent(accountId)}/rooms/${encodeURIComponent(roomId)}`
  return eventId === null
    ? roomHref
    : `${roomHref}?event=${encodeURIComponent(eventId)}`
}

function linkLabel(label: string, href: string, roomName: string): string {
  const trimmed = label.trim()
  return trimmed === '' || trimmed === href ? roomName : label
}

function serverNameFromRoomId(roomId: string): string | null {
  const colon = roomId.indexOf(':')
  if (colon === -1 || colon === roomId.length - 1) {
    return null
  }
  return roomId.slice(colon + 1)
}
