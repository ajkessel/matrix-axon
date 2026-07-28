import type { LanguageFn } from 'highlight.js'

/**
 * The languages we can highlight, each behind its own dynamic `import()` so
 * Vite emits one chunk per grammar and a preview loads exactly the one it
 * needs (ADR 0073). Adding a language costs nothing until someone opens a file
 * in it.
 *
 * `highlight.js` ships ~190 grammars; this is the subset worth carrying for a
 * chat client's attachments and code blocks. An unlisted language renders as
 * plain text, which is the same thing it does today.
 */
const LOADERS: Record<string, () => Promise<{ default: LanguageFn }>> = {
  bash: () => import('highlight.js/lib/languages/bash'),
  c: () => import('highlight.js/lib/languages/c'),
  cpp: () => import('highlight.js/lib/languages/cpp'),
  csharp: () => import('highlight.js/lib/languages/csharp'),
  css: () => import('highlight.js/lib/languages/css'),
  diff: () => import('highlight.js/lib/languages/diff'),
  dockerfile: () => import('highlight.js/lib/languages/dockerfile'),
  go: () => import('highlight.js/lib/languages/go'),
  haskell: () => import('highlight.js/lib/languages/haskell'),
  ini: () => import('highlight.js/lib/languages/ini'),
  java: () => import('highlight.js/lib/languages/java'),
  javascript: () => import('highlight.js/lib/languages/javascript'),
  json: () => import('highlight.js/lib/languages/json'),
  kotlin: () => import('highlight.js/lib/languages/kotlin'),
  lua: () => import('highlight.js/lib/languages/lua'),
  makefile: () => import('highlight.js/lib/languages/makefile'),
  markdown: () => import('highlight.js/lib/languages/markdown'),
  nix: () => import('highlight.js/lib/languages/nix'),
  perl: () => import('highlight.js/lib/languages/perl'),
  php: () => import('highlight.js/lib/languages/php'),
  powershell: () => import('highlight.js/lib/languages/powershell'),
  python: () => import('highlight.js/lib/languages/python'),
  ruby: () => import('highlight.js/lib/languages/ruby'),
  rust: () => import('highlight.js/lib/languages/rust'),
  scala: () => import('highlight.js/lib/languages/scala'),
  sql: () => import('highlight.js/lib/languages/sql'),
  swift: () => import('highlight.js/lib/languages/swift'),
  typescript: () => import('highlight.js/lib/languages/typescript'),
  xml: () => import('highlight.js/lib/languages/xml'),
  yaml: () => import('highlight.js/lib/languages/yaml'),
}

/** Names a sender or a filename may use for a language we do load. */
const ALIASES: Record<string, string> = {
  'c++': 'cpp',
  'objective-c': 'c',
  bat: 'powershell',
  cc: 'cpp',
  cs: 'csharp',
  dockerfile: 'dockerfile',
  h: 'c',
  hpp: 'cpp',
  hs: 'haskell',
  htm: 'xml',
  html: 'xml',
  js: 'javascript',
  jsx: 'javascript',
  kt: 'kotlin',
  make: 'makefile',
  md: 'markdown',
  mjs: 'javascript',
  patch: 'diff',
  pl: 'perl',
  ps1: 'powershell',
  py: 'python',
  rb: 'ruby',
  rs: 'rust',
  sh: 'bash',
  shell: 'bash',
  svg: 'xml',
  tf: 'ini',
  toml: 'ini',
  ts: 'typescript',
  tsx: 'typescript',
  yml: 'yaml',
  zsh: 'bash',
}

/** Filenames that identify a language without any extension at all. */
const BY_FILENAME: Record<string, string> = {
  '.bash_profile': 'bash',
  '.bashrc': 'bash',
  '.gitconfig': 'ini',
  '.zshrc': 'bash',
  dockerfile: 'dockerfile',
  gemfile: 'ruby',
  makefile: 'makefile',
  rakefile: 'ruby',
}

/**
 * Resolve a language name (an extension, an alias, or a canonical id) to one
 * we can load, or `null`. Case-insensitive.
 */
export function canonicalLanguage(name: string): string | null {
  const key = name.trim().toLowerCase()
  if (key === '') {
    return null
  }
  const resolved = ALIASES[key] ?? key
  return resolved in LOADERS ? resolved : null
}

/** The language a filename implies, from its extension or its whole name. */
export function languageForFilename(filename: string): string | null {
  const base = filename.slice(filename.lastIndexOf('/') + 1).toLowerCase()
  const whole = BY_FILENAME[base]
  if (whole !== undefined) {
    return whole
  }
  const dot = base.lastIndexOf('.')
  if (dot <= 0 || dot === base.length - 1) {
    return null
  }
  return canonicalLanguage(base.slice(dot + 1))
}

/**
 * The language a shebang implies. Scripts are routinely uploaded with no
 * extension at all — `deploy`, `backup` — and the first line is then the only
 * thing that says what they are.
 */
export function languageFromShebang(source: string): string | null {
  const firstLine = source.slice(0, source.indexOf('\n') + 1 || undefined)
  if (!firstLine.startsWith('#!')) {
    return null
  }
  // `#!/usr/bin/env python3` → `python3`; `#!/bin/bash -e` → `bash`.
  const words = firstLine.slice(2).trim().split(/\s+/)
  const interpreter = (words[0]?.endsWith('env') === true ? words[1] : words[0])
    ?.split('/')
    .pop()
  if (interpreter === undefined) {
    return null
  }
  // Strip a trailing version (`python3.11`, `ruby2.7`, `perl5`).
  return canonicalLanguage(interpreter.replace(/[\d.]+$/, ''))
}

/** Load one grammar, or `null` when we do not carry it. */
export async function loadLanguage(
  language: string,
): Promise<LanguageFn | null> {
  const loader = LOADERS[language]
  if (loader === undefined) {
    return null
  }
  return (await loader()).default
}
