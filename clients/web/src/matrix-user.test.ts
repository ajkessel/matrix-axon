import { describe, expect, it } from 'vitest'
import { normalizeUserId } from './matrix-user'

describe('normalizeUserId', () => {
  it('accepts full Matrix user IDs and repairs a missing sigil', () => {
    expect(normalizeUserId('@alice:example.org', null)).toBe(
      '@alice:example.org',
    )
    expect(normalizeUserId('alice:example.org', null)).toBe(
      '@alice:example.org',
    )
  })

  it('accepts server names with explicit ports', () => {
    expect(normalizeUserId('@alice:example.org:8448', null)).toBe(
      '@alice:example.org:8448',
    )
    expect(normalizeUserId('alice:example.org:8448', null)).toBe(
      '@alice:example.org:8448',
    )
  })

  it('rejects malformed server-name ports', () => {
    expect(normalizeUserId('alice:example.org:notaport', null)).toBeNull()
    expect(normalizeUserId('alice:example.org:8448:extra', null)).toBeNull()
  })

  it('lowercases user localparts while preserving server-name case', () => {
    expect(normalizeUserId('@Alice:Example.ORG', null)).toBe(
      '@alice:Example.ORG',
    )
    expect(normalizeUserId('Alice:Example.ORG', null)).toBe(
      '@alice:Example.ORG',
    )
  })

  it('uses the default homeserver for localparts with or without @', () => {
    expect(normalizeUserId('alice', 'example.org')).toBe('@alice:example.org')
    expect(normalizeUserId('@alice', 'example.org')).toBe('@alice:example.org')
    expect(normalizeUserId('Alice', 'Example.ORG')).toBe('@alice:Example.ORG')
    expect(normalizeUserId('@Alice', 'Example.ORG')).toBe('@alice:Example.ORG')
  })
})
