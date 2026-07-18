import {
  cleanup,
  fireEvent,
  render,
  waitFor,
  within,
} from '@testing-library/preact'
import { HttpResponse, http } from 'msw'
import { setupServer } from 'msw/node'
import { LocationProvider, Route, Router } from 'preact-iso'
import { useCallback, useMemo, useState } from 'preact/hooks'
import {
  afterAll,
  afterEach,
  beforeAll,
  describe,
  expect,
  it,
  vi,
} from 'vitest'
import { ServicesContext } from '../services'
import { ShellActionsContext } from '../shell-actions'
import type { MemberDto } from '../stores/room-list'
import type { EventDto } from '../stores/timeline'
import { memoryStorage } from '../test/memory-storage'
import { TEST_BASE_URL, testServices } from '../test/services'
import { RoomPage } from './RoomPage'

const ACCOUNT = '6b53f7f0-0000-4000-8000-000000000001'
const ROOM = '!room:hs'
const TIMELINE_PATH = `${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/rooms/${encodeURIComponent(ROOM)}/timeline`

// `relates_to` and `content` are free-form objects in the contract
// (generated as `Record<string, never>`), so overrides take them loosely
// and cast.
function event(
  id: string,
  ts: number,
  overrides: Partial<Omit<EventDto, 'relates_to' | 'content'>> & {
    relates_to?: unknown
    content?: unknown
  } = {},
): EventDto {
  return {
    account_id: ACCOUNT,
    event_id: id,
    room_id: ROOM,
    sender: '@alice:hs',
    origin_ts: ts,
    type: 'm.room.message',
    body: `body of ${id}`,
    content: { msgtype: 'm.text', body: `body of ${id}` },
    redacted: false,
    edited: false,
    edit_count: 0,
    ...overrides,
  } as EventDto
}

function member(
  user_id: string,
  membership: string,
  display_name: string | null = null,
): MemberDto {
  return { user_id, membership, display_name, avatar_url: null } as MemberDto
}

const DAY = 86_400_000
const T0 = Date.UTC(2026, 5, 1, 12, 0, 0)

const server = setupServer(
  // Threads are refreshed on every room mount; empty by default.
  http.get(
    `${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/rooms/:roomId/threads`,
    () => HttpResponse.json({ data: [] }),
  ),
  // Members are refreshed on every room mount; empty by default.
  http.get(
    `${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/rooms/:roomId/members`,
    () => HttpResponse.json({ data: [] }),
  ),
  // Drafts + read markers are hydrated on every room mount (M-W6 steps 5b/5c);
  // empty by default, and debounced writes are accepted.
  http.get(`${TEST_BASE_URL}/v1/devices/:deviceId/state/:namespace`, () =>
    HttpResponse.json({ data: { namespace: 'drafts', entries: {} } }),
  ),
  http.put(`${TEST_BASE_URL}/v1/devices/:deviceId/state/:namespace`, () =>
    HttpResponse.json({ data: { updated_at: '2026-06-01T12:00:00Z' } }),
  ),
  // Outbound read receipts + typing notices (ADR 0067/0068) fire from the same
  // read/compose choke points; accepted as no-ops by default.
  http.post(`${TEST_BASE_URL}/v1/accounts/:accountId/rooms/:roomId/read`, () =>
    HttpResponse.json({ data: {} }),
  ),
  http.put(`${TEST_BASE_URL}/v1/accounts/:accountId/rooms/:roomId/typing`, () =>
    HttpResponse.json({ data: {} }),
  ),
  http.post(`${TEST_BASE_URL}/v1/accounts/:accountId/utds/redecrypt`, () =>
    HttpResponse.json({
      data: {
        selected: 0,
        attempted: 0,
        decrypted: 0,
        still_pending: 0,
        timed_out: false,
      },
    }),
  ),
)
const originalScrollIntoView = Element.prototype.scrollIntoView
const originalResizeObserver = globalThis.ResizeObserver
let resizeObserverCallback: ResizeObserverCallback | null = null
beforeAll(() => server.listen({ onUnhandledRequest: 'error' }))
afterEach(() => {
  cleanup()
  server.resetHandlers()
  vi.useRealTimers()
  vi.restoreAllMocks()
  if (originalScrollIntoView === undefined) {
    delete (Element.prototype as { scrollIntoView?: Element['scrollIntoView'] })
      .scrollIntoView
  } else {
    Element.prototype.scrollIntoView = originalScrollIntoView
  }
  globalThis.ResizeObserver = originalResizeObserver
  resizeObserverCallback = null
  window.history.replaceState(null, '', '/')
})
afterAll(() => server.close())

/** The routed page under test; Router's types want at least two children. */
function routedRoomPage(services: ReturnType<typeof testServices>) {
  return (
    <ServicesContext.Provider value={services}>
      <LocationProvider>
        <Router>
          <Route path="/:accountId/rooms/:roomId" component={RoomPage} />
          <Route default component={RoomPage} />
        </Router>
      </LocationProvider>
    </ServicesContext.Provider>
  )
}

function routedRoomPageWithJumpButton(
  services: ReturnType<typeof testServices>,
) {
  function Harness() {
    const [jumpAction, setJumpActionState] = useState<(() => void) | null>(null)
    const setJumpAction = useCallback((action: (() => void) | null) => {
      setJumpActionState(() => action)
    }, [])
    const shellActions = useMemo(
      () => ({
        jumpAction,
        setJumpAction,
        roomTitle: null,
        roomInfoAction: null,
        setRoomChrome: () => {},
      }),
      [jumpAction, setJumpAction],
    )
    return (
      <ServicesContext.Provider value={services}>
        <ShellActionsContext.Provider value={shellActions}>
          <button
            type="button"
            disabled={jumpAction === null}
            onClick={() => jumpAction?.()}
          >
            Jump
          </button>
          <LocationProvider>
            <Router>
              <Route path="/:accountId/rooms/:roomId" component={RoomPage} />
              <Route default component={RoomPage} />
            </Router>
          </LocationProvider>
        </ShellActionsContext.Provider>
      </ServicesContext.Provider>
    )
  }
  return <Harness />
}

function renderRoom(
  events: EventDto[],
  options: {
    members?: MemberDto[] | (() => MemberDto[])
    nextCursor?: string | null
    roomsPending?: boolean
    storage?: Storage
    url?: string
  } = {},
) {
  server.use(
    http.get(`${TEST_BASE_URL}/v1/rooms`, () =>
      options.roomsPending
        ? new Promise<Response>(() => {})
        : HttpResponse.json({
            data: [
              {
                account_id: ACCOUNT,
                account_user_id: '@me:hs',
                room_id: ROOM,
                name: 'Ops',
                topic: 'Daily operations',
                avatar_url: 'mxc://hs/avatar',
                canonical_alias: '#ops:hs',
                last_activity_ts: T0,
                last_event_id: '$last',
              },
            ],
          }),
    ),
    http.get(TIMELINE_PATH, () =>
      HttpResponse.json({
        data: { events, next_cursor: options.nextCursor ?? null },
      }),
    ),
    http.get(
      `${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/rooms/:roomId/members`,
      () =>
        HttpResponse.json({
          data:
            typeof options.members === 'function'
              ? options.members()
              : (options.members ?? []),
        }),
    ),
  )
  window.history.replaceState(
    null,
    '',
    options.url ?? `/${ACCOUNT}/rooms/${encodeURIComponent(ROOM)}`,
  )
  const services = testServices({ storage: options.storage })
  const utils = render(routedRoomPage(services))
  return { services, ...utils }
}

