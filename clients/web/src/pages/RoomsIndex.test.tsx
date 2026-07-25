import {
  cleanup,
  fireEvent,
  render,
  waitFor,
  within,
} from '@testing-library/preact'
import { LocationProvider } from 'preact-iso'
import { HttpResponse, http } from 'msw'
import { setupServer } from 'msw/node'
import { afterAll, afterEach, beforeAll, describe, expect, it } from 'vitest'
import { ServicesContext } from '../services'
import { TEST_BASE_URL, testServices } from '../test/services'
import { memoryStorage } from '../test/memory-storage'
import { RoomsIndex } from './RoomsIndex'

const ACCOUNT = '6b53f7f0-0000-4000-8000-000000000001'
const ACCOUNT_DTO = {
  account_id: ACCOUNT,
  user_id: '@alice:example.org',
  homeserver_url: 'https://matrix.example.org',
  state: 'active',
  verified: true,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
}
const OTHER_ACCOUNT = '6b53f7f0-0000-4000-8000-000000000002'
const OTHER_ACCOUNT_DTO = {
  ...ACCOUNT_DTO,
  account_id: OTHER_ACCOUNT,
  user_id: '@bob:example.net',
  homeserver_url: 'https://matrix.example.net',
}

const server = setupServer(
  http.get(`${TEST_BASE_URL}/v1/accounts`, () =>
    HttpResponse.json({ data: [ACCOUNT_DTO] }),
  ),
  http.get(`${TEST_BASE_URL}/v1/rooms`, () => HttpResponse.json({ data: [] })),
)
let localStorageMock: Storage
let originalLocalStorage: PropertyDescriptor | undefined

beforeAll(() => {
  server.listen({ onUnhandledRequest: 'error' })
  originalLocalStorage =
    Object.getOwnPropertyDescriptor(window, 'localStorage') ??
    Object.getOwnPropertyDescriptor(Window.prototype, 'localStorage')
})
afterEach(() => {
  cleanup()
  server.resetHandlers()
  localStorageMock?.clear()
  history.replaceState(null, '', '/')
})
afterAll(() => {
  server.close()
  if (originalLocalStorage !== undefined) {
    Object.defineProperty(window, 'localStorage', originalLocalStorage)
  }
})

