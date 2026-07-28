import { expect, test } from '@playwright/test'

test('OAuth callback exchanges the code and enters the signed-in shell', async ({
  page,
}) => {
  await page.addInitScript(() => {
    sessionStorage.setItem(
      'axon.oauth.pending',
      JSON.stringify({
        state: 'state-123',
        codeVerifier: 'verifier-123',
        provider: 'google',
        redirectUri: `${window.location.origin}/oauth/callback`,
        createdAt: Date.now(),
      }),
    )
  })
  await page.goto('/oauth/callback?code=code-1&state=state-123')

  await expect(page.getByRole('navigation', { name: 'Rooms' })).toBeVisible()
  await expect(page.getByRole('link', { name: 'Settings' })).toBeVisible()
  const session = await page.evaluate(() =>
    localStorage.getItem('axon.oauth.session'),
  )
  expect(session).toContain('e2e-oauth-access')
})
