import { expect, test, type Page } from '@playwright/test'
import { ACCOUNT_ID, ROOM_ID, ROOM_URL, signIn } from './helpers'

const message = (id: string, ts: number, body: string, reactions = null) => ({
  account_id: ACCOUNT_ID,
  event_id: id,
  room_id: ROOM_ID,
  sender: '@alice:hs',
  origin_ts: ts,
  type: 'm.room.message',
  body,
  content: { msgtype: 'm.text', body },
  redacted: false,
  edited: false,
  edit_count: 0,
  state_key: null,
  reactions,
  relates_to: null,
})

// Timeline API order is newest-first; the client reverses it for display.
const events = [
  message('$latest', 1_023, 'newest message'),
  ...Array.from({ length: 23 }, (_, index) =>
    message(
      `$m${22 - index}`,
      1_000 + 22 - index,
      `filler message ${22 - index}`,
    ),
  ),
]

async function openReactionFixture(page: Page) {
  await signIn(page)
  await page.setViewportSize({ width: 390, height: 720 })
  await page.route('**/timeline*', (route) =>
    route.fulfill({ json: { data: { events, next_cursor: null } } }),
  )
  await page.route(/\/events\/[^/]+$/, (route) =>
    route.fulfill({
      json: {
        data: message('$latest', 1_023, 'newest message', {
          '👍': { count: 1, me: true, senders: ['@me:hs'] },
        }),
      },
    }),
  )
  await page.route(/\/events\/[^/]+\/reactions$/, (route) =>
    route.fulfill({ json: { data: { event_id: '$rx' } } }),
  )
  await page.goto(ROOM_URL)
  await expect(page.getByText('newest message')).toBeVisible()
}

test('reacting to the newest message keeps the reaction chip inside the timeline viewport', async ({
  page,
}) => {
  await openReactionFixture(page)

  const composer = page.getByRole('textbox', { name: 'Message E2E Room' })
  await composer.fill('/react 👍')
  await composer.press('Enter')

  await expect(page.getByText('👍 1')).toBeVisible()
  await expect
    .poll(() =>
      page.evaluate(() => {
        const timeline = document.querySelector('.timeline')!
        const chip = document.querySelector('.reaction-chip')!
        const timelineBox = timeline.getBoundingClientRect()
        const chipBox = chip.getBoundingClientRect()
        return {
          bottomOverflow: chipBox.bottom - timelineBox.bottom,
          scrollGap:
            timeline.scrollHeight - timeline.scrollTop - timeline.clientHeight,
        }
      }),
    )
    .toMatchObject({
      bottomOverflow: expect.any(Number),
      scrollGap: 0,
    })
  const geometry = await page.evaluate(() => {
    const timeline = document.querySelector('.timeline')!
    const chip = document.querySelector('.reaction-chip')!
    return (
      chip.getBoundingClientRect().bottom -
      timeline.getBoundingClientRect().bottom
    )
  })
  expect(geometry).toBeLessThanOrEqual(0.5)
})
