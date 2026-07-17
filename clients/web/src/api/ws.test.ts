import { describe, expect, it } from 'vitest'
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
