import { HttpResponse, http } from 'msw'
import { setupServer } from 'msw/node'
import {
  afterAll,
  afterEach,
  beforeAll,
  describe,
  expect,
  it,
  vi,
} from 'vitest'
import { createApiClient } from '../api/client'
import {
  createEphemeralSender,
  RECEIPT_DEBOUNCE_MS,
  TYPING_IDLE_MS,
  TYPING_REFRESH_MS,
} from './ephemeral-sender'

const BASE_URL = 'http://axon.test'
const ACCT = '6b53f7f0-0000-4000-8000-000000000001'
const ROOM = '!room:hs'
const READ_PATH = `${BASE_URL}/v1/accounts/:accountId/rooms/:roomId/read`
const TYPING_PATH = `${BASE_URL}/v1/accounts/:accountId/rooms/:roomId/typing`

const server = setupServer()
beforeAll(() => server.listen({ onUnhandledRequest: 'error' }))
afterEach(() => {
  server.resetHandlers()
  vi.useRealTimers()
})
afterAll(() => server.close())

function makeApi() {
  return createApiClient(
    {
      getToken: () => 'tok',
      onAuthFailure: () => {},
      LoginBootstrap: () => null,
    },
    BASE_URL,
  )
}

/** Collect the `event_id`s the sender POSTs to `.../read`. */
function captureReads(): string[] {
  const reads: string[] = []
  server.use(
    http.post(READ_PATH, async ({ request }) => {
      const body = (await request.json()) as { event_id: string }
      reads.push(body.event_id)
      return HttpResponse.json({ data: {} })
    }),
  )
  return reads
}

/** Collect the `typing` flags the sender PUTs to `.../typing`. */
function captureTyping(): boolean[] {
  const flags: boolean[] = []
  server.use(
    http.put(TYPING_PATH, async ({ request }) => {
      const body = (await request.json()) as { typing: boolean }
      flags.push(body.typing)
      return HttpResponse.json({ data: {} })
    }),
  )
  return flags
}

describe('read receipts', () => {
  it('debounces a burst to one receipt for the newest event', async () => {
    vi.useFakeTimers()
    const reads = captureReads()
    const sender = createEphemeralSender(makeApi())

    sender.noteRead(ACCT, ROOM, '$a', 100)
    sender.noteRead(ACCT, ROOM, '$b', 200)
    // Nothing sent before the debounce elapses.
    await vi.advanceTimersByTimeAsync(RECEIPT_DEBOUNCE_MS - 1)
    expect(reads).toEqual([])
    await vi.advanceTimersByTimeAsync(1)
    expect(reads).toEqual(['$b'])
  })

  it('ignores backward or equal positions (forward-only)', async () => {
    vi.useFakeTimers()
    const reads = captureReads()
    const sender = createEphemeralSender(makeApi())

    sender.noteRead(ACCT, ROOM, '$b', 200)
    sender.noteRead(ACCT, ROOM, '$older', 50) // older than pending → dropped
    await vi.advanceTimersByTimeAsync(RECEIPT_DEBOUNCE_MS)
    expect(reads).toEqual(['$b'])

    // Equal to what was already sent → no new receipt.
    sender.noteRead(ACCT, ROOM, '$b-again', 200)
    await vi.advanceTimersByTimeAsync(RECEIPT_DEBOUNCE_MS)
    expect(reads).toEqual(['$b'])

    // Strictly newer → sends again.
    sender.noteRead(ACCT, ROOM, '$c', 300)
    await vi.advanceTimersByTimeAsync(RECEIPT_DEBOUNCE_MS)
    expect(reads).toEqual(['$b', '$c'])
  })
})

describe('typing notices', () => {
  it('sends typing:true once per throttle window and false on stop', async () => {
    vi.useFakeTimers()
    const flags = captureTyping()
    const sender = createEphemeralSender(makeApi())

    sender.noteTyping(ACCT, ROOM)
    sender.noteTyping(ACCT, ROOM) // within throttle → no second true
    await vi.advanceTimersByTimeAsync(0)
    expect(flags).toEqual([true])

    sender.stopTyping(ACCT, ROOM)
    await vi.advanceTimersByTimeAsync(0)
    expect(flags).toEqual([true, false])
  })

  it('auto-stops after the idle window', async () => {
    vi.useFakeTimers()
    const flags = captureTyping()
    const sender = createEphemeralSender(makeApi())

    sender.noteTyping(ACCT, ROOM)
    await vi.advanceTimersByTimeAsync(0)
    expect(flags).toEqual([true])

    await vi.advanceTimersByTimeAsync(TYPING_IDLE_MS)
    expect(flags).toEqual([true, false])
  })

  it('refreshes typing:true while composition continues', async () => {
    vi.useFakeTimers()
    const flags = captureTyping()
    const sender = createEphemeralSender(makeApi())

    sender.noteTyping(ACCT, ROOM)
    await vi.advanceTimersByTimeAsync(0)
    // Keep typing just before idle would fire, deferring the idle stop.
    await vi.advanceTimersByTimeAsync(TYPING_IDLE_MS - 100)
    sender.noteTyping(ACCT, ROOM)
    // Past the refresh window from the first true → resend.
    await vi.advanceTimersByTimeAsync(
      TYPING_REFRESH_MS - (TYPING_IDLE_MS - 100),
    )
    sender.noteTyping(ACCT, ROOM)
    await vi.advanceTimersByTimeAsync(0)
    expect(flags).toEqual([true, true])
  })

  it('stopTyping is a no-op when no notice is active', async () => {
    vi.useFakeTimers()
    const flags = captureTyping()
    const sender = createEphemeralSender(makeApi())

    sender.stopTyping(ACCT, ROOM)
    await vi.advanceTimersByTimeAsync(0)
    expect(flags).toEqual([])
  })
})
