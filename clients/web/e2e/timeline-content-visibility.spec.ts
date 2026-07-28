import { expect, test } from '@playwright/test'
import { ACCOUNT_ID, ROOM_ID, signIn } from './helpers'

const message = (id: string, ts: number, body: string, html?: string) => ({
  account_id: ACCOUNT_ID,
  event_id: id,
  room_id: ROOM_ID,
  sender: '@alice:hs',
  origin_ts: ts,
  type: 'm.room.message',
  body,
  content:
    html === undefined
      ? { msgtype: 'm.text', body }
      : {
          msgtype: 'm.text',
          body,
          format: 'org.matrix.custom.html',
          formatted_body: html,
        },
  redacted: false,
  edited: id.endsWith('7'),
  edit_count: id.endsWith('7') ? 1 : 0,
  state_key: null,
  reactions: id.endsWith('3')
    ? { '👍': { count: 2, me: false, senders: [] } }
    : null,
  relates_to: null,
})

const events = Array.from({ length: 90 }, (_, index) => {
  const n = 89 - index
  const id = `$m${n}`
  if (n % 9 === 0) {
    return message(
      id,
      1_000 + n,
      `formatted ${n}`,
      `<p><strong>formatted ${n}</strong></p><blockquote>${'quoted text '.repeat(
        18,
      )}</blockquote>`,
    )
  }
  if (n % 5 === 0) {
    return message(id, 1_000 + n, `multi-line ${n}\n${'long body '.repeat(35)}`)
  }
  return message(id, 1_000 + n, `short message ${n}`)
})

test('mixed-height timeline scrolling does not drift without content-visibility', async ({
  page,
}) => {
  await signIn(page)
  await page.setViewportSize({ width: 390, height: 844 })
  await page.route('**/timeline*', (route) =>
    route.fulfill({ json: { data: { events, next_cursor: null } } }),
  )
  await page.goto(`/${ACCOUNT_ID}/rooms/${encodeURIComponent(ROOM_ID)}`)
  await expect(page.getByText('short message 1', { exact: true })).toBeVisible()

  const result = await page.evaluate(async () => {
    const timeline = document.querySelector<HTMLElement>('.timeline')!
    const firstRow = document.querySelector<HTMLElement>('.event-row')!
    const style = getComputedStyle(firstRow)
    const samples: Array<{
      afterFirstPaint: number
      afterSecondPaint: number
      scrollHeight: number
    }> = []

    const waitForPaint = () =>
      new Promise<void>((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
      )

    for (const fraction of [0, 0.25, 0.5, 0.75, 1]) {
      const requested =
        (timeline.scrollHeight - timeline.clientHeight) * fraction
      timeline.scrollTop = requested
      await waitForPaint()
      const afterFirstPaint = timeline.scrollTop
      await waitForPaint()
      samples.push({
        afterFirstPaint,
        afterSecondPaint: timeline.scrollTop,
        scrollHeight: timeline.scrollHeight,
      })
    }

    return {
      contentVisibility: style.contentVisibility,
      samples,
    }
  })

  expect(result.contentVisibility).toBe('visible')
  for (const sample of result.samples) {
    expect(
      Math.abs(sample.afterSecondPaint - sample.afterFirstPaint),
    ).toBeLessThanOrEqual(1)
  }
})
