import { cleanup, render, waitFor } from '@testing-library/preact'
import { HttpResponse, http } from 'msw'
import { setupServer } from 'msw/node'
import { afterAll, afterEach, beforeAll, describe, expect, it } from 'vitest'
import { ServicesContext } from '../services'
import { TEST_BASE_URL, testServices } from '../test/services'
import { FormattedBody } from './FormattedBody'
import type { RoomDto } from '../stores/room-list'

const ACCOUNT = '11111111-1111-4111-8111-111111111111'
const PNG = new Uint8Array([0x89, 0x50, 0x4e, 0x47])

const server = setupServer()
beforeAll(() => server.listen({ onUnhandledRequest: 'error' }))
afterEach(() => {
  cleanup()
  server.resetHandlers()
})
afterAll(() => server.close())

function room(overrides: Partial<RoomDto>): RoomDto {
  return {
    account_id: ACCOUNT,
    account_user_id: '@me:hs',
    room_id: '!room:hs',
    name: null,
    canonical_alias: null,
    topic: null,
    last_activity_ts: 0,
    last_event_id: null,
    membership: 'join',
    ...overrides,
  } as RoomDto
}

async function servicesWithRooms(rooms: RoomDto[]) {
  const services = testServices()
  server.use(
    http.get(`${TEST_BASE_URL}/v1/rooms`, () =>
      HttpResponse.json({ data: rooms }),
    ),
  )
  await services.rooms.refresh()
  return services
}

function renderBody(
  props: { body?: string | null; content: unknown },
  services = testServices(),
) {
  return render(
    <ServicesContext.Provider value={services}>
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

  it('leaves plain URL anchors available for the modern message-link style', () => {
    const { container } = renderBody({
      body: 'see https://example.org/x?q=1',
      content: null,
    })
    const anchor = container.querySelector('a')!
    expect(anchor.className).toBe('')
    expect(anchor.closest('.event-body')).toBeNull()
    expect(anchor.closest('.body-text')).toBeTruthy()
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

  it('flags formatted links whose visible URL points somewhere else', () => {
    const { container } = renderBody({
      body: 'fallback',
      content: {
        format: 'org.matrix.custom.html',
        formatted_body: '<a href="https://example.com">https://google.com</a>',
      },
    })
    const anchor = container.querySelector('a')!

    expect(anchor.className).toBe('dangerous-link')
    expect(anchor.getAttribute('title')).toContain('opens https://example.com')
    expect(anchor.getAttribute('aria-label')).toContain(
      'warning: opens https://example.com',
    )
  })

  it('turns known Matrix.to room URLs in plain text into local room pills', async () => {
    const services = await servicesWithRooms([
      room({
        room_id: '!ops:hs',
        name: 'Ops',
        canonical_alias: '#ops:hs',
      }),
    ])
    const { container } = renderBody(
      {
        body: 'visit https://matrix.to/#/%23ops%3Ahs',
        content: null,
      },
      services,
    )

    const anchor = container.querySelector('a')!
    expect(anchor.className).toBe('room-pill')
    expect(anchor.getAttribute('href')).toBe(`/${ACCOUNT}/rooms/!ops%3Ahs`)
    expect(anchor.getAttribute('target')).toBeNull()
    expect(anchor.getAttribute('title')).toBe('Jump to room')
    expect(anchor.textContent).toBe('Ops')
  })

  it('turns Matrix.to user URLs in plain text into mention pills', () => {
    const { container } = renderBody({
      body: 'ping https://matrix.to/#/%40alice%3Ahs',
      content: null,
    })

    const anchor = container.querySelector('a')!
    expect(anchor.className).toBe('mention-pill')
    expect(anchor.getAttribute('href')).toBe(
      'https://matrix.to/#/%40alice%3Ahs',
    )
    expect(anchor.getAttribute('target')).toBeNull()
    expect(anchor.textContent).toBe('@alice')
  })

  it('turns known Matrix.to room anchors in formatted bodies into local room pills', async () => {
    const services = await servicesWithRooms([
      room({
        room_id: '!ops:hs',
        name: 'Ops',
        canonical_alias: '#ops:hs',
      }),
    ])
    const { container } = renderBody(
      {
        body: 'fallback',
        content: {
          format: 'org.matrix.custom.html',
          formatted_body:
            '<a href="https://matrix.to/#/%23ops%3Ahs">#ops:hs</a>',
        },
      },
      services,
    )

    const anchor = container.querySelector('a')!
    expect(anchor.className).toBe('room-pill')
    expect(anchor.getAttribute('href')).toBe(`/${ACCOUNT}/rooms/!ops%3Ahs`)
    expect(anchor.getAttribute('target')).toBeNull()
    expect(anchor.getAttribute('title')).toBe('Jump to room')
    expect(anchor.textContent).toBe('#ops:hs')
  })

  it('turns Matrix.to room-event anchors into local event deep links', async () => {
    const services = await servicesWithRooms([
      room({
        room_id: '!ops:hs',
        name: 'Ops',
        canonical_alias: '#ops:hs',
      }),
    ])
    const { container } = renderBody(
      {
        body: 'fallback',
        content: {
          format: 'org.matrix.custom.html',
          formatted_body:
            '<a href="https://matrix.to/#/%23ops%3Ahs/%24event?via=hs">#ops:hs</a>',
        },
      },
      services,
    )

    const anchor = container.querySelector('a')!
    expect(anchor.className).toBe('room-pill event-pill')
    expect(anchor.getAttribute('href')).toBe(
      `/${ACCOUNT}/rooms/!ops%3Ahs?event=%24event`,
    )
    expect(anchor.getAttribute('target')).toBeNull()
    expect(anchor.getAttribute('title')).toBe('Jump to message')
  })

  it('turns Matrix.to user anchors in formatted bodies into mention pills', () => {
    const { container } = renderBody({
      body: 'fallback',
      content: {
        format: 'org.matrix.custom.html',
        formatted_body: '<a href="https://matrix.to/#/@alice:hs">@alice:hs</a>',
      },
    })

    const anchor = container.querySelector('a')!
    expect(anchor.className).toBe('mention-pill')
    expect(anchor.getAttribute('href')).toBe(
      'https://matrix.to/#/%40alice%3Ahs',
    )
    expect(anchor.getAttribute('target')).toBeNull()
    expect(anchor.textContent).toBe('@alice:hs')
  })

  it('leaves unknown Matrix.to room URLs external until join exists', () => {
    const { container } = renderBody({
      body: 'visit https://matrix.to/#/%23unknown%3Ahs',
      content: null,
    })

    const anchor = container.querySelector('a')!
    expect(anchor.className).toBe('')
    expect(anchor.getAttribute('href')).toBe(
      'https://matrix.to/#/%23unknown%3Ahs',
    )
    expect(anchor.getAttribute('target')).toBe('_blank')
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
