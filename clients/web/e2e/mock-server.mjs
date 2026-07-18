// A dependency-free mock axon backend for the M-W6 e2e lane (ADR 0061): it
// serves the built app and enough of `/v1` — plus a real `/v1/ws` — that two
// browser tabs can see each other's messages live. A POST to `.../send`
// broadcasts a `timeline.event` frame to every connected socket, which is
// exactly how the real server drives live delivery.
import { createServer } from 'node:http'
import { readFile } from 'node:fs/promises'
import { createHash, randomUUID } from 'node:crypto'
import { extname, join, normalize } from 'node:path'
import { fileURLToPath } from 'node:url'

const PORT = Number(process.env.PORT ?? 4599)
const DIST = fileURLToPath(new URL('../dist', import.meta.url))

const ACCOUNT_ID = '11111111-1111-4111-8111-111111111111'
const USER_ID = '@me:hs'
const ROOM_ID = '!room:hs'

/**
 * A second account, and rooms that stress the sidebar rather than the socket:
 * a long name to truncate, and an unnamed room whose title falls back to its
 * room id. With one account the row never shows a user id, and the room-name
 * truncation bug this fixture exists to catch could not reproduce at all.
 *
 * `E2E Room` keeps the newest activity, so recent-first sorting leaves it the
 * first row and the specs that reach for `a.room-link` still land on it.
 */
const ACCOUNT_ID_2 = '22222222-2222-4222-8222-222222222222'
const USER_ID_2 = '@other:hs'
const LONG_ROOM_ID = '!vQZGkFhTnLxWpRdCmYbA:hs'

function account(id, userId) {
  return {
    account_id: id,
    user_id: userId,
    homeserver_url: 'https://hs',
    device_id: 'DEVICE',
    state: 'active',
    verified: true,
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
  }
}

/** Events the reconcile GET (`/events/:id`) can look up after a send. */
const events = new Map()
/** Staged uploads, by `upload_id`, awaiting the `send-media` that claims them. */
const uploads = new Map()
/**
 * The room's history, oldest first, as the timeline GET returns it. A send
 * appends here as well as broadcasting, so a client that missed the frame (it
 * was disconnected) still finds the event when it refetches the head — which
 * is exactly what gap-fill on reconnect does.
 */
const timeline = []
/**
 * One state event at the head of history, so the "show state events" setting
 * has something to hide. It is not an `m.room.message`, so it is never a reply,
 * edit or `ArrowUp` target.
 */
timeline.push({
  account_id: ACCOUNT_ID,
  event_id: '$seed-member:hs',
  room_id: ROOM_ID,
  sender: USER_ID,
  state_key: USER_ID,
  origin_ts: Date.now() - 86_400_000,
  type: 'm.room.member',
  content: { membership: 'join' },
  body: null,
  relates_to: null,
  redacted: false,
  redaction_event_id: null,
  sender_trust: null,
  edited: false,
  edit_count: 0,
  latest_edit_ts: null,
  reactions: null,
})
/**
 * A 1×1 PNG the media proxy serves for `mxc://hs/e2eimg`, and an image event
 * that references it — so the media spec can prove the fetch → Bearer → blob →
 * object-URL → render path in a real browser. Sent by a *different* user so it
 * is never an edit or `ArrowUp` target in the messaging specs.
 */
const PNG_1PX = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==',
  'base64',
)
timeline.push({
  account_id: ACCOUNT_ID,
  event_id: '$seed-image:hs',
  room_id: ROOM_ID,
  sender: '@bob:hs',
  state_key: null,
  origin_ts: Date.now() - 3_600_000,
  type: 'm.room.message',
  content: {
    msgtype: 'm.image',
    body: 'sunrise.png',
    url: 'mxc://hs/e2eimg',
    info: { mimetype: 'image/png', w: 640, h: 480, size: PNG_1PX.length },
  },
  body: 'sunrise.png',
  relates_to: null,
  redacted: false,
  redaction_event_id: null,
  sender_trust: null,
  edited: false,
  edit_count: 0,
  latest_edit_ts: null,
  reactions: null,
})
/** Connected `/v1/ws` sockets. */
const sockets = new Set()
/**
 * While `Date.now()` is below this, `/v1/ws` upgrades are refused. Lets a test
 * hold a client in `reconnecting` long enough to miss a broadcast, so the
 * reconnect must gap-fill rather than race the frame.
 */
