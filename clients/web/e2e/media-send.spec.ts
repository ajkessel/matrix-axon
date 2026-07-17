import { expect, test } from '@playwright/test'
import { createHash } from 'node:crypto'
import { openRoom } from './helpers'

/**
 * Sending media end to end (ADR 0065, M-W8.5). This lane exists to prove the one
 * thing the jsdom unit tests structurally cannot: that a real browser's `fetch`
 * puts the *file's bytes* on the wire when handed a `File` as the request body.
 * Under vitest a jsdom `File` is not undici's `Blob`, so the body never
 * survives the hop and the byte assertion is meaningless there.
 */

const PNG_BYTES = Buffer.from(
  '89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c489',
  'hex',
)

interface StagedUpload {
  kind: string
  filename: string
  content_type: string | null
  size_bytes: number
  sha1: string
}

async function staged(page: import('@playwright/test').Page) {
  const res = await page.request.get('/__e2e/uploads')
  return ((await res.json()) as { data: StagedUpload[] }).data
}

test('attaches a file, uploads its real bytes, and sends it with a caption', async ({
  page,
}) => {
  await openRoom(page)
  // The mock server outlives one spec run, so uploads accumulate: count from
  // where this test starts rather than from zero.
  const before = (await staged(page)).length

  await page.getByLabel('Attach a file').setInputFiles({
    name: 'cat.png',
    mimeType: 'image/png',
    buffer: PNG_BYTES,
  })

  // Staged in the composer, not yet uploaded — the pause is what lets a caption
  // be typed.
  await expect(page.locator('.composer-attachment')).toContainText('cat.png')
  expect(await staged(page)).toHaveLength(before)

  await page.locator('.composer textarea').fill('look at this')
  await page.getByRole('button', { name: 'Send' }).click()

  await expect
    .poll(async () => (await staged(page)).length)
    .toBeGreaterThan(before)
  const upload = (await staged(page)).at(-1)!

  // The bytes made it, intact — the assertion this whole lane exists for.
  expect(upload.size_bytes).toBe(PNG_BYTES.length)
  expect(upload.sha1).toBe(createHash('sha1').update(PNG_BYTES).digest('hex'))
  // `kind` and `Content-Type` agree, which is what the server's `kind=image`
  // rule requires; both come from the one `File.type`.
  expect(upload.kind).toBe('image')
  expect(upload.content_type).toBe('image/png')
  expect(upload.filename).toBe('cat.png')

  // The sent event lands in the timeline, rendered as an image from the proxy.
  await expect(page.locator('.composer-attachment')).toHaveCount(0)
  const sent = page.locator('.media-figure').last()
  await expect(sent.locator('figcaption')).toHaveText('look at this')
  await expect(sent.locator('img')).toHaveAttribute('src', /^blob:/)
})

test('shows the picked image immediately, before the upload finishes', async ({
  page,
}) => {
  await openRoom(page)

  // Hold the upload open so the optimistic echo is observable.
  let release = () => {}
  const held = new Promise<void>((resolve) => {
    release = resolve
  })
  await page.route('**/media/uploads**', async (route) => {
    await held
    await route.continue()
  })

  await page.getByLabel('Attach a file').setInputFiles({
    name: 'cat.png',
    mimeType: 'image/png',
    buffer: PNG_BYTES,
  })
  await page.getByRole('button', { name: 'Send' }).click()

  // The echo renders the local file while the bytes are still in flight — no
  // proxy fetch has resolved, and none could have.
  const echo = page.locator('.media-preview')
  await expect(echo).toHaveAttribute('src', /^blob:/)
  await expect(page.getByText('Sending…')).toBeVisible()

  release()
  await expect(page.getByText('Sending…')).toHaveCount(0)
})
