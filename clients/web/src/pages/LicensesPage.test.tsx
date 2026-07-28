import { cleanup, render } from '@testing-library/preact'
import { afterEach, describe, expect, it, vi } from 'vitest'

// The page reads the build-time disclosure via ../thirdparty; mock it so the
// test never triggers the pnpm-backed virtual module.
vi.mock('../thirdparty', () => ({
  THIRD_PARTY_LICENSES: [
    {
      name: 'preact',
      version: '10.29.4',
      license: 'MIT',
      text: 'The MIT License (MIT)\n\nCopyright (c) 2015-present Jason Miller',
      homepage: 'https://preactjs.com',
    },
    {
      name: 'marked',
      version: '18.0.5',
      license: 'MIT',
      text: 'MIT License text for marked',
    },
  ],
}))

import { LicensesPage } from './LicensesPage'

afterEach(cleanup)

describe('LicensesPage', () => {
  it('lists third-party packages with their license text', () => {
    const { getByText, getByRole } = render(<LicensesPage />)

    expect(getByRole('heading', { name: 'Open-source licenses' })).toBeTruthy()
    expect(getByText('2 third-party packages.')).toBeTruthy()
    expect(getByText('preact')).toBeTruthy()
    expect(getByText(/Copyright \(c\) 2015-present Jason Miller/)).toBeTruthy()
  })

  it('offers an in-app link back to settings', () => {
    const { getByText } = render(<LicensesPage />)

    const back = getByText('← Back to settings') as HTMLAnchorElement
    expect(back.getAttribute('href')).toBe('/settings')
  })
})
