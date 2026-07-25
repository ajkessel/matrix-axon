import { useLocation } from 'preact-iso'
import { useEffect, useMemo, useRef, useState } from 'preact/hooks'
import { apiErrorMessage } from '../api/client'
import type { components } from '../api/schema'
import {
  localRoomHref,
  parseMatrixRoomReference,
  serverNameFromRoomReference,
} from '../matrix-to'
import { useServices } from '../services'
import type { Account } from '../stores/accounts'
import type { RoomDto } from '../stores/room-list'

type PublicRoomSummary = components['schemas']['PublicRoomSummaryDto']
type DirectoryPageRequest = {
  since: string | null
  server: string | null
  searchTerm: string
}

const DIRECTORY_LIMIT = 25
const DIRECTORY_SERVER_RECENTS_KEY = 'axon.publicRoomDirectoryServers'
const DIRECTORY_SERVER_SUGGESTIONS = ['matrix.org', 'matrixrooms.info']

/**
 * The `/` route's right pane (ADR 0062). The room list itself lives in the
 * shell sidebar, so this page owns discovery: direct room entry plus public
 * room directory search (M19-W3).
 */
export function RoomsIndex() {
  const { accounts, api, rooms, settings } = useServices()
  const location = useLocation()
  const joinInput = useRef<HTMLInputElement>(null)
  const joinButton = useRef<HTMLButtonElement>(null)
  const directorySearchInput = useRef<HTMLInputElement>(null)
  const activeAccounts = accounts.accounts.value.filter(
    (account) => account.state === 'active',
  )
  const [selectedAccountId, setSelectedAccountId] = useState<string | null>(
    null,
  )
  const selectedAccount =
    activeAccounts.find(
      (account) => account.account_id === selectedAccountId,
    ) ??
    activeAccounts.find(
      (account) => account.account_id === settings.activeAccountId.value,
    ) ??
    activeAccounts[0] ??
    null
  const accountId = selectedAccount?.account_id ?? null
  const homeServerName =
    selectedAccount === null ? null : serverNameFromAccount(selectedAccount)
  const previousAccountId = useRef<string | null>(null)
  const [joinTarget, setJoinTarget] = useState('')
  const [joinStatus, setJoinStatus] = useState<string | null>(null)
  const [joining, setJoining] = useState(false)
  const [searchTerm, setSearchTerm] = useState('')
  const [directoryServer, setDirectoryServer] = useState('')
  const [directoryRecents, setDirectoryRecents] = useState<string[]>(() =>
    loadRecentDirectoryServers(),
  )
  const [directoryResults, setDirectoryResults] = useState<PublicRoomSummary[]>(
    [],
  )
  const [directoryError, setDirectoryError] = useState<string | null>(null)
  const [directoryLoading, setDirectoryLoading] = useState(false)
  const [directorySearched, setDirectorySearched] = useState(false)
  const [nextBatch, setNextBatch] = useState<DirectoryPageRequest | null>(null)
  const [totalEstimate, setTotalEstimate] = useState<number | null>(null)
  const [joiningRoomId, setJoiningRoomId] = useState<string | null>(null)

  const clearDirectoryResults = () => {
    setDirectoryResults([])
    setNextBatch(null)
    setTotalEstimate(null)
  }

  const directoryServerOptions = useMemo(
    () =>
      [...new Set([...DIRECTORY_SERVER_SUGGESTIONS, ...directoryRecents])].sort(
        (a, b) => a.localeCompare(b),
      ),
    [directoryRecents],
  )

  useEffect(() => {
    if (accounts.loading.value) {
      void accounts.refresh()
    }
    if (rooms.loading.value) {
      void rooms.refresh()
    }
  }, [accounts, rooms])
  useEffect(() => {
    if (location.path === '/rooms/discover') {
      joinInput.current?.focus()
    }
  }, [location.path])
  useEffect(() => {
    if (previousAccountId.current === accountId) {
      return
    }
    const previous = previousAccountId.current
    previousAccountId.current = accountId
    if (previous === null) {
      return
    }
    setJoinTarget('')
    setJoinStatus(null)
    setJoining(false)
    setDirectoryError(null)
    setDirectorySearched(false)
    setJoiningRoomId(null)
    clearDirectoryResults()
  }, [accountId])

  const joinReference = async (
    target: string,
    statusLabel: string,
  ): Promise<void> => {
    setJoinStatus(null)
    if (accountId === null) {
      setJoinStatus('Add an active account before joining rooms.')
      return
    }
    const reference = parseMatrixRoomReference(target, {
      allowAliasShorthand: true,
      defaultAliasServerName: homeServerName,
    })
    if (reference === null) {
      setJoinStatus('Enter a room id, alias, Matrix.to link, or matrix: URI.')
      return
    }
    const existingRoomId = joinedRoomIdForReference(
      accountId,
      reference.roomIdOrAlias,
      rooms.rooms.value,
    )
    if (existingRoomId !== null) {
      location.route(
        localRoomHref(accountId, existingRoomId, reference.eventId),
      )
      return
    }
    setJoining(true)
    const result = await rooms.joinRoom(
      accountId,
      reference.roomIdOrAlias,
      reference.serverNames,
    )
    setJoining(false)
    if (!result.ok) {
      setJoinStatus(`Could not join ${statusLabel}: ${result.message}`)
      return
    }
    setJoinTarget('')
    location.route(localRoomHref(accountId, result.roomId, reference.eventId))
  }

  const searchDirectory = async (pageRequest?: DirectoryPageRequest) => {
    setDirectoryError(null)
    const freshSearch = pageRequest === undefined || pageRequest.since === null
    if (accountId === null) {
      if (freshSearch) {
        clearDirectoryResults()
        setDirectorySearched(false)
      }
      setDirectoryError('Add an active account before searching directories.')
      return
    }
    const request =
      pageRequest ??
      ({
        since: null,
        server: normalizeDirectoryServer(directoryServer),
        searchTerm: searchTerm.trim(),
      } satisfies DirectoryPageRequest)
    if (
      pageRequest === undefined &&
      directoryServer.trim() !== '' &&
      request.server === null
    ) {
      clearDirectoryResults()
      setDirectorySearched(false)
      setDirectoryError('Enter a homeserver name, such as matrix.org.')
      return
    }
    if (freshSearch) {
      clearDirectoryResults()
    }
    setDirectoryLoading(true)
    setDirectorySearched(true)
    try {
      const { data, error: apiError } = await api.GET(
        '/v1/accounts/{account_id}/directory/public_rooms',
        {
          params: {
            path: { account_id: accountId },
            query: {
              limit: DIRECTORY_LIMIT,
              ...(request.server === null ? {} : { server: request.server }),
              ...(request.searchTerm === ''
                ? {}
                : { search_term: request.searchTerm }),
              ...(request.since === null ? {} : { since: request.since }),
            },
          },
        },
      )
      if (apiError !== undefined || data === undefined) {
        setDirectoryError(
          apiError === undefined
            ? 'Directory search failed.'
            : apiErrorMessage(apiError),
        )
        if (freshSearch) {
          setDirectorySearched(false)
        }
        return
      }
      const page = data.data
      setDirectoryResults((current) =>
        request.since === null ? page.chunk : [...current, ...page.chunk],
      )
      setNextBatch(
        page.next_batch === undefined || page.next_batch === null
          ? null
          : { ...request, since: page.next_batch },
      )
      setTotalEstimate(page.total_room_count_estimate ?? null)
      if (request.server !== null) {
        const recents = rememberDirectoryServer(request.server)
        if (recents !== null) {
          setDirectoryRecents(recents)
        }
      }
    } catch (cause) {
      setDirectoryError(cause instanceof Error ? cause.message : String(cause))
      if (freshSearch) {
        setDirectorySearched(false)
      }
    } finally {
      setDirectoryLoading(false)
    }
  }

  const joinDirectoryRoom = async (room: PublicRoomSummary) => {
    if (accountId === null) {
      setDirectoryError('Add an active account before joining rooms.')
      return
    }
    const existingRoomId = joinedRoomIdForDirectoryRoom(
      accountId,
      room,
      rooms.rooms.value,
    )
    if (existingRoomId !== null) {
      location.route(localRoomHref(accountId, existingRoomId, null))
      return
    }
    setDirectoryError(null)
    setJoiningRoomId(room.room_id)
    const canonicalAlias = room.canonical_alias ?? null
    const target = canonicalAlias ?? room.room_id
    const server = serverNameFromRoomReference(room.room_id)
    const result = await rooms.joinRoom(
      accountId,
      target,
      canonicalAlias === null && server !== null ? [server] : [],
    )
    setJoiningRoomId(null)
    if (!result.ok) {
      setDirectoryError(`Could not join ${roomLabel(room)}: ${result.message}`)
      return
    }
    location.route(localRoomHref(accountId, result.roomId, null))
  }

  return (
    <div class="page rooms-index room-discovery">
      <h1>Find or Join a Room</h1>
      <p class="muted">
        Join directly by room id, alias, Matrix.to link, or <code>matrix:</code>{' '}
        URI, or search a public room directory.
      </p>

      {activeAccounts.length > 1 && (
        <label class="field-label">
          Account
          <select
            value={selectedAccount?.account_id ?? ''}
            onChange={(event) =>
              setSelectedAccountId(event.currentTarget.value)
            }
          >
            {activeAccounts.map((account) => (
              <option key={account.account_id} value={account.account_id}>
                {account.user_id}
              </option>
            ))}
          </select>
        </label>
      )}

      <section class="panel room-discovery-panel">
        <h2>Join Directly</h2>
        <form
          class="inline-form room-entry-form"
          onSubmit={(event) => {
            event.preventDefault()
            const target = joinInput.current?.value ?? joinTarget
            void joinReference(target, target)
          }}
        >
          <label>
            Room
            <input
              ref={joinInput}
              type="text"
              value={joinTarget}
              placeholder="#room:server, !room:server, matrix:room/..."
              onInput={(event) => setJoinTarget(event.currentTarget.value)}
              onKeyDown={(event) => {
                if (
                  event.key === 'Tab' &&
                  !event.shiftKey &&
                  !event.ctrlKey &&
                  !event.metaKey &&
                  !event.altKey &&
                  event.currentTarget.value.trim() === ''
                ) {
                  event.preventDefault()
                  directorySearchInput.current?.focus()
                } else if (
                  event.key === 'Tab' &&
                  !event.shiftKey &&
                  !event.ctrlKey &&
                  !event.metaKey &&
                  !event.altKey
                ) {
                  event.preventDefault()
                  joinButton.current?.focus()
                }
              }}
            />
          </label>
          <button
            ref={joinButton}
            type="submit"
            disabled={joining || accountId === null}
          >
            {joining ? 'Joining...' : 'Join'}
          </button>
        </form>
        {homeServerName !== null && (
          <p class="muted">
            Short aliases like <code>#room</code> use{' '}
            <code>{homeServerName}</code>.
          </p>
        )}
        {joinStatus !== null && <p class="error">{joinStatus}</p>}
      </section>

      <section class="panel room-discovery-panel">
        <h2>Public Directory</h2>
        <form
          class="room-directory-form"
          onSubmit={(event) => {
            event.preventDefault()
            void searchDirectory()
          }}
        >
          <label>
            Search
            <input
              ref={directorySearchInput}
              type="search"
              value={searchTerm}
              placeholder="Room name, topic, or alias"
              onInput={(event) => setSearchTerm(event.currentTarget.value)}
              onKeyDown={(event) => {
                if (
                  event.key === 'Tab' &&
                  event.shiftKey &&
                  !event.ctrlKey &&
                  !event.metaKey &&
                  !event.altKey
                ) {
                  event.preventDefault()
                  if (joinInput.current?.value.trim() === '') {
                    joinInput.current.focus()
                  } else {
                    joinButton.current?.focus()
                  }
                }
              }}
            />
          </label>
          <label>
            Directory server
            <input
              type="text"
              list="directory-server-options"
              value={directoryServer}
              placeholder={
                homeServerName === null
                  ? 'Account homeserver'
                  : `Account homeserver (${homeServerName})`
              }
              onInput={(event) => setDirectoryServer(event.currentTarget.value)}
            />
          </label>
          <datalist id="directory-server-options">
            {directoryServerOptions.map((server) => (
              <option key={server} value={server} />
            ))}
          </datalist>
          <button
            type="submit"
            disabled={directoryLoading || accountId === null}
          >
            {directoryLoading ? 'Searching...' : 'Search directory'}
          </button>
        </form>
        <p class="muted">
          Leave the server blank for your account homeserver. Try{' '}
          <button
            type="button"
            class="link-button"
            onClick={() => setDirectoryServer('matrix.org')}
          >
            matrix.org
          </button>{' '}
          or{' '}
          <button
            type="button"
            class="link-button"
            onClick={() => setDirectoryServer('matrixrooms.info')}
          >
            matrixrooms.info
          </button>
          ; recent successful directory servers are remembered in this browser.
        </p>
        {directoryError !== null && <p class="error">{directoryError}</p>}
        <DirectoryResults
          accountId={accountId}
          rooms={rooms.rooms.value}
          results={directoryResults}
          searched={directorySearched}
          loading={directoryLoading}
          totalEstimate={totalEstimate}
          joiningRoomId={joiningRoomId}
          onOpen={(roomId) => {
            if (accountId !== null) {
              location.route(localRoomHref(accountId, roomId, null))
            }
          }}
          onJoin={joinDirectoryRoom}
        />
        {nextBatch !== null && (
          <button
            type="button"
            disabled={directoryLoading}
            onClick={() => void searchDirectory(nextBatch)}
          >
            {directoryLoading ? 'Loading...' : 'Load more'}
          </button>
        )}
      </section>
    </div>
  )
}

