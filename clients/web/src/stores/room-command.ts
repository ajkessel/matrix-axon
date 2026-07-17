import type { RoomDto } from './room-list'

export interface RoomCommandSuggestion {
  room: RoomDto
  completion: string
  matched: string
  detail: string
}

export type RoomTargetResolution =
  | { kind: 'match'; room: RoomDto }
  | { kind: 'ambiguous'; options: string[] }
  | { kind: 'missing' }

export function roomCommandSuggestions(
  rooms: readonly RoomDto[],
  target: string,
): RoomCommandSuggestion[] {
  return visibleRoomsForCompletion(rooms)
    .filter((room) => roomMatchesCompletion(room, target))
    .map((room) => ({
      room,
      completion: roomCompletionValue(room),
      matched:
        roomMatchingPrefixValue(room, target) ?? roomCompletionValue(room),
      detail: roomDetail(room),
    }))
}

export function resolveRoomTarget(
  rooms: readonly RoomDto[],
  target: string,
): RoomTargetResolution {
  const trimmed = target.trim()
  const visible = visibleRoomsForCompletion(rooms)
  const index = Number.parseInt(trimmed, 10)
  if (/^\d+$/.test(trimmed)) {
    const room = visible[index - 1]
    return room === undefined ? { kind: 'missing' } : { kind: 'match', room }
  }

  const targetLower = trimmed.toLowerCase()
  const exact = visible.filter(
    (room) =>
      room.room_id === trimmed ||
      room.canonical_alias === trimmed ||
      room.name?.toLowerCase() === targetLower,
  )
  const exactResolution = classifyRoomMatches(trimmed, exact)
  if (exactResolution !== null) {
    return exactResolution
  }

  const alias = roomAliasWithHash(trimmed)
  if (alias !== null) {
    const aliasMatches = visible.filter(
      (room) => room.canonical_alias === alias,
    )
    const aliasResolution = classifyRoomMatches(trimmed, aliasMatches)
    if (aliasResolution !== null) {
      return aliasResolution
    }
  }

  const targetLocal = incompleteMatrixRoomName(trimmed)
  if (targetLocal !== null) {
    const localMatches = visible.filter((room) => {
      const aliasLocal =
        room.canonical_alias == null
          ? null
          : matrixRoomLocalName(room.canonical_alias)
      return (
        equalsIgnoreCase(aliasLocal, targetLocal) ||
        equalsIgnoreCase(matrixRoomLocalName(room.room_id ?? ''), targetLocal)
      )
    })
    const localResolution = classifyRoomMatches(trimmed, localMatches)
    if (localResolution !== null) {
      return localResolution
    }
  }

  const prefixMatches = visible.filter((room) =>
    roomMatchesCompletion(room, trimmed),
  )
  return classifyRoomMatches(trimmed, prefixMatches) ?? { kind: 'missing' }
}

function visibleRoomsForCompletion(rooms: readonly RoomDto[]): RoomDto[] {
  const result: RoomDto[] = []
  const seen = new Map<string, number>()
  for (const room of rooms) {
    const roomId = room.room_id ?? ''
    const previous = seen.get(roomId)
    if (previous === undefined) {
      seen.set(roomId, result.length)
      result.push(room)
    } else if (
      result[previous].canonical_alias == null &&
      room.canonical_alias != null
    ) {
      result[previous] = room
    }
  }
  return result
}

function classifyRoomMatches(
  target: string,
  matches: readonly RoomDto[],
): RoomTargetResolution | null {
  if (matches.length === 0) {
    return null
  }
  if (matches.length === 1) {
    return { kind: 'match', room: matches[0] }
  }
  return {
    kind: 'ambiguous',
    options: matches.map((room) => roomResolutionOption(room, target)),
  }
}

function roomResolutionOption(room: RoomDto, target: string): string {
  const value =
    roomMatchingPrefixValue(room, target) ?? roomCompletionValue(room)
  return caseInsensitiveSuffix(value, target) || value
}

function roomCompletionValue(room: RoomDto): string {
  return room.canonical_alias ?? room.name ?? room.room_id ?? ''
}

function roomDetail(room: RoomDto): string {
  const display = roomCompletionValue(room)
  if (room.canonical_alias != null && room.canonical_alias !== display) {
    return room.canonical_alias
  }
  return room.room_id ?? ''
}

function roomMatchingPrefixValue(room: RoomDto, target: string): string | null {
  const trimmed = target.trim()
  if (trimmed === '') {
    return roomCompletionValue(room)
  }
  const fields = [room.name, room.canonical_alias, room.room_id]
  for (const field of fields) {
    if (startsWithIgnoreCase(field, trimmed)) {
      return field
    }
  }
  const alias = roomAliasWithHash(trimmed)
  if (alias !== null && startsWithIgnoreCase(room.canonical_alias, alias)) {
    return room.canonical_alias
  }
  const targetLocal = incompleteMatrixRoomName(trimmed)
  if (targetLocal === null) {
    return null
  }
  const aliasLocal =
    room.canonical_alias == null
      ? null
      : matrixRoomLocalName(room.canonical_alias)
  if (startsWithIgnoreCase(aliasLocal, targetLocal)) {
    return aliasLocal
  }
  const roomLocal = matrixRoomLocalName(room.room_id ?? '')
  return startsWithIgnoreCase(roomLocal, targetLocal) ? roomLocal : null
}

function roomMatchesCompletion(room: RoomDto, target: string): boolean {
  return roomMatchingPrefixValue(room, target) !== null
}

function incompleteMatrixRoomName(target: string): string | null {
  const trimmed = target.trim()
  if (trimmed === '' || trimmed.includes(':')) {
    return null
  }
  const local = trimmed.replace(/^[#!]+/, '')
  return local === '' ? null : local
}

function roomAliasWithHash(target: string): string | null {
  const trimmed = target.trim()
  if (
    trimmed === '' ||
    trimmed.startsWith('#') ||
    trimmed.startsWith('!') ||
    !trimmed.includes(':')
  ) {
    return null
  }
  return `#${trimmed}`
}

function matrixRoomLocalName(value: string): string | null {
  if (!value.startsWith('#') && !value.startsWith('!')) {
    return null
  }
  return value.slice(1).split(':', 1)[0] ?? null
}

function startsWithIgnoreCase(
  value: string | null | undefined,
  prefix: string,
): value is string {
  return value?.toLowerCase().startsWith(prefix.toLowerCase()) ?? false
}

function equalsIgnoreCase(
  value: string | null | undefined,
  target: string,
): boolean {
  return value?.toLowerCase() === target.toLowerCase()
}

function caseInsensitiveSuffix(candidate: string, prefix: string): string {
  return candidate.slice(0, prefix.length).toLowerCase() ===
    prefix.toLowerCase()
    ? candidate.slice(prefix.length)
    : ''
}
