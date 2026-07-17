import { useMemo, useState } from 'preact/hooks'
import { inBackground } from '../api/client'
import type { MembersStore } from '../stores/members'
import {
  memberDisplay,
  roomKey,
  roomTitle,
  type MemberDto,
  type RoomDto,
} from '../stores/room-list'
import { ErrorBanner } from './ErrorBanner'
import { UserAvatar } from './UserAvatar'

const MEMBERSHIP_ORDER = new Map([
  ['join', 0],
  ['invite', 1],
  ['leave', 2],
  ['ban', 3],
])

export function RoomInfoPanel({
  accountId,
  roomId,
  room,
  roomTitles,
  members,
  onClose,
}: {
  accountId: string
  roomId: string
  room: RoomDto | undefined
  roomTitles: ReadonlyMap<string, string>
  members: MembersStore
  onClose: () => void
}) {
  const [filter, setFilter] = useState('')
  const displayTitle = room !== undefined ? roomTitle(room, roomTitles) : roomId
  const dmTitle =
    room !== undefined && roomTitles.has(roomKey(room))
      ? (roomTitles.get(roomKey(room)) ?? null)
      : null
  const roster = useMemo(
    () => filteredMembers([...members.members.value.values()], filter),
    [members.members.value, filter],
  )

  return (
    <aside
      id="room-info-panel"
      class="side-panel room-info-panel"
      aria-label="Room information"
    >
      <div class="overlay-head">
        <h2>Room information</h2>
        <button type="button" class="ghost" onClick={onClose}>
          Close
        </button>
      </div>

      <section class="room-info-section" aria-labelledby="room-info-details">
        <h3 id="room-info-details">Details</h3>
        <dl class="detail-list">
          <DetailRow label="Name" value={displayTitle} />
          {dmTitle !== null && dmTitle !== displayTitle && (
            <DetailRow label="DM name" value={dmTitle} />
          )}
          <DetailRow label="Room ID" value={room?.room_id ?? roomId} code />
          <DetailRow
            label="Account ID"
            value={room?.account_id ?? accountId}
            code
          />
          <DetailRow
            label="Your Matrix ID"
            value={room?.account_user_id ?? 'Unavailable from room summary'}
            code={room?.account_user_id !== undefined}
          />
          <DetailRow
            label="Canonical alias"
            value={room?.canonical_alias ?? 'None'}
            code={
              room?.canonical_alias !== undefined &&
              room.canonical_alias !== null
            }
          />
          <DetailRow
            label="Full alias list"
            value="Unavailable from current API"
          />
          <DetailRow label="Topic" value={room?.topic ?? 'None'} />
          <DetailRow label="Avatar" value={room?.avatar_url ?? 'None'} code />
          <DetailRow
            label="Last activity"
            value={
              room !== undefined
                ? new Date(room.last_activity_ts).toLocaleString()
                : 'Unavailable from room summary'
            }
          />
          <DetailRow
            label="Last event"
            value={room?.last_event_id ?? 'None'}
            code={
              room?.last_event_id !== undefined && room.last_event_id !== null
            }
          />
          <DetailRow label="Encryption" value="Unavailable from current API" />
          <DetailRow label="Access" value="Unavailable from current API" />
          <DetailRow
            label="Room type/version"
            value="Unavailable from current API"
          />
        </dl>
      </section>

      <section class="room-info-section" aria-labelledby="room-info-members">
        <div class="room-info-section-head">
          <h3 id="room-info-members">Members</h3>
          <button
            type="button"
            class="ghost"
            disabled={members.loading.value}
            onClick={() => inBackground(members.refresh())}
          >
            {members.loading.value ? 'Refreshing…' : 'Refresh'}
          </button>
        </div>
        <ErrorBanner error={members.error} />
        <label class="member-filter">
          Filter members
          <input
            type="search"
            value={filter}
            placeholder="Name, MXID, membership"
            onInput={(event) => setFilter(event.currentTarget.value)}
          />
        </label>
        {members.loading.value && members.members.value.size === 0 ? (
          <p class="muted">Loading members…</p>
        ) : roster.length === 0 ? (
          <p class="muted">
            {filter.trim() === ''
              ? 'No members available.'
              : 'No members match.'}
          </p>
        ) : (
          <ol class="member-list">
            {roster.map((member) => {
              const display = memberDisplay(member)
              return (
                <li class="member-row" key={member.user_id}>
                  <UserAvatar
                    accountId={accountId}
                    userId={member.user_id}
                    displayName={display}
                    member={member}
                  />
                  <span class="member-copy">
                    <span class="member-name">{display}</span>
                    <code>{member.user_id}</code>
                  </span>
                  <span class={`badge membership-${member.membership}`}>
                    {member.membership || 'unknown'}
                  </span>
                </li>
              )
            })}
          </ol>
        )}
      </section>
    </aside>
  )
}

function DetailRow({
  label,
  value,
  code = false,
}: {
  label: string
  value: string
  code?: boolean
}) {
  return (
    <>
      <dt>{label}</dt>
      <dd>{code ? <code>{value}</code> : value}</dd>
    </>
  )
}

function filteredMembers(members: MemberDto[], filter: string): MemberDto[] {
  const query = filter.trim().toLocaleLowerCase()
  return members
    .filter((member) => {
      if (query === '') {
        return true
      }
      return [memberDisplay(member), member.user_id, member.membership].some(
        (field) => field.toLocaleLowerCase().includes(query),
      )
    })
    .sort((left, right) => {
      const leftRank = MEMBERSHIP_ORDER.get(left.membership) ?? 99
      const rightRank = MEMBERSHIP_ORDER.get(right.membership) ?? 99
      if (leftRank !== rightRank) {
        return leftRank - rightRank
      }
      return (
        memberDisplay(left).localeCompare(memberDisplay(right), undefined, {
          sensitivity: 'base',
        }) || left.user_id.localeCompare(right.user_id)
      )
    })
}
