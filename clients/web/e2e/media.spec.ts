import { expect, test } from '@playwright/test'
import { openRoom } from './helpers'

/**
 * Media rendering end to end (ADR 0064, M-W8). The seed timeline carries one
 * `m.image` pointing at `mxc://hs/e2eimg`, which the mock media route serves as
 * a real PNG behind the bearer guard. This proves in a real browser what jsdom
 * cannot: fetch with an `Authorization` header → `Blob` → `createObjectURL` →
 * an `<img>` the browser actually decodes, plus the lightbox interaction.
 */
test('an inline image loads through the media proxy as a blob url', async ({
  page,
}) => {
  await openRoom(page)

  const img = page.locator('.media-image img')
  await expect(img).toHaveAttribute('src', /^blob:/)
  // The browser decoded real bytes, so the image has intrinsic dimensions.
  await expect
    .poll(() => img.evaluate((el: HTMLImageElement) => el.naturalWidth))
    .toBeGreaterThan(0)
})

test('clicking an inline image opens a lightbox that closes on Escape', async ({
  page,
}) => {
  await openRoom(page)

  await page.locator('.media-image img').click()
  const dialog = page.getByRole('dialog')
  await expect(dialog).toBeVisible()
  await expect(dialog.locator('img')).toHaveAttribute('src', /^blob:/)

  await page.keyboard.press('Escape')
  await expect(dialog).toBeHidden()
})
