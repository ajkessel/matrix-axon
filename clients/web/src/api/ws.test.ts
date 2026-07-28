import { describe, expect, it } from 'vitest'
import { apiBaseUrl } from '../services'
import { wsAuthProtocols, wsUrl } from './ws'

describe('wsUrl', () => {
  it('maps http to ws and https to wss', () => {
    expect(wsUrl('http://localhost:8080')).toBe('ws://localhost:8080/v1/ws')
    expect(wsUrl('https://axon.example.com')).toBe(
      'wss://axon.example.com/v1/ws',
    )
  })

  it('passes explicit ws(s) bases through', () => {
    expect(wsUrl('ws://localhost:8080')).toBe('ws://localhost:8080/v1/ws')
    expect(wsUrl('wss://axon.example.com')).toBe('wss://axon.example.com/v1/ws')
  })

  it('replaces any base path with /v1/ws', () => {
    expect(wsUrl('https://axon.example.com/some/page')).toBe(
      'wss://axon.example.com/v1/ws',
    )
  })

  it('defaults to the page origin (the dev-proxy setup)', () => {
    // jsdom serves the tests from http://localhost:3000 by default.
    expect(wsUrl()).toBe(
      new URL('/v1/ws', window.location.origin)
        .toString()
        .replace(/^http/, 'ws'),
    )
  })

  const sameOrigin = new URL('/v1/ws', window.location.origin)
    .toString()
    .replace(/^http/, 'ws')

  it('resolves a relative base against the page origin', () => {
    // The regression: `apiBaseUrl()` returns '/' for the same-origin default,
    // and `new URL(path, '/')` throws — a parameter default only covers
    // `undefined`, so the explicit '/' slipped straight past it.
    expect(wsUrl('/')).toBe(sameOrigin)
  })

  it('matches the value the live socket is actually opened with', () => {
    // Guards the real call site, `openLiveSocket(token, apiBaseUrl())`. The
    // suite previously only exercised `wsUrl()` with no argument, which is not
    // how anything calls it — that is how the throw shipped.
    expect(wsUrl(apiBaseUrl())).toBe(sameOrigin)
  })

  it('never throws for any base the app can supply', () => {
    // A throw here is invisible in the UI: `socketFactory` throwing sends
    // `openSocket` straight to `scheduleReconnect`, which throws again on
    // every timer, so the client reconnects forever without ever constructing
    // a WebSocket.
    for (const base of ['/', '', undefined, window.location.origin]) {
      expect(() => wsUrl(base)).not.toThrow()
    }
  })
})

describe('wsAuthProtocols', () => {
  it('offers benign axon first, then the bearer.<token> entry (ADR 0029, #238)', () => {
    expect(wsAuthProtocols('tok-123')).toEqual(['axon', 'bearer.tok-123'])
  })

  it('lists axon before the credential so the server echoes a non-secret protocol', () => {
    const protocols = wsAuthProtocols('tok-123')
    expect(protocols[0]).toBe('axon')
    expect(protocols[1]).toContain('tok-123')
  })
})
