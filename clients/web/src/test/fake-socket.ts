/**
 * A minimal, hand-driven `WebSocket` stand-in for tests. jsdom ships no
 * `WebSocket`, and the live layer never opens a real one under test: the
 * `LiveConnection` socket factory returns one of these, and the test drives
 * the handshake (`emitOpen`), inbound frames (`emitMessage`), and teardown
 * (`emitClose`) by hand. Only the surface `LiveConnection` touches is
 * implemented.
 */
export class FakeWebSocket {
  static readonly CONNECTING = 0
  static readonly OPEN = 1
  static readonly CLOSING = 2
  static readonly CLOSED = 3

  readyState = FakeWebSocket.CONNECTING
  onopen: (() => void) | null = null
  onmessage: ((event: { data: unknown }) => void) | null = null
  onclose: (() => void) | null = null
  onerror: (() => void) | null = null

  /** Frames the client sent (unused in M-W6 step 2; here for later steps). */
  readonly sent: unknown[] = []
  /** True once `close()` was called by the code under test. */
  closed = false
  readonly url?: string
  readonly protocols?: string | string[]

  constructor(url?: string, protocols?: string | string[]) {
    this.url = url
    this.protocols = protocols
  }

  send(data: unknown): void {
    this.sent.push(data)
  }

  close(): void {
    this.closed = true
    this.readyState = FakeWebSocket.CLOSED
  }

  /** Drive the open handshake. */
  emitOpen(): void {
    this.readyState = FakeWebSocket.OPEN
    this.onopen?.()
  }

  /** Deliver one inbound frame (a raw string, as the server sends). */
  emitMessage(data: unknown): void {
    this.onmessage?.({ data })
  }

  /** Drive an unexpected close (server or transport hangup). */
  emitClose(): void {
    this.readyState = FakeWebSocket.CLOSED
    this.onclose?.()
  }

  /** Drive a transport error. */
  emitError(): void {
    this.onerror?.()
  }

  /** Cast to the `WebSocket` a socket factory must return. */
  asWebSocket(): WebSocket {
    return this as unknown as WebSocket
  }
}