let blockUpgradesUntil = 0
/** Synthetic rooms `/v1/rooms` appends; set via `/__e2e/bulk-rooms`. */
let bulkRooms = 0
/** `/v1/search` answers 503 while set (the search-disabled server state);
 *  toggled via `/__e2e/search-503` and reset by the spec that sets it. */
let searchDisabled = false

const MIME = {
  '.html': 'text/html',
  '.js': 'text/javascript',
  '.css': 'text/css',
  '.svg': 'image/svg+xml',
  '.json': 'application/json',
  '.ico': 'image/x-icon',
  '.woff2': 'font/woff2',
  '.png': 'image/png',
}

const json = (res, body, status = 200) => {
  res.writeHead(status, { 'content-type': 'application/json' })
  res.end(JSON.stringify(body))
}

function makeEvent(body, eventId) {
  return {
    account_id: ACCOUNT_ID,
    event_id: eventId,
    room_id: ROOM_ID,
    sender: USER_ID,
    state_key: null,
    origin_ts: Date.now(),
    type: 'm.room.message',
    content: { msgtype: 'm.text', body },
    body,
    relates_to: null,
    redacted: false,
    redaction_event_id: null,
    sender_trust: null,
    edited: false,
    edit_count: 0,
    latest_edit_ts: null,
    reactions: null,
  }
}

/** Send one unmasked text frame to every connected socket. */
function broadcast(frame) {
  const payload = Buffer.from(JSON.stringify(frame))
  const len = payload.length
  let header
  if (len < 126) {
    header = Buffer.from([0x81, len])
  } else if (len < 65536) {
    header = Buffer.alloc(4)
    header[0] = 0x81
    header[1] = 126
    header.writeUInt16BE(len, 2)
  } else {
    header = Buffer.alloc(10)
    header[0] = 0x81
    header[1] = 127
    header.writeBigUInt64BE(BigInt(len), 2)
  }
  const buf = Buffer.concat([header, payload])
  for (const socket of sockets) {
    try {
      socket.write(buf)
    } catch {
      sockets.delete(socket)
    }
  }
}

async function serveStatic(res, pathname) {
  // SPA: anything without a file extension routes to index.html.
  const rel =
    pathname === '/' || extname(pathname) === '' ? '/index.html' : pathname
  const file = normalize(join(DIST, rel))
  if (!file.startsWith(DIST)) {
    res.writeHead(403).end()
    return
  }
  try {
    const data = await readFile(file)
    res.writeHead(200, {
      'content-type': MIME[extname(file)] ?? 'application/octet-stream',
    })
    res.end(data)
  } catch {
    res.writeHead(404).end('not found')
  }
}

