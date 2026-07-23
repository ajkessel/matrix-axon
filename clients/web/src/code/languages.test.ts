import { describe, expect, it } from 'vitest'
import {
  canonicalLanguage,
  languageForFilename,
  languageFromShebang,
} from './languages'

describe('canonicalLanguage', () => {
  it('resolves aliases and canonical ids alike', () => {
    expect(canonicalLanguage('py')).toBe('python')
    expect(canonicalLanguage('python')).toBe('python')
    expect(canonicalLanguage('sh')).toBe('bash')
    expect(canonicalLanguage('YML')).toBe('yaml')
    expect(canonicalLanguage('  ts  ')).toBe('typescript')
  })

  it('returns null for a language we do not carry', () => {
    expect(canonicalLanguage('brainfuck')).toBeNull()
    expect(canonicalLanguage('')).toBeNull()
  })
})

describe('languageForFilename', () => {
  it('reads the extension', () => {
    expect(languageForFilename('deploy.py')).toBe('python')
    expect(languageForFilename('run.SH')).toBe('bash')
    expect(languageForFilename('/tmp/a/b/config.yaml')).toBe('yaml')
  })

  it('recognises names that carry no extension', () => {
    expect(languageForFilename('Makefile')).toBe('makefile')
    expect(languageForFilename('Dockerfile')).toBe('dockerfile')
    expect(languageForFilename('.bashrc')).toBe('bash')
  })

  it('returns null when the name says nothing', () => {
    expect(languageForFilename('notes')).toBeNull()
    expect(languageForFilename('archive.tar')).toBeNull()
    expect(languageForFilename('trailing.')).toBeNull()
  })
})

describe('languageFromShebang', () => {
  it('reads a direct interpreter path', () => {
    expect(languageFromShebang('#!/bin/bash\necho hi\n')).toBe('bash')
    expect(languageFromShebang('#!/bin/sh\n')).toBe('bash')
  })

  it('reads an env-style shebang', () => {
    expect(languageFromShebang('#!/usr/bin/env python3\nprint(1)\n')).toBe(
      'python',
    )
    expect(languageFromShebang('#!/usr/bin/env ruby\n')).toBe('ruby')
  })

  it('ignores interpreter flags and version suffixes', () => {
    expect(languageFromShebang('#!/bin/bash -eu\n')).toBe('bash')
    expect(languageFromShebang('#!/usr/bin/perl5\n')).toBe('perl')
  })

  it('returns null without a shebang', () => {
    expect(languageFromShebang('print(1)\n')).toBeNull()
    expect(languageFromShebang('')).toBeNull()
    expect(languageFromShebang('#!/usr/bin/env whatever\n')).toBeNull()
  })
})