function setTimelineScrollGeometry(
  timeline: HTMLElement,
  geometry: { clientHeight: number; scrollHeight: () => number },
) {
  Object.defineProperty(timeline, 'clientHeight', {
    configurable: true,
    get: () => geometry.clientHeight,
  })
  Object.defineProperty(timeline, 'scrollHeight', {
    configurable: true,
    get: geometry.scrollHeight,
  })
}

function installResizeObserver(): void {
  globalThis.ResizeObserver = class ResizeObserver {
    constructor(callback: ResizeObserverCallback) {
      resizeObserverCallback = callback
    }
    observe() {}
    unobserve() {}
    disconnect() {}
  }
}

function triggerObservedResize(): void {
  resizeObserverCallback?.([], {} as ResizeObserver)
}

async function triggerObservedResizeFrame(): Promise<void> {
  triggerObservedResize()
  await new Promise((resolve) => requestAnimationFrame(resolve))
}

describe('RoomPage', () => {
  it('renders the timeline ascending with the room title and day separators', async () => {
    const { findByText, container } = renderRoom([
      event('$2', T0 + DAY),
      event('$1', T0),
    ])

    expect(await findByText('body of $1')).toBeTruthy()
    await waitFor(() =>
      expect(container.querySelector('h1')!.textContent).toBe('Ops'),
    )
    const bodies = [...container.querySelectorAll('.event-body')].map(
      (el) => el.textContent,
    )
    expect(bodies).toEqual(['body of $1', 'body of $2'])
    expect(container.querySelectorAll('.day-separator')).toHaveLength(2)
  })

  it('does not expose the raw room id in the composer while the title loads', async () => {
    const { findByText, getByRole } = renderRoom([event('$1', T0)], {
      roomsPending: true,
    })

    await findByText('body of $1')
    const composer = getByRole('textbox', { name: 'Message' })

    expect((composer as HTMLTextAreaElement).placeholder).toBe('Message')
    expect((composer as HTMLTextAreaElement).placeholder).not.toContain(ROOM)
  })

  it('uses the cached room title while the room list loads', async () => {
    const storage = memoryStorage({
      'axon.token': 'tok-test',
      'axon.room_titles.v1': JSON.stringify({
        version: 1,
        titles: [[[ACCOUNT, ROOM].join('/'), 'Cached Ops']],
      }),
    })
    const { findByText, getByRole } = renderRoom([event('$1', T0)], {
      roomsPending: true,
      storage,
    })

    await findByText('body of $1')
    const composer = getByRole('textbox', { name: 'Message Cached Ops' })
    expect((composer as HTMLTextAreaElement).placeholder).toBe(
      'Message Cached Ops',
    )
  })

  it('opens room information from the title button', async () => {
    const { findByLabelText, findByRole } = renderRoom([event('$1', T0)], {
      members: [
        member('@left:hs', 'leave', 'Left User'),
        member('@alice:hs', 'join', 'Alice'),
        member('@carol:hs', 'invite', 'Carol'),
      ],
    })

    await findByLabelText('Message Ops')
    const titleButton = await findByRole('button', {
      name: 'Open room information',
    })
    fireEvent.click(titleButton)

    const panel = await findByRole('complementary', {
      name: 'Room information',
    })
    expect(within(panel).getByText('Daily operations')).toBeTruthy()
    expect(within(panel).getByText('#ops:hs')).toBeTruthy()
    expect(
      within(panel).getAllByText('Unavailable from current API').length,
    ).toBeGreaterThan(0)
    expect(within(panel).getByText('Alice')).toBeTruthy()
  })

  it('sorts and filters the room information member roster', async () => {
    const { container, findByLabelText, findByRole } = renderRoom(
      [event('$1', T0)],
      {
        members: [
          member('@zara:hs', 'leave', 'Zara'),
          member('@bob:hs', 'join', 'Bob'),
          member('@alice:hs', 'join', 'Alice'),
          member('@ivy:hs', 'invite', 'Ivy'),
        ],
      },
    )

    await findByLabelText('Message Ops')
    fireEvent.click(
      await findByRole('button', { name: 'Open room information' }),
    )

    const panel = await findByLabelText('Room information')
    await within(panel).findByText('Alice')
    expect(
      [...container.querySelectorAll('.member-row')].map(
        (row) => row.querySelector('.member-name')?.textContent,
      ),
    ).toEqual(['Alice', 'Bob', 'Ivy', 'Zara'])

    fireEvent.input(await findByLabelText('Filter members'), {
      target: { value: 'invite' },
    })

    await waitFor(() =>
      expect(
        [...container.querySelectorAll('.member-row')].map(
          (row) => row.querySelector('.member-name')?.textContent,
        ),
      ).toEqual(['Ivy']),
    )
  })

  it('refreshes members from the room information panel', async () => {
    let calls = 0
    const { findByLabelText, findByRole } = renderRoom([event('$1', T0)], {
      members: () => {
        calls += 1
        return calls === 1
          ? [member('@alice:hs', 'join', 'Alice')]
          : [
              member('@alice:hs', 'join', 'Alice'),
              member('@bob:hs', 'join', 'Bob'),
            ]
      },
    })

    await findByLabelText('Message Ops')
    fireEvent.click(
      await findByRole('button', { name: 'Open room information' }),
    )
    const panel = await findByLabelText('Room information')
    expect(await within(panel).findByText('Alice')).toBeTruthy()

    fireEvent.click(await findByRole('button', { name: 'Refresh' }))

    expect(await within(panel).findByText('Bob')).toBeTruthy()
  })

  it('/whereami opens room information and clears the composer', async () => {
    const { findByLabelText, findByRole } = renderRoom([event('$1', T0)])
    const textarea = (await findByLabelText(
      'Message Ops',
    )) as HTMLTextAreaElement

    fireEvent.input(textarea, { target: { value: '/whereami' } })
    fireEvent.keyDown(textarea, { key: 'Enter' })

    expect(
      await findByRole('complementary', { name: 'Room information' }),
    ).toBeTruthy()
    expect(textarea.value).toBe('')
  })

  it('shows placeholders for UTDs and redacted events', async () => {
    const { findByText } = renderRoom([
      event('$utd', T0, {
        type: 'm.room.encrypted',
        content: null,
        body: null,
      }),
      event('$gone', T0 + 1, {
        redacted: true,
        content: null,
        body: null,
        redaction_event_id: '$r',
      }),
    ])

    expect(await findByText('unable to decrypt')).toBeTruthy()
    expect(await findByText('message deleted')).toBeTruthy()
  })

  it('retries UTD decryption once and reloads when rows decrypt', async () => {
    let timelineCalls = 0
    let redecryptCalls = 0
    server.use(
      http.get(TIMELINE_PATH, () => {
        timelineCalls += 1
        return HttpResponse.json({
          data: {
            events:
              timelineCalls === 1
                ? [
                    event('$utd', T0, {
                      type: 'm.room.encrypted',
                      content: null,
                      body: null,
                    }),
                  ]
                : [event('$utd', T0, { body: 'decrypted body' })],
            next_cursor: null,
          },
        })
      }),
      http.post(
        `${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/utds/redecrypt`,
        () => {
          redecryptCalls += 1
          return HttpResponse.json({
            data: {
              selected: 1,
              attempted: 1,
              decrypted: 1,
              still_pending: 0,
              timed_out: false,
            },
          })
        },
      ),
    )

    server.use(
      http.get(`${TEST_BASE_URL}/v1/rooms`, () =>
        HttpResponse.json({
          data: [
            {
              account_id: ACCOUNT,
              account_user_id: '@me:hs',
              room_id: ROOM,
              name: 'Ops',
              last_activity_ts: T0,
            },
          ],
        }),
      ),
    )
    window.history.replaceState(
      null,
      '',
      `/${ACCOUNT}/rooms/${encodeURIComponent(ROOM)}`,
    )
    const { findByText } = render(routedRoomPage(testServices()))

    expect(await findByText('decrypted body')).toBeTruthy()
    expect(redecryptCalls).toBe(1)
    expect(timelineCalls).toBe(2)
  })

  it('hides unsupported bodyless events outside developer mode', async () => {
    const { findByText, queryByText, container } = renderRoom([
      event('$call', T0, {
        type: 'm.call.invite',
        body: null,
        content: { call_id: 'call-1' },
      }),
    ])

    expect(await findByText('No displayable events on this page.')).toBeTruthy()
    expect(queryByText('unsupported event: m.call.invite')).toBeNull()
    expect(container.querySelector('[data-event-id="$call"]')).toBeNull()
  })

  it('shows per-event diagnostics only in developer mode', async () => {
    const { services, findByText, findByRole, queryByRole, queryByText } =
      renderRoom([
        event('$debug', T0, {
          type: 'm.call.invite',
          body: null,
          content: { call_id: 'call-1' },
          sender_trust: 'verified',
        }),
      ])

    expect(await findByText('No displayable events on this page.')).toBeTruthy()
    expect(queryByText('unsupported event: m.call.invite')).toBeNull()
    expect(queryByRole('button', { name: 'Inspect' })).toBeNull()

    services.settings.developerMode.value = true
    expect(await findByText('unsupported event: m.call.invite')).toBeTruthy()
    fireEvent.click(await findByRole('button', { name: 'Inspect' }))

    const inspector = await findByRole('region', {
      name: 'Event diagnostics for $debug',
    })
    expect(inspector.textContent).toContain('"event_id": "$debug"')
    expect(inspector.textContent).toContain('"type": "m.call.invite"')
    expect(inspector.textContent).toContain('"call_id": "call-1"')
    expect(inspector.textContent).toContain('"sender_trust": "verified"')
  })

  it('hides state events unless the setting is on', async () => {
    // The toggle lives in Settings now, so the room reads the persisted
    // preference rather than owning an ephemeral checkbox.
    const { services, findByText, queryByText } = renderRoom([
      event('$msg', T0),
      event('$member', T0 + 1, {
        type: 'm.room.member',
        state_key: '@bob:hs',
        content: { membership: 'join' },
        body: null,
      }),
    ])

    expect(await findByText('body of $msg')).toBeTruthy()
    expect(queryByText(/m\.room\.member/)).toBeNull()

    services.settings.showStateEvents.value = true
    expect(await findByText(/m\.room\.member/)).toBeTruthy()

    services.settings.showStateEvents.value = false
    await waitFor(() => expect(queryByText(/m\.room\.member/)).toBeNull())
  })

  it('no longer offers a state-events checkbox in the room header', async () => {
    const { findByText, queryByLabelText } = renderRoom([event('$msg', T0)])
    await findByText('body of $msg')

    expect(queryByLabelText('State events')).toBeNull()
    expect(queryByLabelText('Jump to date')).toBeNull()
  })

  it('jumps to the selected local calendar day from the shell action', async () => {
    let seenAtTs: string | null = null
    const expectedStartTs = new Date(2026, 4, 20).getTime()
    const expectedAtTs = new Date(2026, 4, 21).getTime() - 1 // end of local 2026-05-20
    server.use(
      http.get(`${TEST_BASE_URL}/v1/rooms`, () =>
        HttpResponse.json({
          data: [
            {
              account_id: ACCOUNT,
              account_user_id: '@me:hs',
              room_id: ROOM,
              name: 'Ops',
              topic: null,
              avatar_url: null,
              canonical_alias: null,
              last_activity_ts: T0,
              last_event_id: '$latest',
            },
          ],
        }),
      ),
      http.get(TIMELINE_PATH, ({ request }) => {
        seenAtTs = new URL(request.url).searchParams.get('at_ts')
        return HttpResponse.json({
          data:
            seenAtTs === null
              ? { events: [event('$latest', T0)], next_cursor: null }
              : {
                  events: [event('$old', expectedStartTs + 1)],
                  next_cursor: null,
                },
        })
      }),
    )
    window.history.replaceState(
      null,
      '',
      `/${ACCOUNT}/rooms/${encodeURIComponent(ROOM)}`,
    )
    const services = testServices()
    const { findByRole, findByText } = render(
      routedRoomPageWithJumpButton(services),
    )
    await findByText('body of $latest')
    const jump = await findByRole('button', { name: 'Jump' })
    await waitFor(() => expect(jump.hasAttribute('disabled')).toBe(false))

    fireEvent.click(jump)
    const dialog = await findByRole('dialog', { name: 'Jump to date' })
    fireEvent.input(within(dialog).getByLabelText('Date'), {
      target: { value: '2026-05-20' },
    })
    fireEvent.click(within(dialog).getByRole('button', { name: 'Jump' }))

    await waitFor(() => expect(seenAtTs).toBe(String(expectedAtTs)))
    expect(await findByText('body of $old')).toBeTruthy()
  })

  it('keeps the jump dialog open on invalid input and closes it with Escape', async () => {
    server.use(
      http.get(`${TEST_BASE_URL}/v1/rooms`, () =>
        HttpResponse.json({
          data: [
            {
              account_id: ACCOUNT,
              account_user_id: '@me:hs',
              room_id: ROOM,
              name: 'Ops',
              topic: null,
              avatar_url: null,
              canonical_alias: null,
              last_activity_ts: T0,
              last_event_id: '$msg',
            },
          ],
        }),
      ),
      http.get(TIMELINE_PATH, () =>
        HttpResponse.json({
          data: { events: [event('$msg', T0)], next_cursor: null },
        }),
      ),
    )
    window.history.replaceState(
      null,
      '',
      `/${ACCOUNT}/rooms/${encodeURIComponent(ROOM)}`,
    )
    const services = testServices()
    const { findByRole, queryByRole, findByText } = render(
      routedRoomPageWithJumpButton(services),
    )
    await findByText('body of $msg')
    const jump = await findByRole('button', { name: 'Jump' })
    await waitFor(() => expect(jump.hasAttribute('disabled')).toBe(false))

    fireEvent.click(jump)
    const dialog = await findByRole('dialog', { name: 'Jump to date' })
    fireEvent.input(within(dialog).getByLabelText('Date'), {
      target: { value: 'not-a-date' },
    })
    fireEvent.click(within(dialog).getByRole('button', { name: 'Jump' }))

    expect(await findByRole('alert')).toBeTruthy()
    fireEvent.keyDown(document.body, { key: 'Escape' })
    await waitFor(() =>
      expect(queryByRole('dialog', { name: 'Jump to date' })).toBeNull(),
    )
  })

  it('/jump opens the jump dialog from the composer', async () => {
    const { findByText, findByLabelText, findByRole } = renderRoom([
      event('$msg', T0),
    ])
    await findByText('body of $msg')
    const composer = await findByLabelText('Message Ops')

    fireEvent.input(composer, { target: { value: '/jump' } })
    fireEvent.keyDown(composer, { key: 'Enter' })

    expect(await findByRole('dialog', { name: 'Jump to date' })).toBeTruthy()
  })

  it('/jump with a date jumps directly from the composer', async () => {
    let seenAtTs: string | null = null
    let scrolledEventId: string | null = null
    Element.prototype.scrollIntoView = function () {
      scrolledEventId =
        this.closest('.event-row')?.getAttribute('data-event-id') ?? null
    }
    const expectedStartTs = new Date(2026, 0, 1).getTime()
    const expectedAtTs = new Date(2026, 0, 2).getTime() - 1 // end of local 2026-01-01
    server.use(
      http.get(`${TEST_BASE_URL}/v1/rooms`, () =>
        HttpResponse.json({
          data: [
            {
              account_id: ACCOUNT,
              account_user_id: '@me:hs',
              room_id: ROOM,
              name: 'Ops',
              topic: null,
              avatar_url: null,
              canonical_alias: null,
              last_activity_ts: T0,
              last_event_id: '$latest',
            },
          ],
        }),
      ),
      http.get(TIMELINE_PATH, ({ request }) => {
        seenAtTs = new URL(request.url).searchParams.get('at_ts')
        return HttpResponse.json({
          data:
            seenAtTs === null
              ? { events: [event('$latest', T0)], next_cursor: null }
              : {
                  events: [
                    event('$late-jan1', expectedAtTs - 1),
                    event('$jan1', expectedStartTs + 1),
                    event('$before-jan1', expectedStartTs - 1),
                  ],
                  next_cursor: null,
                },
        })
      }),
    )
    window.history.replaceState(
      null,
      '',
      `/${ACCOUNT}/rooms/${encodeURIComponent(ROOM)}`,
    )
    const { findByText, findByLabelText, queryByRole } = render(
      routedRoomPage(testServices()),
    )
    await findByText('body of $latest')
    const composer = await findByLabelText('Message Ops')

    fireEvent.input(composer, { target: { value: '/jump 2026-01-01' } })
    fireEvent.keyDown(composer, { key: 'Enter' })

    await waitFor(() => expect(seenAtTs).toBe(String(expectedAtTs)))
    expect(await findByText('body of $jan1')).toBeTruthy()
    expect(scrolledEventId).toBe('$jan1')
    expect(queryByRole('dialog', { name: 'Jump to date' })).toBeNull()
  })

  it('/jump with no messages on that date anchors to the newest earlier visible event', async () => {
    let seenAtTs: string | null = null
    let scrolledEventId: string | null = null
    Element.prototype.scrollIntoView = function () {
      scrolledEventId =
        this.closest('.event-row')?.getAttribute('data-event-id') ?? null
    }
    const juneStart = new Date(2026, 5, 1).getTime()
    const juneEnd = new Date(2026, 5, 2).getTime() - 1
    server.use(
      http.get(`${TEST_BASE_URL}/v1/rooms`, () =>
        HttpResponse.json({
          data: [
            {
              account_id: ACCOUNT,
              account_user_id: '@me:hs',
              room_id: ROOM,
              name: 'Ops',
              topic: null,
              avatar_url: null,
              canonical_alias: null,
              last_activity_ts: T0,
              last_event_id: '$latest',
            },
          ],
        }),
      ),
      http.get(TIMELINE_PATH, ({ request }) => {
        const atTs = new URL(request.url).searchParams.get('at_ts')
        seenAtTs = atTs
        return HttpResponse.json({
          data:
            atTs === null
              ? { events: [event('$latest', T0)], next_cursor: null }
              : {
                  events: [
                    event('$may31', juneStart - 1),
                    event('$may12', juneStart - 20 * DAY),
                  ],
                  next_cursor: null,
                },
        })
      }),
    )
    window.history.replaceState(
      null,
      '',
      `/${ACCOUNT}/rooms/${encodeURIComponent(ROOM)}`,
    )
    const { findByText, findByLabelText } = render(
      routedRoomPage(testServices()),
    )
    await findByText('body of $latest')
    const composer = await findByLabelText('Message Ops')

    fireEvent.input(composer, { target: { value: '/jump 2026-06-01' } })
    fireEvent.keyDown(composer, { key: 'Enter' })

    await waitFor(() => expect(seenAtTs).toBe(String(juneEnd)))
    await waitFor(() => expect(scrolledEventId).toBe('$may31'))
    expect(await findByText('body of $may31')).toBeTruthy()
  })

  it('renders sanitized formatted bodies and reveals spoilers on click', async () => {
    const { findByText, container } = renderRoom([
      event('$fmt', T0, {
        content: {
          msgtype: 'm.text',
          body: 'fallback',
          format: 'org.matrix.custom.html',
          formatted_body:
            '<strong>bold</strong> <span data-mx-spoiler>secret</span><script>x()</script>',
        },
      }),
    ])

    expect(await findByText('bold')).toBeTruthy()
    expect(container.querySelector('script')).toBeNull()

    const spoiler = container.querySelector('.spoiler')!
    expect(spoiler.classList.contains('spoiler-revealed')).toBe(false)
    fireEvent.click(spoiler)
    expect(spoiler.classList.contains('spoiler-revealed')).toBe(true)
  })

  it('renders emote and notice message kinds distinctly', async () => {
    const { findByText, container } = renderRoom([
      event('$emote', T0, {
        content: { msgtype: 'm.emote', body: 'waves' },
        body: 'waves',
      }),
      event('$notice', T0 + 1, {
        content: { msgtype: 'm.notice', body: 'bot says' },
        body: 'bot says',
      }),
    ])

    await findByText(/waves/)
    expect(container.querySelector('.event-body em')).toBeTruthy()
    expect((await findByText('bot says')).closest('.muted')).toBeTruthy()
  })

  it('renders sender avatars through the media proxy', async () => {
    server.use(
      http.get(
        `${TEST_BASE_URL}/v1/media/${ACCOUNT}/hs/alice`,
        () =>
          new HttpResponse('avatar-bytes', {
            headers: { 'content-type': 'image/png' },
          }),
      ),
    )
    const { findByText, container } = renderRoom(
      [event('$avatar', T0, { sender: '@alice:hs' })],
      {
        members: [
          {
            user_id: '@alice:hs',
            membership: 'join',
            display_name: 'Alice',
            avatar_url: 'mxc://hs/alice',
          },
        ],
      },
    )

    expect(await findByText('Alice')).toBeTruthy()
    await waitFor(() => {
      const avatar = container.querySelector<HTMLImageElement>(
        '.event-row .user-avatar img',
      )
      expect(avatar?.src).toMatch(/^blob:/)
    })
  })

  it('renders colored fallback initials for senders without avatars', async () => {
    const { findByText, container } = renderRoom(
      [event('$fallback', T0, { sender: '@alice:hs' })],
      {
        members: [
          {
            user_id: '@alice:hs',
            membership: 'join',
            display_name: 'Alice',
          },
        ],
      },
    )

    expect(await findByText('Alice')).toBeTruthy()
    const avatar = container.querySelector<HTMLElement>(
      '.event-row .user-avatar',
    )!
    expect(avatar.textContent).toBe('A')
    expect(avatar.className).toMatch(/\buser-avatar-color-\d\b/)
    expect(avatar.querySelector('img')).toBeNull()
  })

  it('refreshes sender avatars after a live membership update', async () => {
    server.use(
      http.get(
        `${TEST_BASE_URL}/v1/media/${ACCOUNT}/hs/alice-live`,
        () =>
          new HttpResponse('avatar-bytes', {
            headers: { 'content-type': 'image/png' },
          }),
      ),
    )
    const { services, findByText, container } = renderRoom(
      [event('$avatar-live-target', T0, { sender: '@alice:hs' })],
      {
        members: [
          {
            user_id: '@alice:hs',
            membership: 'join',
            display_name: 'Alice',
          },
        ],
      },
    )

    expect(await findByText('Alice')).toBeTruthy()
    expect(container.querySelector('.event-row .user-avatar img')).toBeNull()

    server.use(
      http.get(
        `${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/rooms/:roomId/members`,
        () =>
          HttpResponse.json({
            data: [
              {
                user_id: '@alice:hs',
                membership: 'join',
                display_name: 'Alice',
                avatar_url: 'mxc://hs/alice-live',
              },
            ],
          }),
      ),
    )
    services.live.start()
    services.sockets[0].emitOpen()
    services.sockets[0].emitMessage(
      JSON.stringify({
        type: 'timeline.event',
        account_id: ACCOUNT,
        payload: event('$avatar-live-member', T0 + 1, {
          type: 'm.room.member',
          sender: '@alice:hs',
          state_key: '@alice:hs',
          content: {
            membership: 'join',
            avatar_url: 'mxc://hs/alice-live',
          },
        }),
      }),
    )

    await waitFor(() => {
      const avatar = container.querySelector<HTMLImageElement>(
        '.event-row .user-avatar img',
      )
      expect(avatar?.src).toMatch(/^blob:/)
    })
  })

  it('shows and clears live typing indicators from ephemeral frames', async () => {
    const { services, findByText, queryByText } = renderRoom(
      [event('$1', T0)],
      {
        members: [
          member('@alice:hs', 'join', 'Alice'),
          member('@bob:hs', 'join', 'Bob'),
        ],
      },
    )
    services.live.start()
    services.sockets[0].emitOpen()

    services.sockets[0].emitMessage(
      JSON.stringify({
        type: 'ephemeral.passthrough',
        account_id: ACCOUNT,
        payload: {
          room_id: ROOM,
          event_type: 'm.typing',
          content: { user_ids: ['@me:hs', '@alice:hs', '@bob:hs'] },
        },
      }),
    )

    expect(await findByText('Alice and Bob are typing')).toBeTruthy()

    services.sockets[0].emitMessage(
      JSON.stringify({
        type: 'ephemeral.passthrough',
        account_id: ACCOUNT,
        payload: {
          room_id: ROOM,
          event_type: 'm.typing',
          content: { user_ids: [] },
        },
      }),
    )
    await waitFor(() =>
      expect(queryByText('Alice and Bob are typing')).toBeNull(),
    )
  })

  it('renders public read receipts on my own messages only', async () => {
    const { services, findByText, queryByText } = renderRoom(
      [
        event('$mine', T0, { sender: '@me:hs', body: 'my message' }),
        event('$theirs', T0 + 1, {
          sender: '@alice:hs',
          body: 'their message',
        }),
      ],
      {
        members: [
          member('@alice:hs', 'join', 'Alice'),
          member('@bob:hs', 'join', 'Bob'),
        ],
      },
    )
    services.live.start()
    services.sockets[0].emitOpen()

    services.sockets[0].emitMessage(
      JSON.stringify({
        type: 'ephemeral.passthrough',
        account_id: ACCOUNT,
        payload: {
          room_id: ROOM,
          event_type: 'm.receipt',
          content: {
            $mine: {
              'm.read': {
                '@me:hs': { ts: 99 },
                '@alice:hs': { ts: 100 },
                '@bob:hs': { ts: 90 },
              },
            },
            $theirs: {
              'm.read': {
                '@bob:hs': { ts: 110 },
              },
            },
          },
        },
      }),
    )

    expect(await findByText('Seen by Alice')).toBeTruthy()
    expect(queryByText('Seen by Bob')).toBeNull()
  })

  it('shows reaction tallies with my reactions highlighted', async () => {
    const { findByText, container } = renderRoom([
      event('$r', T0, {
        reactions: {
          '👍': { count: 2, me: true, senders: ['@a:hs', '@me:hs'] },
          '🎉': { count: 1, me: false, senders: ['@b:hs'] },
        },
      }),
    ])

    expect(await findByText('👍 2')).toBeTruthy()
    expect(await findByText('🎉 1')).toBeTruthy()
    expect(
      container.querySelector('.reaction-chip.mine')!.textContent,
    ).toContain('👍')
  })

  it('shows bounded reaction sender popover with display names', async () => {
    const senders = Array.from(
      { length: 12 },
      (_, index) => `@u${index + 1}:hs`,
    )
    const members = senders.map((user_id, index) => ({
      user_id,
      membership: 'join',
      display_name: `User ${index + 1}`,
    }))
    const { findByRole, findByText } = renderRoom(
      [
        event('$r', T0, {
          reactions: {
            '🔥': { count: 12, me: false, senders },
          },
        }),
      ],
      { members },
    )

    const chip = await findByText('🔥 12')
    fireEvent.mouseEnter(chip.parentElement!)
    expect((await findByRole('tooltip')).textContent).toBe(
      '🔥: User 1, User 2, User 3, User 4, User 5, User 6, User 7, User 8, User 9, User 10, and 2 more',
    )
  })

  it('shows reaction sender popover on touch hold without toggling the reaction', async () => {
    vi.useFakeTimers()
    let reactionPosts = 0
    server.use(
      http.post(
        `${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/rooms/:roomId/events/:eventId/reactions`,
        () => {
          reactionPosts += 1
          return HttpResponse.json({ data: { event_id: '$rx' } })
        },
      ),
    )
    const { findByText, findByRole } = renderRoom([
      event('$r', T0, {
        reactions: {
          '🔥': { count: 1, me: false, senders: ['@alice:hs'] },
        },
      }),
    ])

    const chip = await findByText('🔥 1')
    fireEvent.touchStart(chip)
    vi.advanceTimersByTime(450)
    expect((await findByRole('tooltip')).textContent).toBe('🔥: @alice:hs')

    fireEvent.touchEnd(chip)
    fireEvent.click(chip)
    expect(reactionPosts).toBe(0)
  })

  it('reacts on the tap after a long press that never produced a click', async () => {
    vi.useFakeTimers()
    let reactionPosts = 0
    const reacted = event('$r', T0, {
      reactions: { '🔥': { count: 2, me: true, senders: ['@alice:hs'] } },
    })
    server.use(
      http.post(
        `${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/rooms/:roomId/events/:eventId/reactions`,
        () => {
          reactionPosts += 1
          return HttpResponse.json({ data: { event_id: '$rx' } })
        },
      ),
      // The toggle reconciles by re-reading its target.
      http.get(`${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/events/:eventId`, () =>
        HttpResponse.json({ data: reacted }),
      ),
    )
    const { findByText } = renderRoom([
      event('$r', T0, {
        reactions: {
          '🔥': { count: 1, me: false, senders: ['@alice:hs'] },
        },
      }),
    ])

    // Hold to read the senders, then lift without tapping the chip.
    const chip = await findByText('🔥 1')
    fireEvent.touchStart(chip)
    vi.advanceTimersByTime(450)
    fireEvent.touchEnd(chip)
    vi.useRealTimers()

    // The next tap is a real one and must toggle.
    fireEvent.touchStart(chip)
    fireEvent.touchEnd(chip)
    fireEvent.click(chip)

    await waitFor(() => expect(reactionPosts).toBe(1))
  })

  it('keeps a live reaction on the newest message visible when pinned to bottom', async () => {
    server.use(
      http.get(`${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/events/:eventId`, () =>
        HttpResponse.json({
          data: event('$latest', T0, {
            reactions: {
              '🔥': { count: 1, me: false, senders: ['@bob:hs'] },
            },
          }),
        }),
      ),
    )
    const { services, findByText, container } = renderRoom([
      event('$latest', T0),
    ])
    await findByText('body of $latest')
    const timeline = container.querySelector<HTMLElement>('.timeline')!
    setTimelineScrollGeometry(timeline, {
      clientHeight: 50,
      scrollHeight: () =>
        container.querySelector('.reaction-chip') === null ? 100 : 120,
    })
    timeline.scrollTop = 50

    services.live.start()
    services.sockets[0].emitOpen()
    services.sockets[0].emitMessage(
      JSON.stringify({
        type: 'timeline.event',
        account_id: ACCOUNT,
        payload: event('$rx', T0 + 1, {
          sender: '@bob:hs',
          type: 'm.reaction',
          body: null,
          relates_to: {
            rel_type: 'm.annotation',
            event_id: '$latest',
            key: '🔥',
          },
        }),
      }),
    )

    expect(await findByText('🔥 1')).toBeTruthy()
    await waitFor(() => expect(timeline.scrollTop).toBe(120))
  })

  it('keeps the newest message visible when room content grows after mount', async () => {
    installResizeObserver()
    let scrollHeight = 100
    const { findByText, container } = renderRoom([event('$latest', T0)])
    await findByText('body of $latest')
    const timeline = container.querySelector<HTMLElement>('.timeline')!
    setTimelineScrollGeometry(timeline, {
      clientHeight: 50,
      scrollHeight: () => scrollHeight,
    })
    timeline.scrollTop = 50

    scrollHeight = 130
    await triggerObservedResizeFrame()

    expect(timeline.scrollTop).toBe(130)
  })

  it('does not repin resized room content after the user scrolls back', async () => {
    installResizeObserver()
    let scrollHeight = 100
    const { findByText, container } = renderRoom([event('$latest', T0)])
    await findByText('body of $latest')
    const timeline = container.querySelector<HTMLElement>('.timeline')!
    setTimelineScrollGeometry(timeline, {
      clientHeight: 50,
      scrollHeight: () => scrollHeight,
    })
    timeline.scrollTop = 0
    fireEvent.scroll(timeline)

    scrollHeight = 130
    await triggerObservedResizeFrame()

    expect(timeline.scrollTop).toBe(0)
  })

  it('does not write scrollTop again when resized content is already pinned', async () => {
    installResizeObserver()
    const scrollHeight = 100
    const { findByText, container } = renderRoom([event('$latest', T0)])
    await findByText('body of $latest')
    const timeline = container.querySelector<HTMLElement>('.timeline')!
    setTimelineScrollGeometry(timeline, {
      clientHeight: 50,
      scrollHeight: () => scrollHeight,
    })
    let writes = 0
    let top = 50
    Object.defineProperty(timeline, 'scrollTop', {
      configurable: true,
      get: () => top,
      set: (value) => {
        writes += 1
        top = value
      },
    })

    await triggerObservedResizeFrame()

    expect(writes).toBe(0)
    expect(timeline.scrollTop).toBe(50)
  })

  it('keeps my new reaction on the newest message visible when pinned to bottom', async () => {
    server.use(
      http.post(
        `${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/rooms/:roomId/events/:eventId/reactions`,
        () => HttpResponse.json({ data: { event_id: '$rx' } }),
      ),
      http.get(`${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/events/:eventId`, () =>
        HttpResponse.json({
          data: event('$latest', T0, {
            reactions: {
              '👍': { count: 1, me: true, senders: ['@me:hs'] },
            },
          }),
        }),
      ),
    )
    const { findByRole, findByText, container } = renderRoom([
      event('$latest', T0),
    ])
    await findByText('body of $latest')
    const timeline = container.querySelector<HTMLElement>('.timeline')!
    setTimelineScrollGeometry(timeline, {
      clientHeight: 50,
      scrollHeight: () =>
        container.querySelector('.reaction-chip') === null ? 100 : 120,
    })
    timeline.scrollTop = 50

    fireEvent.click(await findByRole('button', { name: 'React' }))
    fireEvent.click(await findByRole('button', { name: '👍' }))

    expect(await findByText('👍 1')).toBeTruthy()
    await waitFor(() => expect(timeline.scrollTop).toBe(120))
  })

  it('renders reply context from the loaded slice', async () => {
    const { findByText, container } = renderRoom([
      event('$target', T0, { body: 'original message' }),
      event('$reply', T0 + 1, {
        relates_to: { 'm.in_reply_to': { event_id: '$target' } },
      }),
    ])

    await findByText('body of $reply')
    const quote = container.querySelector('.reply-context')!
    expect(quote.textContent).toContain('original message')
  })

  it('renders formatted markdown in reply context anchors', async () => {
    const { findByText, container } = renderRoom([
      event('$target', T0, {
        body: '**bold** anchor',
        content: {
          msgtype: 'm.text',
          body: '**bold** anchor',
          format: 'org.matrix.custom.html',
          formatted_body: '<p><strong>bold</strong> anchor</p>',
        },
      }),
      event('$reply', T0 + 1, {
        relates_to: { 'm.in_reply_to': { event_id: '$target' } },
      }),
    ])

    await findByText('body of $reply')
    const quote = container.querySelector('.reply-context')!
    expect(quote.querySelector('strong')?.textContent).toBe('bold')
    expect(quote.textContent).not.toContain('**bold**')
  })

  it('opens edit history from the (edited) marker', async () => {
    server.use(
      http.get(
        `${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/events/:eventId/edits`,
        () =>
          HttpResponse.json({
            data: [
              event('$e1', T0 + 10, {
                content: {
                  'm.new_content': { msgtype: 'm.text', body: 'first version' },
                },
                body: '* first version',
              }),
            ],
          }),
      ),
    )
    const { findByText, findByRole, getByRole } = renderRoom([
      event('$edited', T0, { edited: true, edit_count: 1 }),
    ])

    fireEvent.click(await findByText('(edited)'))
    expect(await findByRole('dialog')).toBeTruthy()
    expect(await findByText('first version')).toBeTruthy()

    fireEvent.click(getByRole('button', { name: 'Close' }))
  })

  it('load-older prepends the older page', async () => {
    let calls = 0
    server.use(
      http.get(`${TEST_BASE_URL}/v1/rooms`, () =>
        HttpResponse.json({ data: [] }),
      ),
      http.get(TIMELINE_PATH, ({ request }) => {
        calls += 1
        const cursor = new URL(request.url).searchParams.get('cursor')
        if (cursor === null) {
          return HttpResponse.json({
            data: { events: [event('$2', T0 + 1)], next_cursor: 'older' },
          })
        }
        return HttpResponse.json({
          data: { events: [event('$1', T0)], next_cursor: null },
        })
      }),
    )
    window.history.replaceState(
      null,
      '',
      `/${ACCOUNT}/rooms/${encodeURIComponent(ROOM)}`,
    )
    const services = testServices()
    const { findByRole, findByText, container } = render(
      routedRoomPage(services),
    )

    fireEvent.click(await findByRole('button', { name: 'Load older messages' }))

    expect(await findByText('Beginning of room history.')).toBeTruthy()
    const bodies = [...container.querySelectorAll('.event-body')].map(
      (el) => el.textContent,
    )
    expect(bodies).toEqual(['body of $1', 'body of $2'])
    expect(calls).toBe(2)
  })

  it('per-row UI state stays with its message across a load-older prepend (WCR-01)', async () => {
    server.use(
      http.get(`${TEST_BASE_URL}/v1/rooms`, () =>
        HttpResponse.json({ data: [] }),
      ),
      http.get(TIMELINE_PATH, ({ request }) => {
        const cursor = new URL(request.url).searchParams.get('cursor')
        if (cursor === null) {
          return HttpResponse.json({
            data: { events: [event('$2', T0 + 1)], next_cursor: 'older' },
          })
        }
        return HttpResponse.json({
          data: { events: [event('$1', T0)], next_cursor: null },
        })
      }),
    )
    window.history.replaceState(
      null,
      '',
      `/${ACCOUNT}/rooms/${encodeURIComponent(ROOM)}`,
    )
    const services = testServices()
    const { findByRole, findByText, container } = render(
      routedRoomPage(services),
    )

    // Open the reaction picker on $2, then prepend an older page. The rows
    // are keyed fragments; an index-matched reconcile would leave the open
    // picker attached to whatever message lands at that position ($1).
    fireEvent.click(await findByRole('button', { name: 'React' }))
    expect(
      container
        .querySelector('li.event-row[data-event-id="$2"]')!
        .querySelector('.reaction-picker'),
    ).not.toBeNull()

    fireEvent.click(await findByRole('button', { name: 'Load older messages' }))
    expect(await findByText('body of $1')).toBeTruthy()

    expect(
      container
        .querySelector('li.event-row[data-event-id="$2"]')!
        .querySelector('.reaction-picker'),
    ).not.toBeNull()
    expect(
      container
        .querySelector('li.event-row[data-event-id="$1"]')!
        .querySelector('.reaction-picker'),
    ).toBeNull()
  })

  it('opens the reaction picker below the target body and scrolls it into view', async () => {
    const scrollIntoView = vi.fn()
    Element.prototype.scrollIntoView = scrollIntoView
    const { findByRole, container } = renderRoom([event('$1', T0)])

    fireEvent.click(await findByRole('button', { name: 'React' }))

    const row = container.querySelector('li.event-row[data-event-id="$1"]')!
    const body = row.querySelector('.event-body')!
    const picker = row.querySelector('.reaction-picker-shell')!
    expect(
      body.compareDocumentPosition(picker) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).not.toBe(0)
    expect(scrollIntoView).toHaveBeenCalledWith({
      block: 'nearest',
      inline: 'nearest',
    })
  })

  it('?event= deep link jumps to the event and highlights it', async () => {
    server.use(
      http.get(`${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/events/:eventId`, () =>
        HttpResponse.json({ data: event('$old', T0 - 5 * DAY) }),
      ),
    )
    let seenAtTs: string | null = null
    server.use(
      http.get(`${TEST_BASE_URL}/v1/rooms`, () =>
        HttpResponse.json({ data: [] }),
      ),
      http.get(TIMELINE_PATH, ({ request }) => {
        seenAtTs = new URL(request.url).searchParams.get('at_ts')
        return HttpResponse.json({
          data: {
            events: [event('$old', T0 - 5 * DAY)],
            next_cursor: null,
          },
        })
      }),
    )
    window.history.replaceState(
      null,
      '',
      `/${ACCOUNT}/rooms/${encodeURIComponent(ROOM)}?event=%24old`,
    )
    const services = testServices()
    const { findByText, container } = render(routedRoomPage(services))

    expect(await findByText('body of $old')).toBeTruthy()
    expect(seenAtTs).toBe(String(T0 - 5 * DAY))
    await waitFor(() =>
      expect(container.querySelector('.event-row.highlighted')).toBeTruthy(),
    )
  })

  it('a deep link scrolls the highlighted row into view (WCR-09)', async () => {
    const scrolled: { element: Element; options: ScrollIntoViewOptions }[] = []
    // jsdom has no scrollIntoView; install one so the reveal is observable.
    Element.prototype.scrollIntoView = function (options) {
      scrolled.push({
        element: this,
        options: options as ScrollIntoViewOptions,
      })
    }
    try {
      server.use(
        http.get(
          `${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/events/:eventId`,
          () => HttpResponse.json({ data: event('$old', T0 - 5 * DAY) }),
        ),
        http.get(`${TEST_BASE_URL}/v1/rooms`, () =>
          HttpResponse.json({ data: [] }),
        ),
        http.get(TIMELINE_PATH, () =>
          HttpResponse.json({
            data: {
              events: [event('$newer', T0), event('$old', T0 - 5 * DAY)],
              next_cursor: null,
            },
          }),
        ),
      )
      window.history.replaceState(
        null,
        '',
        `/${ACCOUNT}/rooms/${encodeURIComponent(ROOM)}?event=%24old`,
      )
      const services = testServices()
      const { findByText } = render(routedRoomPage(services))

      await findByText('body of $old')
      await waitFor(() =>
        expect(
          scrolled.some(
            ({ element, options }) =>
              element.getAttribute('data-event-id') === '$old' &&
              options.block === 'center' &&
              options.inline === 'nearest',
          ),
        ).toBe(true),
      )
    } finally {
      delete (Element.prototype as { scrollIntoView?: unknown }).scrollIntoView
    }
  })

  it('keeps a search/deep-link target centered when timeline rows resize', async () => {
    installResizeObserver()
    const scrolled: string[] = []
    Element.prototype.scrollIntoView = function () {
      const id = this.getAttribute('data-event-id')
      if (id !== null) {
        scrolled.push(id)
      }
    }
    server.use(
      http.get(`${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/events/:eventId`, () =>
        HttpResponse.json({ data: event('$old', T0 - 5 * DAY) }),
      ),
      http.get(`${TEST_BASE_URL}/v1/rooms`, () =>
        HttpResponse.json({ data: [] }),
      ),
      http.get(TIMELINE_PATH, () =>
        HttpResponse.json({
          data: {
            events: [event('$newer', T0), event('$old', T0 - 5 * DAY)],
            next_cursor: null,
          },
        }),
      ),
    )
    window.history.replaceState(
      null,
      '',
      `/${ACCOUNT}/rooms/${encodeURIComponent(ROOM)}?event=%24old`,
    )
    const { findByText } = render(routedRoomPage(testServices()))

    await findByText('body of $old')
    await waitFor(() => expect(scrolled).toContain('$old'))
    scrolled.length = 0

    await triggerObservedResizeFrame()

    expect(scrolled).toContain('$old')
  })

  it('navigating to ?event= inside an open room jumps without a remount (WCR-09)', async () => {
    const atTsSeen: (string | null)[] = []
    server.use(
      http.get(`${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/events/:eventId`, () =>
        HttpResponse.json({ data: event('$old', T0 - 5 * DAY) }),
      ),
      http.get(`${TEST_BASE_URL}/v1/rooms`, () =>
        HttpResponse.json({ data: [] }),
      ),
      http.get(TIMELINE_PATH, ({ request }) => {
        const atTs = new URL(request.url).searchParams.get('at_ts')
        atTsSeen.push(atTs)
        return HttpResponse.json({
          data:
            atTs === null
              ? { events: [event('$new', T0)], next_cursor: 'c1' }
              : {
                  events: [event('$old', T0 - 5 * DAY)],
                  next_cursor: null,
                },
        })
      }),
    )
    window.history.replaceState(
      null,
      '',
      `/${ACCOUNT}/rooms/${encodeURIComponent(ROOM)}`,
    )
    const services = testServices()
    const { findByText, container } = render(routedRoomPage(services))
    await findByText('body of $new')

    // The user follows an in-room deep link (M-W10 search will route this
    // way): same room, new ?event= — previously ignored because the load
    // effect keyed on the timeline instance alone.
    window.history.pushState(
      null,
      '',
      `/${ACCOUNT}/rooms/${encodeURIComponent(ROOM)}?event=%24old`,
    )
    window.dispatchEvent(new PopStateEvent('popstate'))

    expect(await findByText('body of $old')).toBeTruthy()
    expect(atTsSeen).toContain(String(T0 - 5 * DAY))
    await waitFor(() =>
      expect(
        container
          .querySelector('.event-row.highlighted')
          ?.getAttribute('data-event-id'),
      ).toBe('$old'),
    )
  })

  it('a jumped timeline offers Load newer until it reaches the present', async () => {
    const all = [
      event('$old', T0 - 5 * DAY),
      event('$mid', T0 - 2 * DAY),
      event('$new', T0),
    ]
    server.use(
      http.get(`${TEST_BASE_URL}/v1/accounts/${ACCOUNT}/events/:eventId`, () =>
        HttpResponse.json({ data: event('$old', T0 - 5 * DAY) }),
      ),
      http.get(`${TEST_BASE_URL}/v1/rooms`, () =>
        HttpResponse.json({ data: [] }),
      ),
      // The real endpoint's shape: newest-first, bounded from above by at_ts.
      http.get(TIMELINE_PATH, ({ request }) => {
        const params = new URL(request.url).searchParams
        const atTs = params.get('at_ts')
        let pool = [...all].sort((a, b) => b.origin_ts - a.origin_ts)
        if (atTs !== null) {
          pool = pool.filter((e) => e.origin_ts <= Number(atTs))
        }
        return HttpResponse.json({
          data: { events: pool, next_cursor: null },
        })
      }),
    )
    window.history.replaceState(
      null,
      '',
      `/${ACCOUNT}/rooms/${encodeURIComponent(ROOM)}?event=%24old`,
    )
    const { findByText, findByRole, queryByRole, queryByText } = render(
      routedRoomPage(testServices()),
    )
    await findByText('body of $old')

    // Parked in history: the newest message is not loaded, and the timeline
    // offers the way forward (jsdom has no IntersectionObserver, so the
    // button is the testable path — same contract as "Load older").
    expect(queryByText('body of $new')).toBeNull()
    fireEvent.click(await findByRole('button', { name: 'Load newer messages' }))
    await findByText('body of $new')

    // Caught up: an empty probe retires the affordance.
    fireEvent.click(await findByRole('button', { name: 'Load newer messages' }))
    await waitFor(() =>
      expect(queryByRole('button', { name: 'Load newer messages' })).toBeNull(),
    )
  })

  it('opening the room clears its unread count', async () => {
    const { services } = renderRoom([event('$1', T0)])
    services.unread.recordEvent(`${ACCOUNT}/${ROOM}`)
    // markSeen ran in the mount effect before this record… so re-assert via
    // a fresh mount instead: the effect must clear pre-existing counts.
    cleanup()
    const key = `${ACCOUNT}/${ROOM}`
    expect(services.unread.count(key)).toBe(1)

    render(routedRoomPage(services))
    await waitFor(() => expect(services.unread.count(key)).toBe(0))
  })
})