async function handleApi(req, res, url) {
  const { pathname } = url
  const method = req.method

  if (method === 'POST' && pathname === '/v1/oauth/token') {
    let raw = ''
    req.on('data', (chunk) => (raw += chunk))
    req.on('end', () => {
      const form = new URLSearchParams(raw)
      if (form.get('grant_type') !== 'authorization_code') {
        return json(
          res,
          {
            error: 'unsupported_grant_type',
            error_description: 'mock server only supports authorization_code',
          },
          400,
        )
      }
      return json(res, {
        access_token: 'e2e-oauth-access',
        token_type: 'Bearer',
        expires_in: 3600,
        refresh_token: 'e2e-oauth-refresh',
      })
    })
    return
  }

  if (method === 'GET' && pathname === '/v1/accounts') {
    return json(res, {
      data: [account(ACCOUNT_ID, USER_ID), account(ACCOUNT_ID_2, USER_ID_2)],
    })
  }
  if (method === 'GET' && pathname === '/v1/status') {
    return json(res, {
      data: { backfill: { paused: false, free_bytes: 0, accounts: [] } },
    })
  }
  if (method === 'GET' && pathname === '/v1/rooms') {
    const now = Date.now()
    // Synthetic filler for the windowing lane: enough rooms that only a slice
    // of them can be in the DOM. Appended with older activity than the three
    // fixtures, so recent-first sorting leaves those at the top and every other
    // spec still finds `E2E Room` on the first row.
    const filler = Array.from({ length: bulkRooms }, (_, i) => ({
      account_id: ACCOUNT_ID,
      account_user_id: USER_ID,
      room_id: `!bulk${i}:hs`,
      name: `Bulk Room ${String(i).padStart(4, '0')}`,
      last_activity_ts: now - 30 * 86_400_000 - i * 1000,
    }))
    return json(res, {
      data: [
        {
          account_id: ACCOUNT_ID,
          account_user_id: USER_ID,
          room_id: ROOM_ID,
          name: 'E2E Room',
          last_activity_ts: now,
        },
        {
          account_id: ACCOUNT_ID,
          account_user_id: USER_ID,
          room_id: '!long:hs',
          name: 'A Room With A Name Far Too Long For The Sidebar',
          last_activity_ts: now - 3_600_000,
        },
        {
          // No name and no alias: the title falls back to this room id, which
          // is longer than any sidebar and must ellipsize rather than clip.
          account_id: ACCOUNT_ID_2,
          account_user_id: USER_ID_2,
          room_id: LONG_ROOM_ID,
          last_activity_ts: now - 8 * 86_400_000,
        },
        ...filler,
      ],
    })
  }
  // Unnamed rooms ask for members to derive a DM title; none here, so the
  // room-id fallback stands.
  if (method === 'GET' && /\/rooms\/[^/]+\/members$/.test(pathname)) {
    return json(res, { data: [] })
  }
  if (method === 'GET' && /\/rooms\/[^/]+\/threads$/.test(pathname)) {
    return json(res, { data: [] })
  }
  if (method === 'GET' && /\/rooms\/[^/]+\/timeline$/.test(pathname)) {
    // `at_ts` follows the real endpoint (the page at or before the timestamp,
    // newest-first, `limit`-sized) so jumps and forward paging are honest;
    // the un-jumped read keeps returning everything, as the other specs'
    // fixtures assume.
    const atTs = url.searchParams.get('at_ts')
    if (atTs !== null) {
      const limit = Number(url.searchParams.get('limit') ?? 50)
      const pool = [...timeline]
        .sort((a, b) => b.origin_ts - a.origin_ts)
        .filter((event) => event.origin_ts <= Number(atTs))
      return json(res, {
        data: { events: pool.slice(0, limit), next_cursor: null },
      })
    }
    return json(res, { data: { events: timeline, next_cursor: null } })
  }
  // The media proxy: raw image bytes behind the bearer guard, mirroring the
  // real route's headers so the client's fetch → blob path is exercised.
  if (method === 'GET' && /^\/v1\/media\/[^/]+\/[^/]+\/[^/]+$/.test(pathname)) {
    res.writeHead(200, {
      'content-type': 'image/png',
      etag: '"e2eimg"',
      'accept-ranges': 'bytes',
      'x-content-type-options': 'nosniff',
    })
    res.end(PNG_1PX)
    return
  }
  if (method === 'GET' && /\/devices\/[^/]+\/state\/[^/]+$/.test(pathname)) {
    const namespace = pathname.split('/').pop()
    return json(res, { data: { namespace, entries: {} } })
  }
  if (method === 'PUT' && /\/devices\/[^/]+\/state\/[^/]+$/.test(pathname)) {
    return json(res, { data: { updated_at: new Date().toISOString() } })
  }
  // Outbound read receipt (ADR 0067): accepted, nothing to echo back.
  if (method === 'POST' && /\/rooms\/[^/]+\/read$/.test(pathname)) {
    return json(res, { data: {} })
  }
  // Outbound typing notice (ADR 0068 M19a). Echo it to every tab as an
  // ephemeral passthrough attributed to a *peer* user, so another tab renders
  // the indicator — a tab filters its own user id out of its own view, so
  // echoing the sender's id would show nothing.
  if (method === 'PUT' && /\/rooms\/[^/]+\/typing$/.test(pathname)) {
    let raw = ''
    req.on('data', (chunk) => (raw += chunk))
    req.on('end', () => {
      const typing = JSON.parse(raw || '{}').typing === true
      broadcast({
        type: 'ephemeral.passthrough',
        account_id: ACCOUNT_ID,
        payload: {
          room_id: ROOM_ID,
          event_type: 'm.typing',
          content: { user_ids: typing ? [USER_ID_2] : [] },
        },
      })
      json(res, { data: {} })
    })
    return
  }
  if (method === 'GET' && /\/events\/[^/]+$/.test(pathname)) {
    const id = decodeURIComponent(pathname.split('/').pop())
    const event = events.get(id)
    return event
      ? json(res, { data: event })
      : json(res, { error: 'not_found' }, 404)
  }
  // The two-step media send (ADR 0059/0065): stage raw bytes, then claim the
  // staged id. Staging records what actually arrived — the query metadata, the
  // content type, and the byte count — so the spec can assert a real browser
  // put the file's bytes on the wire, which jsdom cannot prove.
  if (method === 'POST' && /\/media\/uploads$/.test(pathname)) {
    const chunks = []
    req.on('data', (chunk) => chunks.push(chunk))
    req.on('end', () => {
      const bytes = Buffer.concat(chunks)
      const uploadId = randomUUID()
      uploads.set(uploadId, {
        kind: url.searchParams.get('kind'),
        filename: url.searchParams.get('filename'),
        contentType: req.headers['content-type'] ?? null,
        bytes,
      })
      json(res, {
        data: {
          upload_id: uploadId,
          kind: url.searchParams.get('kind'),
          filename: url.searchParams.get('filename'),
          content_type: req.headers['content-type'] ?? null,
          size_bytes: bytes.length,
          expires_at: new Date(Date.now() + 3_600_000).toISOString(),
        },
      })
    })
    return
  }
  if (method === 'POST' && /\/rooms\/[^/]+\/send-media$/.test(pathname)) {
    let raw = ''
    req.on('data', (chunk) => (raw += chunk))
    req.on('end', () => {
      const request = JSON.parse(raw || '{}')
      const staged = uploads.get(request.upload_id)
      if (staged === undefined) {
        return json(
          res,
          { error: { code: 'not_found', message: 'unknown upload' } },
          404,
        )
      }
      // The real server uses the caption as the body, falling back to the
      // staged filename — the rule the client's local echo mirrors.
      const body = request.caption ?? staged.filename
      const eventId = `$${createHash('sha1')
        .update(body + Date.now())
        .digest('hex')
        .slice(0, 16)}:hs`
      const event = makeEvent(body, eventId)
      event.content = {
        msgtype: staged.kind === 'image' ? 'm.image' : 'm.file',
        body,
        filename: staged.filename,
        url: 'mxc://hs/e2eimg',
        info: {
          mimetype: staged.contentType,
          size: staged.bytes.length,
        },
      }
      events.set(eventId, event)
      // Deliberately *not* pushed onto `timeline`, unlike a text send. The
      // server outlives a single spec file (`reuseExistingServer`), so a sent
      // image appended to the seeded history would still be there for every
      // later spec — and `media.spec.ts`, which asserts on the seeded image,
      // would be reading this one. The live broadcast plus the `/events/:id`
      // lookup is all this lane needs; it never reloads.
      broadcast({
        type: 'timeline.event',
        account_id: ACCOUNT_ID,
        payload: event,
      })
      json(res, { data: { event_id: eventId } })
    })
    return
  }
  if (method === 'POST' && /\/rooms\/[^/]+\/send$/.test(pathname)) {
    let raw = ''
    req.on('data', (chunk) => (raw += chunk))
    req.on('end', () => {
      const body = (JSON.parse(raw || '{}').body ?? '').toString()
      const eventId = `$${createHash('sha1')
        .update(body + Date.now())
        .digest('hex')
        .slice(0, 16)}:hs`
      const event = makeEvent(body, eventId)
      events.set(eventId, event)
      timeline.push(event)
      // Drive live delivery to every tab, exactly like the real bus — which is
      // lossy: a tab that is disconnected right now simply misses this frame.
      broadcast({
        type: 'timeline.event',
        account_id: ACCOUNT_ID,
        payload: event,
      })
      json(res, { data: { event_id: eventId } })
    })
    return
  }
  // Search over everything in the room history (ADR 0039's semantics, small):
  // AND'd case-insensitive term match on `body`, the narrowing filters, and
  // an offset cursor — enough for the M-W10 overlay's paging and deep links.
  if (method === 'GET' && pathname === '/v1/search') {
    if (searchDisabled) {
      return json(
        res,
        { error: { code: 'unavailable', message: 'search is disabled' } },
        503,
      )
    }
    const q = url.searchParams.get('q') ?? ''
    const roomId = url.searchParams.get('room_id')
    const accountId = url.searchParams.get('account_id')
    const sender = url.searchParams.get('sender')?.toLowerCase()
    const from = Number(url.searchParams.get('from') ?? NaN)
    const to = Number(url.searchParams.get('to') ?? NaN)
    const limit = Number(url.searchParams.get('limit') ?? 50)
    const offset = Number(url.searchParams.get('cursor') ?? 0)
    const terms = q.toLowerCase().split(/\s+/).filter(Boolean)
    const matches = timeline.filter(
      (event) =>
        typeof event.body === 'string' &&
        terms.every((term) => event.body.toLowerCase().includes(term)) &&
        (roomId === null || event.room_id === roomId) &&
        (accountId === null || event.account_id === accountId) &&
        (sender === undefined || event.sender.toLowerCase().includes(sender)) &&
        (Number.isNaN(from) || event.origin_ts >= from) &&
        (Number.isNaN(to) || event.origin_ts <= to),
    )
    const page = matches.slice(offset, offset + limit)
    return json(res, {
      data: {
        results: page.map((event) => ({ event, score: 1 })),
        total: matches.length,
        next_cursor:
          offset + limit < matches.length ? String(offset + limit) : null,
      },
    })
  }
  return json(res, { error: 'unhandled' }, 404)
}

