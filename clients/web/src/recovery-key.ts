/**
 * Matrix Secure Storage (4S) recovery-key format validation, done offline.
 *
 * A recovery key is the SSSS key encoded per the spec: the 2-byte prefix
 * `0x8B 0x01`, the 32-byte key, then a trailing parity byte (the XOR of the
 * preceding 34) — 35 bytes in all — Base58-encoded and displayed in
 * space-separated groups of four (e.g. `EsTc …`). This mirrors what
 * matrix-js-sdk's `decodeRecoveryKey` accepts.
 *
 * The check is of *shape*, not correctness: a structurally valid key still may
 * not be the right one for the account (wrong/rotated key, or no Secure
 * Backup). Only the server can say that — a well-formed key that fails there
 * comes back as a readable 400. This exists to catch typos, truncation, and
 * pasted-the-wrong-thing before a round trip, and to tell the user their input
 * is plausible.
 */

// Bitcoin Base58 alphabet — the ordering matters (no 0, O, I, l).
const BASE58_ALPHABET =
  '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
const PREFIX = [0x8b, 0x01] as const
const KEY_LENGTH = 32
/** prefix (2) + key (32) + parity (1). */
const DECODED_LENGTH = PREFIX.length + KEY_LENGTH + 1

/**
 * Decode a Base58 (Bitcoin alphabet) string to its bytes, big-endian, or
 * `null` if it contains any non-alphabet character. Each leading `1` maps to a
 * leading zero byte, per the encoding.
 */
export function decodeBase58(input: string): Uint8Array | null {
  const bytes: number[] = []
  for (const ch of input) {
    let carry = BASE58_ALPHABET.indexOf(ch)
    if (carry < 0) {
      return null
    }
    for (let i = 0; i < bytes.length; i++) {
      carry += bytes[i] * 58
      bytes[i] = carry & 0xff
      carry >>= 8
    }
    while (carry > 0) {
      bytes.push(carry & 0xff)
      carry >>= 8
    }
  }
  // `bytes` is little-endian so far; each leading '1' is a leading zero byte.
  for (let i = 0; i < input.length && input[i] === '1'; i++) {
    bytes.push(0)
  }
  return Uint8Array.from(bytes.reverse())
}

/**
 * Whether `input` is a structurally valid Matrix recovery key: Base58 that
 * decodes to 35 bytes carrying the `0x8B 0x01` prefix and a zero XOR parity.
 * Whitespace anywhere is ignored, since the display format is space-grouped.
 * Format only — not proof the key unlocks the account (see the file header).
 */
export function isValidRecoveryKey(input: string): boolean {
  const decoded = decodeBase58(input.replace(/\s/g, ''))
  if (decoded === null || decoded.length !== DECODED_LENGTH) {
    return false
  }
  if (decoded[0] !== PREFIX[0] || decoded[1] !== PREFIX[1]) {
    return false
  }
  let parity = 0
  for (const b of decoded) {
    parity ^= b
  }
  return parity === 0
}