function DirectoryResults({
  accountId,
  rooms,
  results,
  searched,
  loading,
  totalEstimate,
  joiningRoomId,
  onOpen,
  onJoin,
}: {
  accountId: string | null
  rooms: readonly RoomDto[]
  results: readonly PublicRoomSummary[]
  searched: boolean
  loading: boolean
  totalEstimate: number | null
  joiningRoomId: string | null
  onOpen: (roomId: string) => void
  onJoin: (room: PublicRoomSummary) => Promise<void>
}) {
  if (!searched) {
    return null
  }
  if (loading && results.length === 0) {
    return <p>Searching directory...</p>
  }
  if (results.length === 0) {
    return <p class="muted">No public rooms found.</p>
  }
  return (
    <>
      {totalEstimate !== null && (
        <p class="muted">{totalEstimate.toLocaleString()} rooms estimated.</p>
      )}
      <ul class="cards public-room-results">
        {results.map((room) => {
          const joinedRoomId =
            accountId === null
              ? null
              : joinedRoomIdForDirectoryRoom(accountId, room, rooms)
          return (
            <li key={room.room_id} class="card public-room-card">
              <div class="card-head">
                <strong>{roomLabel(room)}</strong>
                <span class="badge">{room.join_rule}</span>
                {room.room_type !== null && room.room_type !== undefined && (
                  <span class="badge">{room.room_type}</span>
                )}
              </div>
              <div class="card-meta">
                {room.canonical_alias ?? room.room_id}
              </div>
              {room.topic !== null && room.topic !== undefined && (
                <p>{room.topic}</p>
              )}
              <p class="muted">
                {room.num_joined_members.toLocaleString()} members
                {room.world_readable ? ' · world-readable' : ''}
                {room.guest_can_join ? ' · guests can join' : ''}
              </p>
              <div class="card-actions">
                <button
                  type="button"
                  disabled={
                    accountId === null || joiningRoomId === room.room_id
                  }
                  onClick={() =>
                    joinedRoomId === null
                      ? void onJoin(room)
                      : onOpen(joinedRoomId)
                  }
                >
                  {joinedRoomId !== null
                    ? 'Open'
                    : joiningRoomId === room.room_id
                      ? 'Joining...'
                      : 'Join'}
                </button>
              </div>
            </li>
          )
        })}
      </ul>
    </>
  )
}