const server = createServer((req, res) => {
  const url = new URL(req.url, `http://${req.headers.host}`)
  // Test-only control surface, deliberately outside `/v1` so it can never be
  // mistaken for app API. Killing every socket without a close frame is the
  // rude disconnect a client must survive: the tab should show `Reconnecting…`
  // and heal by backoff + gap-fill.
  // How many synthetic rooms `/v1/rooms` pads its fixtures with. Zero by
  // default, so every other spec sees the three it always has.
  if (req.method === 'POST' && url.pathname === '/__e2e/bulk-rooms') {
    bulkRooms = Number(url.searchParams.get('count') ?? 0)
    return json(res, { data: { bulk_rooms: bulkRooms } })
  }
  // What the browser actually staged. The unit tests cannot assert the upload's
  // *bytes* — under jsdom a `File` is not undici's `Blob`, so the body never
  // survives the hop — so the real browser proves it here instead.
  if (req.method === 'GET' && url.pathname === '/__e2e/uploads') {
    return json(res, {
      data: [...uploads.values()].map((upload) => ({
        kind: upload.kind,
        filename: upload.filename,
        content_type: upload.contentType,
        size_bytes: upload.bytes.length,
        // A digest, not the bytes: media is binary, and any text encoding of it
        // would be lossy — an equal digest is exact byte identity.
        sha1: createHash('sha1').update(upload.bytes).digest('hex'),
      })),
    })
  }
  if (req.method === 'POST' && url.pathname === '/__e2e/search-503') {
    searchDisabled = url.searchParams.get('disabled') === 'true'
    return json(res, { data: { search_disabled: searchDisabled } })
  }
  if (req.method === 'POST' && url.pathname === '/__e2e/drop-sockets') {
    const blockMs = Number(url.searchParams.get('block_ms') ?? 0)
    blockUpgradesUntil = Date.now() + blockMs
    for (const socket of sockets) {
      socket.destroy()
    }
    sockets.clear()
    return json(res, { data: { dropped: true, block_ms: blockMs } })
  }
  if (url.pathname.startsWith('/v1/')) {
    void handleApi(req, res, url)
  } else {
    void serveStatic(res, url.pathname)
  }
})