describe('RoomsIndex discovery', () => {
  it('focuses direct join first and tabs to directory search on the discovery route', async () => {
    history.replaceState(null, '', '/rooms/discover')
    const { findByLabelText } = renderRoomsIndex()

    const roomInput = (await findByLabelText('Room')) as HTMLInputElement
    const searchInput = (await findByLabelText('Search')) as HTMLInputElement

    await waitFor(() => expect(document.activeElement).toBe(roomInput))

    fireEvent.keyDown(roomInput, { key: 'Tab' })

    expect(document.activeElement).toBe(searchInput)
  })

  it('keeps normal tab order to Join when the direct room field has a target', async () => {
    history.replaceState(null, '', '/rooms/discover')
    const { findByLabelText, findByRole, findByText } = renderRoomsIndex()

    await findByText('example.org')
    const roomInput = (await findByLabelText('Room')) as HTMLInputElement
    const joinButton = await findByRole('button', { name: 'Join' })
    roomInput.value = '#ops:example.org'

    fireEvent.keyDown(roomInput, { key: 'Tab' })

    expect(document.activeElement).toBe(joinButton)
  })

  it('shift-tabs from directory search back to the empty direct room field', async () => {
    history.replaceState(null, '', '/rooms/discover')
    const { findByLabelText, findByText } = renderRoomsIndex()

    await findByText('example.org')
    const roomInput = (await findByLabelText('Room')) as HTMLInputElement
    const searchInput = (await findByLabelText('Search')) as HTMLInputElement
    searchInput.focus()

    fireEvent.keyDown(searchInput, { key: 'Tab', shiftKey: true })

    expect(document.activeElement).toBe(roomInput)
  })

  it('shift-tabs from directory search back to Join when direct room has a target', async () => {
    history.replaceState(null, '', '/rooms/discover')
    const { findByLabelText, findByRole, findByText } = renderRoomsIndex()

    await findByText('example.org')
    const roomInput = (await findByLabelText('Room')) as HTMLInputElement
    const searchInput = (await findByLabelText('Search')) as HTMLInputElement
    const joinButton = await findByRole('button', { name: 'Join' })
    roomInput.value = '#ops:example.org'
    searchInput.focus()

    fireEvent.keyDown(searchInput, { key: 'Tab', shiftKey: true })

    expect(document.activeElement).toBe(joinButton)
  })

  it('submits the live direct room value when input state has not committed yet', async () => {
    let joinBody: unknown = null
    server.use(
      http.post(
        `${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/rooms/join`,
        async ({ request }) => {
          joinBody = await request.json()
          return HttpResponse.json({ data: { room_id: '!ops:example.org' } })
        },
      ),
    )
    const { findByLabelText, findByRole, findByText } = renderRoomsIndex()

    await findByText('example.org')
    const roomInput = (await findByLabelText('Room')) as HTMLInputElement
    roomInput.value = '#ops:example.org'
    fireEvent.keyDown(roomInput, { key: 'Tab' })
    fireEvent.click(await findByRole('button', { name: 'Join' }))

    await waitFor(() =>
      expect(window.location.pathname).toBe(
        `/${ACCOUNT}/rooms/${encodeURIComponent('!ops:example.org')}`,
      ),
    )
    expect(joinBody).toEqual({
      room_id_or_alias: '#ops:example.org',
      server_names: [],
    })
  })

  it('joins directly by alias shorthand and routes to the joined room', async () => {
    let joinBody: unknown = null
    server.use(
      http.post(
        `${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/rooms/join`,
        async ({ request }) => {
          joinBody = await request.json()
          return HttpResponse.json({ data: { room_id: '!ops:example.org' } })
        },
      ),
    )
    const { findByLabelText, findByRole, findByText } = renderRoomsIndex()

    await findByText('example.org')
    fireEvent.input(await findByLabelText('Room'), {
      target: { value: '#ops' },
    })
    fireEvent.click(await findByRole('button', { name: 'Join' }))

    await waitFor(() =>
      expect(window.location.pathname).toBe(
        `/${ACCOUNT}/rooms/${encodeURIComponent('!ops:example.org')}`,
      ),
    )
    expect(joinBody).toEqual({
      room_id_or_alias: '#ops:example.org',
      server_names: [],
    })
  })

  it('searches the account homeserver directory and joins a result', async () => {
    let directoryUrl: string | null = null
    let joinBody: unknown = null
    server.use(
      http.get(
        `${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/directory/public_rooms`,
        ({ request }) => {
          directoryUrl = request.url
          return HttpResponse.json({
            data: {
              chunk: [
                {
                  room_id: '!ops:example.org',
                  canonical_alias: '#ops:example.org',
                  name: 'Ops',
                  topic: 'Operations',
                  avatar_url: null,
                  num_joined_members: 42,
                  world_readable: true,
                  guest_can_join: false,
                  join_rule: 'public',
                  room_type: null,
                },
              ],
              next_batch: null,
              prev_batch: null,
              total_room_count_estimate: 1,
            },
          })
        },
      ),
      http.post(
        `${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/rooms/join`,
        async ({ request }) => {
          joinBody = await request.json()
          return HttpResponse.json({ data: { room_id: '!ops:example.org' } })
        },
      ),
    )
    const { findByLabelText, findByRole, findByText } = renderRoomsIndex()

    await findByText('example.org')
    fireEvent.input(await findByLabelText('Search'), {
      target: { value: 'ops' },
    })
    fireEvent.click(await findByRole('button', { name: 'Search directory' }))
    const card = (await findByText('Ops')).closest('.card')!
    fireEvent.click(
      within(card as HTMLElement).getByRole('button', { name: 'Join' }),
    )

    await waitFor(() =>
      expect(window.location.pathname).toBe(
        `/${ACCOUNT}/rooms/${encodeURIComponent('!ops:example.org')}`,
      ),
    )
    expect(directoryUrl).not.toBeNull()
    const directoryQuery = new URL(directoryUrl!).searchParams
    expect(directoryQuery.get('search_term')).toBe('ops')
    expect(directoryQuery.get('server')).toBeNull()
    expect(directoryQuery.get('limit')).toBe('25')
    expect(joinBody).toEqual({
      room_id_or_alias: '#ops:example.org',
      server_names: [],
    })
  })

  it('keeps load-more requests pinned to the submitted directory query', async () => {
    const directoryUrls: string[] = []
    server.use(
      http.get(
        `${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/directory/public_rooms`,
        ({ request }) => {
          directoryUrls.push(request.url)
          const params = new URL(request.url).searchParams
          return HttpResponse.json({
            data: {
              chunk:
                params.get('since') === 'next-ops'
                  ? [
                      {
                        room_id: '!ops-more:example.org',
                        canonical_alias: '#ops-more:example.org',
                        name: 'Ops More',
                        topic: null,
                        avatar_url: null,
                        num_joined_members: 12,
                        world_readable: true,
                        guest_can_join: false,
                        join_rule: 'public',
                        room_type: null,
                      },
                    ]
                  : [
                      {
                        room_id: '!ops:example.org',
                        canonical_alias: '#ops:example.org',
                        name: 'Ops',
                        topic: null,
                        avatar_url: null,
                        num_joined_members: 42,
                        world_readable: true,
                        guest_can_join: false,
                        join_rule: 'public',
                        room_type: null,
                      },
                    ],
              next_batch:
                params.get('since') === 'next-ops' ? null : 'next-ops',
              prev_batch: null,
              total_room_count_estimate: 2,
            },
          })
        },
      ),
    )
    const { findByLabelText, findByRole, findByText } = renderRoomsIndex()

    await findByText('example.org')
    fireEvent.input(await findByLabelText('Search'), {
      target: { value: 'ops' },
    })
    fireEvent.input(await findByLabelText('Directory server'), {
      target: { value: 'matrix.org' },
    })
    fireEvent.click(await findByRole('button', { name: 'Search directory' }))
    await findByText('Ops')

    fireEvent.input(await findByLabelText('Search'), {
      target: { value: 'dev' },
    })
    fireEvent.input(await findByLabelText('Directory server'), {
      target: { value: 'matrixrooms.info' },
    })
    fireEvent.click(await findByRole('button', { name: 'Load more' }))

    await findByText('Ops More')
    expect(directoryUrls).toHaveLength(2)
    const loadMoreQuery = new URL(directoryUrls[1]).searchParams
    expect(loadMoreQuery.get('since')).toBe('next-ops')
    expect(loadMoreQuery.get('search_term')).toBe('ops')
    expect(loadMoreQuery.get('server')).toBe('matrix.org')
  })

  it('clears stale directory results and cursor after a fresh search fails', async () => {
    let requests = 0
    server.use(
      http.get(
        `${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/directory/public_rooms`,
        () => {
          requests += 1
          if (requests > 1) {
            return HttpResponse.json(
              { error: 'directory unavailable' },
              { status: 502 },
            )
          }
          return HttpResponse.json({
            data: {
              chunk: [
                {
                  room_id: '!ops:example.org',
                  canonical_alias: '#ops:example.org',
                  name: 'Ops',
                  topic: null,
                  avatar_url: null,
                  num_joined_members: 42,
                  world_readable: true,
                  guest_can_join: false,
                  join_rule: 'public',
                  room_type: null,
                },
              ],
              next_batch: 'next-ops',
              prev_batch: null,
              total_room_count_estimate: 2,
            },
          })
        },
      ),
    )
    const {
      container,
      findByLabelText,
      findByRole,
      findByText,
      queryByRole,
      queryByText,
    } = renderRoomsIndex()

    await findByText('example.org')
    fireEvent.input(await findByLabelText('Search'), {
      target: { value: 'ops' },
    })
    fireEvent.click(await findByRole('button', { name: 'Search directory' }))
    await findByText('Ops')
    expect(await findByRole('button', { name: 'Load more' })).toBeTruthy()

    fireEvent.input(await findByLabelText('Search'), {
      target: { value: 'broken' },
    })
    fireEvent.click(await findByRole('button', { name: 'Search directory' }))

    await waitFor(() => {
      expect(queryByText('Ops')).toBeNull()
      expect(queryByRole('button', { name: 'Load more' })).toBeNull()
      expect(container.querySelector('.error')?.textContent).not.toBe('')
    })
  })

  it('joins a directory room with an omitted canonical alias by room id and via server', async () => {
    let joinBody: unknown = null
    server.use(
      http.get(`${TEST_BASE_URL}/v1/rooms`, () =>
        HttpResponse.json({
          data: [
            {
              account_id: ACCOUNT,
              account_user_id: ACCOUNT_DTO.user_id,
              room_id: '!other:example.org',
              name: 'Other',
              topic: null,
              last_activity_ts: 0,
            },
          ],
        }),
      ),
      http.get(
        `${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/directory/public_rooms`,
        () =>
          HttpResponse.json({
            data: {
              chunk: [
                {
                  room_id: '!remote:example.org',
                  name: 'Remote',
                  topic: null,
                  avatar_url: null,
                  num_joined_members: 42,
                  world_readable: true,
                  guest_can_join: false,
                  join_rule: 'public',
                  room_type: null,
                },
              ],
              next_batch: null,
              prev_batch: null,
              total_room_count_estimate: 1,
            },
          }),
      ),
      http.post(
        `${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/rooms/join`,
        async ({ request }) => {
          joinBody = await request.json()
          return HttpResponse.json({ data: { room_id: '!remote:example.org' } })
        },
      ),
    )
    const { findByRole, findByText } = renderRoomsIndex()

    await findByText('example.org')
    fireEvent.click(await findByRole('button', { name: 'Search directory' }))
    const card = (await findByText('Remote')).closest('.card')!
    fireEvent.click(
      within(card as HTMLElement).getByRole('button', { name: 'Join' }),
    )

    await waitFor(() =>
      expect(window.location.pathname).toBe(
        `/${ACCOUNT}/rooms/${encodeURIComponent('!remote:example.org')}`,
      ),
    )
    expect(joinBody).toEqual({
      room_id_or_alias: '!remote:example.org',
      server_names: ['example.org'],
    })
  })

  it('opens an already joined directory result without joining again', async () => {
    let joined = false
    server.use(
      http.get(`${TEST_BASE_URL}/v1/rooms`, () =>
        HttpResponse.json({
          data: [
            {
              account_id: ACCOUNT,
              account_user_id: ACCOUNT_DTO.user_id,
              room_id: '!ops:example.org',
              name: 'Ops',
              canonical_alias: '#ops:example.org',
              topic: null,
              last_activity_ts: 0,
            },
          ],
        }),
      ),
      http.get(
        `${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/directory/public_rooms`,
        () =>
          HttpResponse.json({
            data: {
              chunk: [
                {
                  room_id: '!ops:example.org',
                  canonical_alias: '#ops:example.org',
                  name: 'Ops',
                  topic: null,
                  avatar_url: null,
                  num_joined_members: 42,
                  world_readable: true,
                  guest_can_join: false,
                  join_rule: 'public',
                  room_type: null,
                },
              ],
              next_batch: null,
              prev_batch: null,
              total_room_count_estimate: 1,
            },
          }),
      ),
      http.post(`${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/rooms/join`, () => {
        joined = true
        return HttpResponse.json({ data: { room_id: '!ops:example.org' } })
      }),
    )
    const { findByRole, findByText } = renderRoomsIndex()

    await findByText('example.org')
    fireEvent.click(await findByRole('button', { name: 'Search directory' }))
    const card = (await findByText('Ops')).closest('.card')!
    fireEvent.click(
      within(card as HTMLElement).getByRole('button', { name: 'Open' }),
    )

    await waitFor(() =>
      expect(window.location.pathname).toBe(
        `/${ACCOUNT}/rooms/${encodeURIComponent('!ops:example.org')}`,
      ),
    )
    expect(joined).toBe(false)
  })

  it('searches a chosen directory server and remembers it as a recent', async () => {
    let directoryUrl: string | null = null
    server.use(
      http.get(
        `${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/directory/public_rooms`,
        ({ request }) => {
          directoryUrl = request.url
          return HttpResponse.json({
            data: {
              chunk: [],
              next_batch: null,
              prev_batch: null,
              total_room_count_estimate: 0,
            },
          })
        },
      ),
    )
    const { findByLabelText, findByRole, findByText } = renderRoomsIndex()

    await findByText('example.org')
    fireEvent.input(await findByLabelText('Directory server'), {
      target: { value: 'https://matrixrooms.info/' },
    })
    fireEvent.click(await findByRole('button', { name: 'Search directory' }))

    await waitFor(() => expect(directoryUrl).not.toBeNull())
    expect(new URL(directoryUrl!).searchParams.get('server')).toBe(
      'matrixrooms.info',
    )
    expect(window.localStorage.getItem('axon.publicRoomDirectoryServers')).toBe(
      '["matrixrooms.info"]',
    )
  })

  it('keeps successful directory results when remembering a recent server fails', async () => {
    let directoryUrl: string | null = null
    server.use(
      http.get(
        `${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/directory/public_rooms`,
        ({ request }) => {
          directoryUrl = request.url
          return HttpResponse.json({
            data: {
              chunk: [
                {
                  room_id: '!ops:example.org',
                  canonical_alias: '#ops:example.org',
                  name: 'Ops',
                  topic: null,
                  avatar_url: null,
                  num_joined_members: 42,
                  world_readable: true,
                  guest_can_join: false,
                  join_rule: 'public',
                  room_type: null,
                },
              ],
              next_batch: null,
              prev_batch: null,
              total_room_count_estimate: 1,
            },
          })
        },
      ),
    )
    const storage = memoryStorage()
    storage.setItem = () => {
      throw new Error('quota exceeded')
    }
    const { findByLabelText, findByRole, findByText, queryByText } =
      renderRoomsIndex({ storage })

    await findByText('example.org')
    fireEvent.input(await findByLabelText('Directory server'), {
      target: { value: 'matrix.org' },
    })
    fireEvent.click(await findByRole('button', { name: 'Search directory' }))

    expect(await findByText('Ops')).toBeTruthy()
    expect(queryByText('quota exceeded')).toBeNull()
    expect(directoryUrl).not.toBeNull()
    expect(new URL(directoryUrl!).searchParams.get('server')).toBe('matrix.org')
  })

  it('clears account-scoped discovery and join state when switching accounts', async () => {
    server.use(
      http.get(`${TEST_BASE_URL}/v1/accounts`, () =>
        HttpResponse.json({ data: [ACCOUNT_DTO, OTHER_ACCOUNT_DTO] }),
      ),
      http.get(
        `${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/directory/public_rooms`,
        () =>
          HttpResponse.json({
            data: {
              chunk: [
                {
                  room_id: '!ops:example.org',
                  canonical_alias: '#ops:example.org',
                  name: 'Ops',
                  topic: null,
                  avatar_url: null,
                  num_joined_members: 42,
                  world_readable: true,
                  guest_can_join: false,
                  join_rule: 'public',
                  room_type: null,
                },
              ],
              next_batch: 'next-ops',
              prev_batch: null,
              total_room_count_estimate: 2,
            },
          }),
      ),
    )
    const {
      findByLabelText,
      findByRole,
      findByText,
      queryByRole,
      queryByText,
    } = renderRoomsIndex()

    await findByText('example.org')
    const roomInput = (await findByLabelText('Room')) as HTMLInputElement
    fireEvent.input(roomInput, { target: { value: 'not a room' } })
    fireEvent.click(await findByRole('button', { name: 'Join' }))
    await findByText('Enter a room id, alias, Matrix.to link, or matrix: URI.')
    fireEvent.click(await findByRole('button', { name: 'Search directory' }))
    await findByText('Ops')
    expect(await findByRole('button', { name: 'Load more' })).toBeTruthy()

    fireEvent.change(await findByLabelText('Account'), {
      target: { value: OTHER_ACCOUNT },
    })

    await waitFor(() => {
      expect(roomInput.value).toBe('')
      expect(queryByText('Ops')).toBeNull()
      expect(queryByRole('button', { name: 'Load more' })).toBeNull()
      expect(
        queryByText('Enter a room id, alias, Matrix.to link, or matrix: URI.'),
      ).toBeNull()
    })
  })
})

function renderRoomsIndex({
  storage = memoryStorage(),
}: { storage?: Storage } = {}) {
  localStorageMock = storage
  Object.defineProperty(window, 'localStorage', {
    configurable: true,
    value: localStorageMock,
  })
  const services = testServices()
  return render(
    <ServicesContext.Provider value={services}>
      <LocationProvider>
        <RoomsIndex />
      </LocationProvider>
    </ServicesContext.Provider>,
  )
}