function serverNameFromAccount(account: Account): string | null {
  const userServer = serverNameFromRoomReference(account.user_id)
  if (userServer !== null) {
    return userServer
  }
  try {
    return new URL(account.homeserver_url).host || null
  } catch {
    return null
  }
}

function joinedRoomIdForReference(
  accountId: string,
  roomIdOrAlias: string,
  rooms: readonly RoomDto[],
): string | null {
  return (
    rooms.find(
      (room) =>
        room.account_id === accountId &&
        (room.room_id === roomIdOrAlias ||
          room.canonical_alias === roomIdOrAlias),
    )?.room_id ?? null
  )
}

function joinedRoomIdForDirectoryRoom(
  accountId: string,
  directoryRoom: PublicRoomSummary,
  rooms: readonly RoomDto[],
): string | null {
  const canonicalAlias = directoryRoom.canonical_alias ?? null
  return (
    rooms.find(
      (room) =>
        room.account_id === accountId &&
        (room.room_id === directoryRoom.room_id ||
          (canonicalAlias !== null && room.canonical_alias === canonicalAlias)),
    )?.room_id ?? null
  )
}

function roomLabel(room: PublicRoomSummary): string {
  return room.name?.trim() || room.canonical_alias || room.room_id
}

function normalizeDirectoryServer(input: string): string | null {
  const trimmed = input.trim()
  if (trimmed === '') {
    return null
  }
  if (/^https?:\/\//i.test(trimmed)) {
    try {
      return new URL(trimmed).host || null
    } catch {
      return null
    }
  }
  if (trimmed.includes('/') || trimmed.includes('@')) {
    return null
  }
  return trimmed
}