// WebSocket upgrade: RFC 6455 handshake echoing the benign `axon` subprotocol
// (the #238 contract) and registering the socket for broadcast. We never read
// client frames — the app sends over REST — so no frame parsing is needed.
const WS_GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11'
server.on('upgrade', (req, socket) => {
  if (Date.now() < blockUpgradesUntil) {
    socket.destroy()
    return
  }
  const key = req.headers['sec-websocket-key']
  const accept = createHash('sha1')
    .update(key + WS_GUID)
    .digest('base64')
  const offered = (req.headers['sec-websocket-protocol'] ?? '')
    .split(',')
    .map((s) => s.trim())
  const lines = [
    'HTTP/1.1 101 Switching Protocols',
    'Upgrade: websocket',
    'Connection: Upgrade',
    `Sec-WebSocket-Accept: ${accept}`,
  ]
  if (offered.includes('axon')) {
    lines.push('Sec-WebSocket-Protocol: axon')
  }
  socket.write(lines.join('\r\n') + '\r\n\r\n')
  sockets.add(socket)
  socket.on('close', () => sockets.delete(socket))
  socket.on('error', () => sockets.delete(socket))
})

server.listen(PORT, '127.0.0.1', () => {
  console.log(`mock axon backend on http://127.0.0.1:${PORT}`)
})
