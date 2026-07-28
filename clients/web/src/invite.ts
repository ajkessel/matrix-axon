import type { MemberMutationResult } from './stores/members'

export function inviteErrorMessage(
  result: Extract<MemberMutationResult, { ok: false }>,
): string {
  switch (result.code) {
    case 'forbidden':
      {
        const alreadyInRoom = alreadyInRoomMessage(result.message)
        if (alreadyInRoom !== null) {
          return `Could not invite users. ${alreadyInRoom}`
        }
      }
      return 'Could not invite users. This account is not allowed to invite people to this room.'
    case 'not_found':
      return 'Could not invite users. Check that the room still exists and the invited users are valid Matrix IDs.'
    case 'bad_request':
      return `Could not invite users. Check the Matrix user IDs: ${result.message}`
    case 'service_unavailable':
      return 'Could not invite users. The selected account is not connected; try again after Axon reconnects.'
    default:
      return `Could not invite users: ${result.message}`
  }
}

function alreadyInRoomMessage(message: string): string | null {
  const match = /(@\S+)\s+is already in the room\b/i.exec(message)
  if (match === null) {
    return null
  }
  return `${match[1]} is already in the room.`
}

export function formatInviteeList(userIds: readonly string[]): string {
  if (userIds.length <= 2) {
    return userIds.join(' and ')
  }
  return `${userIds.slice(0, -1).join(', ')}, and ${userIds[userIds.length - 1]}`
}
