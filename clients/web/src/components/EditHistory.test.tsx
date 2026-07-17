import { cleanup, fireEvent, render, waitFor } from '@testing-library/preact'
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
import { ServicesContext } from '../services'
import { TEST_BASE_URL, testServices } from '../test/services'
import { EditHistory } from './EditHistory'

const ACCOUNT = '6b53f7f0-0000-4000-8000-000000000001'

const server = setupServer(
  http.get(
    `${TEST_BASE_URL}/v1/accounts/:accountId/events/:eventId/edits`,
    () => HttpResponse.json({ data: [] }),
  ),
)
beforeAll(() => server.listen({ onUnhandledRequest: 'error' }))
afterEach(() => {
  cleanup()
  server.resetHandlers()
})
afterAll(() => server.close())

function renderHistory() {
  const onClose = vi.fn()
  const utils = render(
    <ServicesContext.Provider value={testServices()}>
      <EditHistory accountId={ACCOUNT} eventId="$1" onClose={onClose} />
    </ServicesContext.Provider>,
  )
  return { onClose, ...utils }
}

describe('EditHistory', () => {
  it('is a labelled dialog', async () => {
    const { findByRole } = renderHistory()
    expect(await findByRole('dialog', { name: 'Edit history' })).toBeTruthy()
  })

  it('closes on Escape (ADR 0063) and marks the event handled', async () => {
    const { onClose, findByRole } = renderHistory()
    await findByRole('dialog', { name: 'Edit history' })

    const event = new KeyboardEvent('keydown', {
      key: 'Escape',
      bubbles: true,
      cancelable: true,
    })
    document.body.dispatchEvent(event)

    await waitFor(() => expect(onClose).toHaveBeenCalledOnce())
    // Claiming the event in capture is what stops RoomPage's staged Escape
    // from also closing the thread panel behind this modal.
    expect(event.defaultPrevented).toBe(true)
  })

  it('closes on the Close button', async () => {
    const { onClose, findByRole } = renderHistory()
    fireEvent.click(await findByRole('button', { name: 'Close' }))
    expect(onClose).toHaveBeenCalledOnce()
  })

  it('takes focus on open, traps Tab, and restores focus on close (WCR-14)', async () => {
    // Something outside the modal holds focus before it opens.
    const outside = document.createElement('button')
    outside.textContent = 'outside'
    document.body.appendChild(outside)
    outside.focus()
    expect(document.activeElement).toBe(outside)

    const { findByRole, unmount } = renderHistory()
    const close = await findByRole('button', { name: 'Close' })
    await waitFor(() => expect(document.activeElement).toBe(close))

    // With one focusable, Tab wraps onto itself — focus cannot leave.
    fireEvent.keyDown(close, { key: 'Tab' })
    expect(document.activeElement).toBe(close)
    fireEvent.keyDown(close, { key: 'Tab', shiftKey: true })
    expect(document.activeElement).toBe(close)

    unmount()
    expect(document.activeElement).toBe(outside)
    outside.remove()
  })
})
