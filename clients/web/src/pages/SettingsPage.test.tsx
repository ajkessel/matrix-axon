import { cleanup, fireEvent, render, waitFor } from '@testing-library/preact'
import { HttpResponse, http } from 'msw'
import { setupServer } from 'msw/node'
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from 'vitest'
import { ServicesContext } from '../services'
import { roomKey } from '../stores/room-list'
import { TEST_BASE_URL, testServices } from '../test/services'
import { SettingsPage } from './SettingsPage'

const ACCOUNT = '6b53f7f0-0000-4000-8000-000000000001'
const ROOM = '!ops:hs'

const server = setupServer(
  http.get(`${TEST_BASE_URL}/v1/accounts`, () =>
    HttpResponse.json({ data: [] }),
  ),
  http.get(`${TEST_BASE_URL}/v1/status`, () =>
    HttpResponse.json({
      data: { backfill: { paused: false, free_bytes: 0, accounts: [] } },
    }),
  ),
)

beforeAll(() => server.listen({ onUnhandledRequest: 'error' }))
afterEach(() => {
  cleanup()
  server.resetHandlers()
})
afterAll(() => server.close())

describe('SettingsPage', () => {
  it('reflects and updates the theme setting', () => {
    const services = testServices()
    const { getByLabelText } = render(
      <ServicesContext.Provider value={services}>
        <SettingsPage />
      </ServicesContext.Provider>,
    )

    expect((getByLabelText('System') as HTMLInputElement).checked).toBe(true)

    fireEvent.click(getByLabelText('Dark'))

    expect(services.settings.theme.value).toBe('dark')
    expect((getByLabelText('Dark') as HTMLInputElement).checked).toBe(true)
  })

  it('toggles state events, persisted rather than per-room', () => {
    const services = testServices()
    const { getByLabelText } = render(
      <ServicesContext.Provider value={services}>
        <SettingsPage />
      </ServicesContext.Provider>,
    )
    const box = getByLabelText('Show state events') as HTMLInputElement

    expect(box.checked).toBe(false)
    fireEvent.click(box)

    expect(services.settings.showStateEvents.value).toBe(true)
    expect(box.checked).toBe(true)
  })

  it('toggles developer mode', () => {
    const services = testServices()
    const { getByLabelText } = render(
      <ServicesContext.Provider value={services}>
        <SettingsPage />
      </ServicesContext.Provider>,
    )
    const box = getByLabelText('Developer mode') as HTMLInputElement

    expect(box.checked).toBe(false)
    fireEvent.click(box)

    expect(services.settings.developerMode.value).toBe(true)
    expect(box.checked).toBe(true)
  })

  it('includes account management and browser sign-out controls', async () => {
    const services = testServices()
    const { findByRole, getByRole } = render(
      <ServicesContext.Provider value={services}>
        <SettingsPage />
      </ServicesContext.Provider>,
    )

    expect(getByRole('heading', { name: 'Accounts' })).toBeTruthy()
    expect(await findByRole('button', { name: 'Log in' })).toBeTruthy()

    fireEvent.click(getByRole('button', { name: 'Sign out' }))
    expect(services.auth.signedIn.value).toBe(false)
  })

  it('marks all room summaries read after the device-state write lands', async () => {
    let putRequested = false
    let resolvePut!: () => void
    const putGate = new Promise<void>((resolve) => {
      resolvePut = resolve
    })
    server.use(
      http.get(`${TEST_BASE_URL}/v1/rooms`, () =>
        HttpResponse.json({
          data: [
            {
              account_id: ACCOUNT,
              account_user_id: '@me:hs',
              room_id: ROOM,
              name: 'Ops',
              last_activity_ts: 200,
              last_event_id: '$latest',
            },
          ],
        }),
      ),
      http.put(
        `${TEST_BASE_URL}/v1/devices/:deviceId/state/:namespace`,
        () =>
          new Promise<Response>((resolve) => {
            putRequested = true
            void putGate.then(() =>
              resolve(
                HttpResponse.json({
                  data: { updated_at: '2026-07-20T12:00:00Z' },
                }),
              ),
            )
          }),
      ),
    )
    const services = testServices()
    services.unread.recordEvent(roomKey({ account_id: ACCOUNT, room_id: ROOM }))
    const { findByRole } = render(
      <ServicesContext.Provider value={services}>
        <SettingsPage />
      </ServicesContext.Provider>,
    )

    fireEvent.click(await findByRole('button', { name: 'Mark all as read' }))

    expect(await findByRole('button', { name: 'Marking…' })).toBeTruthy()
    expect(services.unread.count(`${ACCOUNT}/${ROOM}`)).toBe(1)
    await waitFor(() => expect(putRequested).toBe(true))
    resolvePut()
    await waitFor(() =>
      expect(services.deviceState.readMarker(ACCOUNT, ROOM)).toEqual({
        eventId: '$latest',
        originTs: 200,
      }),
    )
    await waitFor(() =>
      expect(services.unread.count(`${ACCOUNT}/${ROOM}`)).toBe(0),
    )
    expect(await findByRole('button', { name: 'Mark all as read' })).toBeTruthy()
  })

  it('clears local unread badges when the mark-read write is requeued', async () => {
    vi.useFakeTimers()
    let puts = 0
    server.use(
      http.get(`${TEST_BASE_URL}/v1/rooms`, () =>
        HttpResponse.json({
          data: [
            {
              account_id: ACCOUNT,
              account_user_id: '@me:hs',
              room_id: ROOM,
              name: 'Ops',
              last_activity_ts: 200,
              last_event_id: '$latest',
            },
          ],
        }),
      ),
      http.put(`${TEST_BASE_URL}/v1/devices/:deviceId/state/:namespace`, () => {
        puts += 1
        return HttpResponse.error()
      }),
    )
    const services = testServices()
    services.unread.recordEvent(roomKey({ account_id: ACCOUNT, room_id: ROOM }))
    const { findByRole } = render(
      <ServicesContext.Provider value={services}>
        <SettingsPage />
      </ServicesContext.Provider>,
    )

    fireEvent.click(await findByRole('button', { name: 'Mark all as read' }))

    await waitFor(() =>
      expect(services.deviceState.readMarker(ACCOUNT, ROOM)).toEqual({
        eventId: '$latest',
        originTs: 200,
      }),
    )
    await waitFor(() =>
      expect(services.unread.count(`${ACCOUNT}/${ROOM}`)).toBe(0),
    )
    expect(puts).toBe(1)
    expect(await findByRole('button', { name: 'Mark all as read' })).toBeTruthy()
    vi.clearAllTimers()
    vi.useRealTimers()
  })

  it('shows the web client build version', () => {
    const services = testServices()
    const { getByText } = render(
      <ServicesContext.Provider value={services}>
        <SettingsPage />
      </ServicesContext.Provider>,
    )

    expect(getByText(/Web client/)).toBeTruthy()
  })
})
