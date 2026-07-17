import { cleanup, fireEvent, render, waitFor } from '@testing-library/preact'
import { HttpResponse, http } from 'msw'
import { setupServer } from 'msw/node'
import { afterAll, afterEach, beforeAll, describe, expect, it } from 'vitest'
import { ServicesContext, type AppServices } from '../services'
import { TEST_BASE_URL, testServices } from '../test/services'
import { AccountsPage } from './AccountsPage'

const ALICE = {
  account_id: '6b53f7f0-0000-4000-8000-000000000001',
  user_id: '@alice:example.org',
  homeserver_url: 'https://matrix.example.org',
  state: 'active',
  verified: true,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
}
const BOB = {
  ...ALICE,
  account_id: '6b53f7f0-0000-4000-8000-000000000002',
  user_id: '@bob:example.org',
  state: 'deactivated',
  verified: null,
}
const STATUS = {
  backfill: {
    paused: true,
    reason: 'low_disk',
    free_bytes: 2 * 1024 ** 3,
    accounts: [
      {
        account_id: ALICE.account_id,
        events: 1234,
        rooms_total: 10,
        rooms_backfilled: 7,
        complete: false,
      },
    ],
  },
}

const server = setupServer()
beforeAll(() => server.listen({ onUnhandledRequest: 'error' }))
afterEach(() => {
  cleanup()
  server.resetHandlers()
})
afterAll(() => server.close())

function renderPage(accounts: unknown[] = [ALICE, BOB]) {
  server.use(
    http.get(`${TEST_BASE_URL}/v1/accounts`, () =>
      HttpResponse.json({ data: accounts }),
    ),
    http.get(`${TEST_BASE_URL}/v1/status`, () =>
      HttpResponse.json({ data: STATUS }),
    ),
  )
  const services: AppServices = testServices()
  const utils = render(
    <ServicesContext.Provider value={services}>
      <AccountsPage />
    </ServicesContext.Provider>,
  )
  return { services, ...utils }
}

describe('AccountsPage', () => {
  it('lists accounts with state and verification badges', async () => {
    const { findByText, getByText } = renderPage()

    expect(await findByText('@alice:example.org')).toBeTruthy()
    expect(getByText('@bob:example.org')).toBeTruthy()
    expect(getByText('verified')).toBeTruthy()
    expect(getByText('deactivated')).toBeTruthy()
  })

  it('shows the backfill status including the paused warning', async () => {
    const { findByText, getByText } = renderPage()
    expect(await findByText('paused (low_disk)')).toBeTruthy()
    expect(getByText(/7\/10 rooms backfilled/)).toBeTruthy()
  })

  it('logs in through the add-account form', async () => {
    let loginBody: unknown
    let recoverBody: unknown
    const { findByText, getByLabelText, getByRole } = renderPage([])
    server.use(
      http.post(`${TEST_BASE_URL}/v1/accounts/login`, async ({ request }) => {
        loginBody = await request.json()
        return HttpResponse.json({ data: ALICE })
      }),
      http.post(
        `${TEST_BASE_URL}/v1/accounts/${ALICE.account_id}/recover`,
        async ({ request }) => {
          recoverBody = await request.json()
          return HttpResponse.json({ data: { ...ALICE, verified: true } })
        },
      ),
    )
    await findByText('No accounts yet — add one below.')

    fireEvent.input(getByLabelText(/Matrix user ID/), {
      target: { value: '@alice:example.org' },
    })
    fireEvent.input(getByLabelText('Password'), {
      target: { value: 'hunter2' },
    })
    fireEvent.input(getByLabelText(/Matrix Recovery Key/), {
      target: { value: ' EsTc secret ' },
    })
    fireEvent.click(getByRole('button', { name: 'Log in' }))

    await waitFor(() =>
      expect(loginBody).toEqual({
        username: '@alice:example.org',
        password: 'hunter2',
        homeserver_url: null,
      }),
    )
    expect(recoverBody).toEqual({ recovery_key: 'EsTc secret' })
  })

  it('delete requires a confirmation step', async () => {
    let deleted = false
    const { findAllByRole, getByRole, queryByRole } = renderPage([ALICE])
    server.use(
      http.delete(`${TEST_BASE_URL}/v1/accounts/${ALICE.account_id}`, () => {
        deleted = true
        return new HttpResponse(null, { status: 204 })
      }),
    )

    const [deleteButton] = await findAllByRole('button', { name: 'Delete' })
    fireEvent.click(deleteButton)
    expect(deleted).toBe(false)

    // Cancel first: nothing happens.
    fireEvent.click(getByRole('button', { name: 'Cancel' }))
    expect(queryByRole('button', { name: 'Confirm delete' })).toBeNull()

    fireEvent.click(getByRole('button', { name: 'Delete' }))
    fireEvent.click(getByRole('button', { name: 'Confirm delete' }))
    await waitFor(() => expect(deleted).toBe(true))
  })

  it('logout posts to the account-scoped route', async () => {
    let loggedOut = false
    const { findByRole } = renderPage([ALICE])
    server.use(
      http.post(
        `${TEST_BASE_URL}/v1/accounts/${ALICE.account_id}/logout`,
        () => {
          loggedOut = true
          return HttpResponse.json({ data: { ...ALICE, state: 'deactivated' } })
        },
      ),
    )

    fireEvent.click(await findByRole('button', { name: 'Log out' }))
    await waitFor(() => expect(loggedOut).toBe(true))
  })

  it('recover reveals the key form and submits it', async () => {
    let recoverBody: unknown
    const { findByRole, getByLabelText, getByRole } = renderPage([ALICE])
    server.use(
      http.post(
        `${TEST_BASE_URL}/v1/accounts/${ALICE.account_id}/recover`,
        async ({ request }) => {
          recoverBody = await request.json()
          return HttpResponse.json({ data: ALICE })
        },
      ),
    )

    fireEvent.click(await findByRole('button', { name: 'Recover keys' }))
    fireEvent.input(getByLabelText('Recovery key'), {
      target: { value: 'EsTc aaaa bbbb' },
    })
    fireEvent.click(getByRole('button', { name: 'Recover' }))

    await waitFor(() =>
      expect(recoverBody).toEqual({ recovery_key: 'EsTc aaaa bbbb' }),
    )
  })

  it('the account switch persists to settings', async () => {
    const { services, findAllByLabelText } = renderPage([ALICE, BOB])
    const [radio] = await findAllByLabelText('use this account')

    fireEvent.click(radio)

    expect(services.settings.activeAccountId.value).toBe(ALICE.account_id)
  })

  it('surfaces and dismisses a load error', async () => {
    server.use(
      http.get(`${TEST_BASE_URL}/v1/accounts`, () =>
        HttpResponse.json(
          { error: { code: 'internal', message: 'database unavailable' } },
          { status: 500 },
        ),
      ),
      http.get(`${TEST_BASE_URL}/v1/status`, () =>
        HttpResponse.json({ data: STATUS }),
      ),
    )
    const services = testServices()
    const { findByRole, getByRole, queryByRole } = render(
      <ServicesContext.Provider value={services}>
        <AccountsPage />
      </ServicesContext.Provider>,
    )

    expect((await findByRole('alert')).textContent).toContain(
      'database unavailable',
    )
    fireEvent.click(getByRole('button', { name: 'Dismiss' }))
    expect(queryByRole('alert')).toBeNull()
  })
})
