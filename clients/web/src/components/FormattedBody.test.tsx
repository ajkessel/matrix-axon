import { cleanup, render, waitFor } from '@testing-library/preact'
import { HttpResponse, http } from 'msw'
import { setupServer } from 'msw/node'
import { afterAll, afterEach, beforeAll, describe, expect, it } from 'vitest'
import { ServicesContext } from '../services'
import { TEST_BASE_URL, testServices } from '../test/services'
import { FormattedBody } from './FormattedBody'

const ACCOUNT = '11111111-1111-4111-8111-111111111111'
const PNG = new Uint8Array([0x89, 0x50, 0x4e, 0x47])

const server = setupServer()
beforeAll(() => server.listen({ onUnhandledRequest: 'error' }))
afterEach(() => {
  cleanup()
  server.resetHandlers()
})
afterAll(() => server.close())

function renderBody(props: { body?: string | null; content: unknown }) {
  return render(
    <ServicesContext.Provider value={testServices()}>
      <FormattedBody
        accountId={ACCOUNT}
        body={props.body}
        content={props.content}
      />
    </ServicesContext.Provider>,
  )
}

describe('FormattedBody plain-text linkification', () => {
  it('turns bare URLs into safe anchors', () => {
    const { container } = renderBody({
      body: 'see https://example.org/x?q=1 for details',
      content: null,
    })
    const anchor = container.querySelector('a')!
    expect(anchor.getAttribute('href')).toBe('https://example.org/x?q=1')
    expect(anchor.getAttribute('target')).toBe('_blank')
    expect(anchor.getAttribute('rel')).toBe('noopener noreferrer')
    expect(container.textContent).toBe(
      'see https://example.org/x?q=1 for details',
    )
  })

  it('trims trailing sentence punctuation from the link', () => {
    const { container } = renderBody({
      body: 'read this: https://example.org/page.',
      content: null,
    })
    expect(container.querySelector('a')!.getAttribute('href')).toBe(
      'https://example.org/page',
    )
    expect(container.textContent).toBe('read this: https://example.org/page.')
  })

  it('linkifies multiple URLs and leaves other text alone', () => {
    const { container } = renderBody({
      body: 'a https://one.example and http://two.example b',
      content: null,
    })
    const hrefs = [...container.querySelectorAll('a')].map((a) =>
      a.getAttribute('href'),
    )
    expect(hrefs).toEqual(['https://one.example', 'http://two.example'])
  })

  it('does not linkify non-http schemes or plain text', () => {
    const { container } = renderBody({
      body: 'javascript:alert(1) and ftp://x and words',
      content: null,
    })
    expect(container.querySelector('a')).toBeNull()
  })

  it('linkifies bare URLs inside formatted bodies too (Element parity)', () => {
    const { container } = renderBody({
      body: 'fallback',
      content: {
        format: 'org.matrix.custom.html',
        formatted_body: 'try this: https://example.org<br>second line',
      },
    })
    expect(container.querySelector('a')!.getAttribute('href')).toBe(
      'https://example.org',
    )
  })
})

describe('FormattedBody inline media (ADR 0064)', () => {
  it('resolves an inline data-mxc image to a blob src after mount', async () => {
    server.use(
      http.get(
        `${TEST_BASE_URL}/v1/media/:account/:server/:media`,
        () =>
          new HttpResponse(PNG, { headers: { 'content-type': 'image/png' } }),
      ),
    )
    const { container } = renderBody({
      body: 'a picture',
      content: {
        format: 'org.matrix.custom.html',
        formatted_body: '<img src="mxc://hs/inline" alt="inline">',
      },
    })
    const img = container.querySelector('img')!
    // The sanitizer stashed the mxc in data-mxc and dropped src.
    expect(img.getAttribute('data-mxc')).toBe('mxc://hs/inline')
    await waitFor(() => expect(img.getAttribute('src')).toMatch(/^blob:/))
  })
})
