import { HttpResponse, http } from 'msw'
import { setupServer } from 'msw/node'
import {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from 'vitest'
import { createApiClient } from '../api/client'
import { createMediaService } from '../media/media-service'
import { createTimelineStore, type EventDto } from './timeline'

/**
 * The media-send path (ADR 0065): stage the bytes, claim them with
 * `send-media`, and reconcile the optimistic echo — the same echo machinery a
 * text send uses, so these tests lean on it rather than re-proving it.
 */

const BASE_URL = 'http://axon.test'
const ACCOUNT = '6b53f7f0-0000-4000-8000-000000000001'
const ROOM = '!room:hs'
const UPLOAD_ID = '22222222-2222-4222-8222-222222222222'
const UPLOAD_PATH = `${BASE_URL}/v1/accounts/${ACCOUNT}/media/uploads`
const SEND_MEDIA_PATH = `${BASE_URL}/v1/accounts/${ACCOUNT}/rooms/:roomId/send-media`
const EVENTS_PATH = `${BASE_URL}/v1/accounts/${ACCOUNT}/events/:eventId`

const server = setupServer()
beforeAll(() => server.listen({ onUnhandledRequest: 'error' }))
afterEach(() => server.resetHandlers())
afterAll(() => server.close())

let revoked: string[]
beforeEach(() => {
  revoked = []
  vi.spyOn(URL, 'createObjectURL').mockImplementation(() => 'blob:preview')
  vi.spyOn(URL, 'revokeObjectURL').mockImplementation((url) => {
    revoked.push(url)
  })
})
afterEach(() => vi.restoreAllMocks())

function makeStore(threadRoot?: string) {
  const auth = {
    getToken: () => 'tok-test',
    onAuthFailure: () => {},
    LoginBootstrap: () => null,
  }
  const api = createApiClient(auth, BASE_URL)
  const media = createMediaService({ auth, baseUrl: BASE_URL })
  return createTimelineStore(api, media, ACCOUNT, ROOM, threadRoot)
}

function confirmed(id: string, body: string): EventDto {
  return {
    account_id: ACCOUNT,
    event_id: id,
    room_id: ROOM,
    sender: '@me:hs',
    origin_ts: 100,
    type: 'm.room.message',
    body,
    content: { msgtype: 'm.image', body, url: 'mxc://hs/abc' },
    redacted: false,
    edited: false,
    edit_count: 0,
  } as unknown as EventDto
}

/** The happy path: both endpoints succeed, the confirmed event is fetchable. */
function serveSuccess(): { sendBody: () => unknown } {
  let sendBody: unknown
  server.use(
    http.post(UPLOAD_PATH, () =>
      HttpResponse.json({ data: { upload_id: UPLOAD_ID } }),
    ),
    http.post(SEND_MEDIA_PATH, async ({ request }) => {
      sendBody = await request.json()
      return HttpResponse.json({ data: { event_id: '$img' } })
    }),
    http.get(EVENTS_PATH, () =>
      HttpResponse.json({ data: confirmed('$img', 'a caption') }),
    ),
  )
  return { sendBody: () => sendBody }
}

const png = () => new File(['bytes'], 'cat.png', { type: 'image/png' })

describe('sendMedia', () => {
  it('echoes the image locally, then claims the staged upload and reconciles', async () => {
    const seen = serveSuccess()
    const store = makeStore()

    const result = store.sendMedia(png(), {
      caption: 'a caption',
      senderId: '@me:hs',
    })

    // The echo renders synchronously — the whole point of not waiting out an
    // upload's transfer time in silence.
    expect(store.events.value).toHaveLength(1)
    const echo = store.events.value[0]
    expect(echo.localEcho?.status).toBe('pending')
    expect(echo.localEcho?.media?.previewUrl).toBe('blob:preview')
    expect(echo.localEcho?.media?.kind).toBe('image')
    expect(echo.body).toBe('a caption')

    expect(await result).toBe(true)

    expect(seen.sendBody()).toEqual({
      upload_id: UPLOAD_ID,
      caption: 'a caption',
      reply_to: null,
      thread_root: null,
    })
    // The echo is gone, replaced by the server's event — and its preview url
    // with it, since the confirmed event renders from the media proxy.
    expect(store.events.value).toHaveLength(1)
    expect(store.events.value[0].event_id).toBe('$img')
    expect(store.events.value[0].localEcho).toBeUndefined()
    expect(revoked).toEqual(['blob:preview'])
  })

  it('uses the filename as the echo body when there is no caption', async () => {
    serveSuccess()
    const store = makeStore()

    const result = store.sendMedia(png())

    // The server falls back to the filename for the event body, so the echo
    // must too — that identity is what lets the live frame confirm the echo.
    expect(store.events.value[0].body).toBe('cat.png')
    await result
  })

  it('carries reply and thread relations exactly as a text send does', async () => {
    const seen = serveSuccess()
    const store = makeStore('$root')

    await store.sendMedia(png(), { replyTo: '$parent' })

    expect(seen.sendBody()).toMatchObject({
      reply_to: '$parent',
      thread_root: '$root',
    })
  })

  it('stages a non-image as an m.file echo with no preview', async () => {
    serveSuccess()
    const store = makeStore()

    const result = store.sendMedia(
      new File(['%PDF'], 'notes.pdf', { type: 'application/pdf' }),
    )

    const echo = store.events.value[0]
    expect(echo.localEcho?.media?.kind).toBe('file')
    expect(echo.localEcho?.media?.previewUrl).toBeNull()
    expect((echo.content as unknown as { msgtype: string }).msgtype).toBe(
      'm.file',
    )
    await result
  })

  it('keeps the file on a failed upload, and retries with the same bytes', async () => {
    let uploads = 0
    server.use(
      http.post(UPLOAD_PATH, () => {
        uploads += 1
        return uploads === 1
          ? HttpResponse.json(
              { error: { code: 'bad_gateway', message: 'homeserver down' } },
              { status: 502 },
            )
          : HttpResponse.json({ data: { upload_id: UPLOAD_ID } })
      }),
      http.post(SEND_MEDIA_PATH, () =>
        HttpResponse.json({ data: { event_id: '$img' } }),
      ),
      http.get(EVENTS_PATH, () =>
        HttpResponse.json({ data: confirmed('$img', 'cat.png') }),
      ),
    )
    const store = makeStore()

    expect(await store.sendMedia(png())).toBe(false)

    // A failed upload must not cost the user the file they picked.
    const failed = store.events.value[0]
    expect(failed.localEcho?.status).toBe('failed')
    expect(failed.localEcho?.media?.file.name).toBe('cat.png')
    expect(store.error.value).toBe('Upload failed')

    expect(await store.retrySend(failed.event_id)).toBe(true)

    expect(uploads).toBe(2)
    expect(store.events.value).toHaveLength(1)
    expect(store.events.value[0].event_id).toBe('$img')
  })

  it('marks the echo failed when the send-media mutation is rejected', async () => {
    server.use(
      http.post(UPLOAD_PATH, () =>
        HttpResponse.json({ data: { upload_id: UPLOAD_ID } }),
      ),
      http.post(SEND_MEDIA_PATH, () =>
        HttpResponse.json(
          { error: { code: 'not_found', message: 'unknown upload' } },
          { status: 404 },
        ),
      ),
    )
    const store = makeStore()

    expect(await store.sendMedia(png())).toBe(false)

    expect(store.events.value[0].localEcho?.status).toBe('failed')
    expect(store.error.value).toBe('unknown upload')
    // Retained for the retry — the staged upload is re-made from the file.
    expect(store.events.value[0].localEcho?.media?.file).toBeInstanceOf(File)
  })

  it('revokes the preview url when a failed echo is discarded', async () => {
    server.use(
      http.post(UPLOAD_PATH, () =>
        HttpResponse.json(
          { error: { code: 'bad_request', message: 'nope' } },
          { status: 400 },
        ),
      ),
    )
    const store = makeStore()
    await store.sendMedia(png())

    store.discardSend(store.events.value[0].event_id)

    expect(store.events.value).toHaveLength(0)
    expect(revoked).toEqual(['blob:preview'])
  })

  it('drops the echo — and its preview — when a live frame confirms the send first', async () => {
    server.use(
      http.post(UPLOAD_PATH, () =>
        HttpResponse.json({ data: { upload_id: UPLOAD_ID } }),
      ),
      http.post(SEND_MEDIA_PATH, () => {
        // The bus beats our own reconcile: the frame lands while the mutation's
        // response is still in flight (WCR-07).
        store.ingestLive(confirmed('$img', 'cat.png'))
        return HttpResponse.json({ data: { event_id: '$img' } })
      }),
      http.get(EVENTS_PATH, () =>
        HttpResponse.json({ data: confirmed('$img', 'cat.png') }),
      ),
    )
    const store = makeStore()

    expect(await store.sendMedia(png(), { senderId: '@me:hs' })).toBe(true)

    // Exactly once in the slice, not twice.
    expect(store.events.value).toHaveLength(1)
    expect(store.events.value[0].event_id).toBe('$img')
    expect(revoked).toEqual(['blob:preview'])
  })
})
