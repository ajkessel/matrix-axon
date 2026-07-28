/**
 * `/v1/ws` URL + auth-subprotocol helpers (ADR 0046, M-W2; #238, M-W6).
 *
 * A browser `WebSocket` cannot set an `Authorization` header, so the token
 * rides in `Sec-WebSocket-Protocol` as a `bearer.<token>` entry (ADR 0029);
 * the server reads it there at upgrade time.
 *
 * The client also offers a benign, credential-free `axon` subprotocol *first*.
 * RFC 6455 §4.1 requires a client that offered subprotocols to fail the
 * connection when the server echoes none, and browsers enforce it (Chrome
 * hard, Firefox lenient) — so the server echoes `axon` in the 101 to keep the
 * handshake compliant, while never echoing the token-bearing entry (that would
 * leak the secret into response headers proxies and access logs may capture).
 * The server side landed in commit `3ee4c541`; this is its required client
 * half (`axon-api/src/ws.rs`, ADR 0029 amendment, #238) — mirrors the
 * Kubernetes API server's fix for the same problem.
 */

/**
 * The WebSocket URL for the API at `baseUrl` — same-origin by default (the
 * dev-proxy setup), or a cross-origin server root. `http(s)` maps to
 * `ws(s)`; an explicit `ws(s)` base passes through.
 *
 * `baseUrl` may be **relative**: `apiBaseUrl()` returns `'/'` for the
 * same-origin default, and that is the value the live socket is opened with.
 * It is resolved against the page origin first, because `new URL(path, base)`
 * throws on a relative base — a parameter default only covers `undefined`, so
 * an explicit `'/'` slipped past it and threw before any socket was
 * constructed, leaving the client permanently "reconnecting" (regression from
 * the `apiBaseUrl()` extraction in #349).
 */
export function wsUrl(baseUrl: string | URL = window.location.origin): string {
  const url = new URL('/v1/ws', new URL(baseUrl, window.location.origin))
  if (url.protocol === 'https:' || url.protocol === 'wss:') {
    url.protocol = 'wss:'
  } else {
    url.protocol = 'ws:'
  }
  return url.toString()
}

/**
 * The `Sec-WebSocket-Protocol` entries the browser offers: the benign `axon`
 * subprotocol the server echoes to keep the 101 RFC 6455-compliant (#238),
 * followed by the `bearer.<token>` entry carrying the credential (ADR 0029).
 * `axon` is listed first so the server has a non-secret protocol to negotiate;
 * the token-bearing entry is accepted but never echoed back.
 */
export function wsAuthProtocols(token: string): string[] {
  return ['axon', `bearer.${token}`]
}

/** Open the live-event socket, authenticated via subprotocol. */
export function openLiveSocket(
  token: string,
  baseUrl?: string | URL,
): WebSocket {
  return new WebSocket(wsUrl(baseUrl), wsAuthProtocols(token))
}
