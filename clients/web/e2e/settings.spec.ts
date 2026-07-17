import { expect, test } from '@playwright/test'
import { openRoom, ROOM_URL, signIn } from './helpers'

/**
 * The state-events toggle moved from the room header into Settings, which makes
 * it a *persisted preference* rather than per-room view state. Persistence
 * through real `localStorage` and a reload is the part unit tests (which inject
 * an in-memory store) cannot prove.
 */
test.describe.configure({ mode: 'serial' })

test('the room header no longer carries a state-events checkbox', async ({
  page,
}) => {
  await openRoom(page)

  await expect(page.getByLabel('State events')).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Jump' })).toBeVisible()
})

test('the setting hides and shows state events, and survives a reload', async ({
  page,
}) => {
  await signIn(page)
  await page.setViewportSize({ width: 1400, height: 900 })
  await page.goto(ROOM_URL)
  await expect(page.getByRole('status')).toHaveText('Live')

  // Off by default: the seeded m.room.member event is filtered out.
  await expect(page.locator('.event-row.state-event')).toHaveCount(0)

  await page.goto('/settings')
  const box = page.getByLabel('Show state events')
  await expect(box).not.toBeChecked()
  await box.check()

  await page.goto(ROOM_URL)
  await expect(page.locator('.event-row.state-event')).toHaveCount(1)

  // The preference outlives the tab, unlike the checkbox it replaced.
  await page.reload()
  await expect(page.getByRole('status')).toHaveText('Live')
  await expect(page.locator('.event-row.state-event')).toHaveCount(1)

  await page.goto('/settings')
  await expect(page.getByLabel('Show state events')).toBeChecked()
  await page.getByLabel('Show state events').uncheck()

  await page.goto(ROOM_URL)
  await expect(page.locator('.event-row.state-event')).toHaveCount(0)
})

test('the checkbox renders as a checkbox, not a full-width text field', async ({
  page,
}) => {
  await signIn(page)
  await page.setViewportSize({ width: 1100, height: 700 })
  await page.goto('/settings')

  // The global `input` rule sizes text fields (width: 100%, padded, bordered)
  // and only radios were exempt until this setting arrived.
  const box = await page.getByLabel('Show state events').evaluate((el) => ({
    width: el.clientWidth,
    rowHeight: el.closest('.setting-row')!.clientHeight,
  }))
  expect(box.width).toBeLessThan(32)
  expect(box.rowHeight).toBeLessThan(40) // one line, not a wrapped label
})
