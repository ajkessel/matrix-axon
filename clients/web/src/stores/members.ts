import {
  computed,
  signal,
  type ReadonlySignal,
  type Signal,
} from '@preact/signals'
import { apiErrorMessage, type ApiClient } from '../api/client'
import { memberDisplay, type MemberDto } from './room-list'

export interface MembersStore {
  members: ReadonlySignal<ReadonlyMap<string, MemberDto>>
  loading: ReadonlySignal<boolean>
  error: Signal<string | null>

  /** Resolved display name for a sender id, falling back per `memberDisplay`. */
  displayName(userId: string): string

  /** Fetch the room's current member list. */
  refresh(): Promise<void>
}

/**
 * One room's members (ADR 0046), fetched once per room and used to resolve
 * senders in the timeline and thread panel to their displayname (falling
 * back to `@localpart`, then the full user id) instead of the raw Matrix id —
 * the same resolution `dmTitleFromMembers` already does for room titles.
 */
export function createMembersStore(
  api: ApiClient,
  accountId: string,
  roomId: string,
): MembersStore {
  const members = signal<ReadonlyMap<string, MemberDto>>(new Map())
  const loading = signal(true)
  const error = signal<string | null>(null)

  return {
    members: computed(() => members.value),
    loading: computed(() => loading.value),
    error,

    displayName(userId: string): string {
      const member = members.value.get(userId)
      return member !== undefined ? memberDisplay(member) : userId
    },

    async refresh() {
      try {
        const { data, error: apiError } = await api.GET(
          '/v1/accounts/{account_id}/rooms/{room_id}/members',
          { params: { path: { account_id: accountId, room_id: roomId } } },
        )
        if (apiError !== undefined) {
          error.value = apiErrorMessage(apiError)
          return
        }
        error.value = null
        members.value = new Map(
          data.data.map((member) => [member.user_id, member]),
        )
      } catch (cause) {
        error.value = cause instanceof Error ? cause.message : String(cause)
      } finally {
        loading.value = false
      }
    },
  }
}
