import { cleanup, fireEvent, render } from '@testing-library/preact'
import { HttpResponse, http } from 'msw'
import { setupServer } from 'msw/node'
import { afterAll, afterEach, beforeAll, describe, expect, it } from 'vitest'
import { ServicesContext } from '../services'
import { TEST_BASE_URL, testServices } from '../test/services'
import { SettingsPage } from './SettingsPage'

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
