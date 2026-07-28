import type { EventDto } from './stores/timeline'
import { userIdDisplay } from './stores/room-list'

/**
 * Plain-English timeline notices for state events, and the tier that decides
 * whether one is shown (ADR 0083).
 *
 * The interesting half is membership. A Matrix `m.room.member` event carries
 * only the *new* state, so `content.membership: "join"` covers both a real
 * join and someone editing their display name — the bug behind issue #31.
 * ADR 0074 exposes `unsigned.prev_content` on `EventDto` across both the
 * timeline read and the live `/v1/ws` frame; comparing the two is what makes
 * the transition knowable. With no `prev_content` (the first member event for
 * a user, or an older row) we degrade to the new membership alone, which is
 * exactly what the client could say before.
 *
 * Pure functions on purpose: this is the logic worth testing, and it is shared
 * by the timeline filter (`pages/RoomPage.tsx`) and the renderer
 * (`components/EventBody.tsx`).
 */

/**
 * Visibility tier for a state event. `important` is membership only — the
 * same boundary the TUI draws in `should_show_event`
 * (`clients/tui/src/app/timeline.rs`), which shows membership events
 * regardless of its own `show_state_events` toggle. Everything else (topic,
 * power levels, ACLs, room config) is `other`.
 */
export type StateEventTier = 'important' | 'other'

export function stateEventTier(event: EventDto): StateEventTier {
  return event.type === 'm.room.member' ? 'important' : 'other'
}

/** A string field off an opaque `content`/`prev_content`, or `null`. */
function field(content: unknown, key: string): string | null {
  const value = (content as Record<string, unknown> | null | undefined)?.[key]
  return typeof value === 'string' ? value : null
}

/** Trimmed display name, with blank treated as absent. */
function displayName(content: unknown): string | null {
  const name = field(content, 'displayname')?.trim()
  return name === undefined || name === '' ? null : name
}

/**
 * Resolve `userId` to something readable. The member's own event content is
 * preferred over the roster: a user who left the room is no longer in
 * `members`, but their own membership event still carries the name they had.
 */
function nameFor(
  userId: string,
  contents: readonly unknown[],
  resolveName?: (userId: string) => string,
): string {
  for (const content of contents) {
    const name = displayName(content)
    if (name !== null) {
      return name
    }
  }
  // `MembersStore.displayName` echoes the raw id back for a user it doesn't
  // know, which is the one thing we don't want in a sentence.
  const resolved = resolveName?.(userId)?.trim()
  return resolved !== undefined && resolved !== '' && resolved !== userId
    ? resolved
    : userIdDisplay(userId)
}

/**
 * A one-line notice for a state event, or `null` when there is nothing worth
 * saying — a non-membership state event (the caller keeps its raw rendering),
 * or a member event that changed no user-visible field, which Matrix emits
 * routinely and no client should render as a row.
 *
 * `resolveName` is the members-store lookup (`MembersStore.displayName`),
 * consulted only when the event itself carries no name.
 */
export function stateEventNotice(
  event: EventDto,
  resolveName?: (userId: string) => string,
): string | null {
  if (event.type !== 'm.room.member') {
    return null
  }
  const stateKey = event.state_key
  if (stateKey === null || stateKey === undefined || stateKey === '') {
    return null
  }
  const content = event.content
  const prev = event.prev_content
  const membership = field(content, 'membership')
  if (membership === null) {
    return null
  }
  const prevMembership = field(prev, 'membership')

  // The subject's name comes from the event first, newest side first; the
  // actor is whoever sent it, resolvable only through the roster.
  const target = nameFor(stateKey, [content, prev], resolveName)
  const actor =
    event.sender === stateKey ? target : nameFor(event.sender, [], resolveName)

  if (membership === 'join') {
    if (prevMembership === 'join') {
      // A profile change is described by who they *were*, so the "before"
      // side never comes from this event's new content.
      return profileNotice(
        content,
        prev,
        nameFor(stateKey, [prev], resolveName),
      )
    }
    return `${target} joined the room`
  }
  if (membership === 'leave') {
    if (event.sender === stateKey) {
      return prevMembership === 'invite'
        ? `${target} declined the invitation`
        : `${target} left the room`
    }
    if (prevMembership === 'ban') {
      return `${actor} unbanned ${target}`
    }
    if (prevMembership === 'invite') {
      return `${actor} withdrew the invitation to ${target}`
    }
    return `${actor} removed ${target} from the room`
  }
  if (membership === 'ban') {
    return `${actor} banned ${target}`
  }
  if (membership === 'invite') {
    return `${actor} invited ${target}`
  }
  if (membership === 'knock') {
    return `${target} asked to join the room`
  }
  return `${target} membership changed: ${membership}`
}

/**
 * The issue #31 case: membership stayed `join`, so only the profile moved.
 * `null` when neither the display name nor the avatar actually changed.
 */
function profileNotice(
  content: unknown,
  prev: unknown,
  /** Who they were before this event: their old display name, else their id. */
  subject: string,
): string | null {
  const before = displayName(prev)
  const after = displayName(content)
  if (before !== after) {
    if (after === null) {
      return `${subject} removed their display name`
    }
    return before === null
      ? `${subject} set their display name to ${after}`
      : `${subject} changed their display name to ${after}`
  }
  if (field(prev, 'avatar_url') !== field(content, 'avatar_url')) {
    return `${subject} changed their profile picture`
  }
  return null
}
