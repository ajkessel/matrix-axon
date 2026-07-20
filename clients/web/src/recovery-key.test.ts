import { describe, expect, it } from 'vitest'
import { decodeBase58, isValidRecoveryKey } from './recovery-key'

// Generated with the SSSS scheme matrix-js-sdk uses: prefix 0x8B 0x01, a
// 32-byte key, and an XOR parity byte, Base58-encoded and space-grouped.
const VALID = 'EsT1 t3bE JPZs Bz9H xApv jfQh PY9X gmGM bhbN Kz2L 2t9n aeKB'
const VALID_2 = 'EsTX QTvB cJ6G LBYo EJ7X QcH1 M3xg bNoH 1VpY 3Jgu V1eM vaos'
// Structurally 35 bytes with a zero parity, but the wrong prefix.
const WRONG_PREFIX = '11uN FJp1 ZGEk 9634 5h6U zk2a w95s 1Gfg Wuzw SELD BXFt TZ'
// Correct prefix and length, but the parity byte does not check out.
const BAD_PARITY = 'EsT1 t3bE JPZs Bz9H xApv jfQh PY9X gmGM bhbN Kz2L 2t9n aeJp'

describe('decodeBase58', () => {
  it('rejects characters outside the Base58 alphabet', () => {
    // 0, O, I, and l are deliberately absent from the alphabet.
    expect(decodeBase58('abc0')).toBeNull()
    expect(decodeBase58('OIl')).toBeNull()
  })

  it('maps each leading "1" to a leading zero byte', () => {
    expect(Array.from(decodeBase58('11') ?? [])).toEqual([0, 0])
  })

  it('round-trips a small value', () => {
    // 'a' is index 33 in the alphabet.
    expect(Array.from(decodeBase58('a') ?? [])).toEqual([33])
  })
})

describe('isValidRecoveryKey', () => {
  it('accepts a well-formed key', () => {
    expect(isValidRecoveryKey(VALID)).toBe(true)
    expect(isValidRecoveryKey(VALID_2)).toBe(true)
  })

  it('ignores surrounding and interior whitespace', () => {
    expect(isValidRecoveryKey(`  ${VALID}  `)).toBe(true)
    expect(isValidRecoveryKey(VALID.replace(/ /g, ''))).toBe(true)
    expect(isValidRecoveryKey(VALID.replace(/ /g, '\n'))).toBe(true)
  })

  it('rejects an empty or whitespace-only string', () => {
    expect(isValidRecoveryKey('')).toBe(false)
    expect(isValidRecoveryKey('    ')).toBe(false)
  })

  it('rejects a key of the wrong length', () => {
    // The pre-validation placeholder that used to be accepted verbatim.
    expect(isValidRecoveryKey('EsTc aaaa bbbb')).toBe(false)
    // A truncated real key.
    expect(isValidRecoveryKey(VALID.slice(0, 20))).toBe(false)
  })

  it('rejects the wrong prefix even with a valid parity', () => {
    expect(isValidRecoveryKey(WRONG_PREFIX)).toBe(false)
  })

  it('rejects a bad parity byte', () => {
    expect(isValidRecoveryKey(BAD_PARITY)).toBe(false)
  })

  it('rejects non-Base58 characters', () => {
    expect(isValidRecoveryKey('EsT0 t3bE JPZs')).toBe(false)
  })
})