function loadRecentDirectoryServers(): string[] {
  const storage = browserLocalStorage()
  if (storage === null) {
    return []
  }
  return parseRecentDirectoryServers(
    storage.getItem(DIRECTORY_SERVER_RECENTS_KEY),
  )
}

function parseRecentDirectoryServers(raw: string | null): string[] {
  if (raw === null) {
    return []
  }
  try {
    const parsed = JSON.parse(raw)
    return Array.isArray(parsed)
      ? parsed.filter((server): server is string => typeof server === 'string')
      : []
  } catch {
    return []
  }
}

function rememberDirectoryServer(server: string): string[] | null {
  const recents = [
    server,
    ...loadRecentDirectoryServers().filter((recent) => recent !== server),
  ].slice(0, 5)
  const storage = browserLocalStorage()
  if (storage === null) {
    return null
  }
  try {
    storage.setItem(DIRECTORY_SERVER_RECENTS_KEY, JSON.stringify(recents))
  } catch {
    return null
  }
  return recents
}

function browserLocalStorage(): Storage | null {
  if (typeof window === 'undefined') {
    return null
  }
  let storage: Storage | undefined
  try {
    storage = window.localStorage
  } catch {
    return null
  }
  return storage !== undefined &&
    typeof storage.getItem === 'function' &&
    typeof storage.setItem === 'function'
    ? storage
    : null
}
