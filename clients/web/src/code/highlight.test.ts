import { describe, expect, it } from 'vitest'
import { HIGHLIGHT_MAX_CHARS, highlightCode } from './highlight'

describe('highlightCode', () => {
  it('marks up a language it carries', async () => {
    const html = await highlightCode('def f():\n    return 1\n', 'python')
    expect(html).not.toBeNull()
    expect(html).toContain('hljs-keyword')
    // The source text survives intact — this is decoration, not a rewrite.
    expect(html).toContain('return')
  })

  it('accepts an alias', async () => {
    expect(await highlightCode('echo hi\n', 'sh')).toContain('span')
  })

  it('declines a language it does not carry', async () => {
    expect(await highlightCode('x', 'brainfuck')).toBeNull()
  })

  it('declines a file over the size gate rather than stalling the thread', async () => {
    const huge = 'a = 1\n'.repeat(HIGHLIGHT_MAX_CHARS)
    expect(await highlightCode(huge, 'python')).toBeNull()
  })

  it('cannot turn source text into live markup', async () => {
    // highlight.js escapes its input and DOMPurify runs over the result. The
    // property that matters is not the absence of a substring — the payload
    // legitimately appears as *text* — but that parsing the output yields no
    // element beyond the decoration, and the text comes back byte-identical.
    const source = 'x = "<img src=x onerror=alert(1)>"\n'
    const html = await highlightCode(source, 'python')
    expect(html).not.toBeNull()

    const host = document.createElement('div')
    host.innerHTML = html!
    expect(host.querySelector('img')).toBeNull()
    expect(host.textContent).toBe(source)
    for (const el of host.querySelectorAll('*')) {
      expect(el.tagName).toBe('SPAN')
      expect(el.getAttributeNames()).toEqual(['class'])
    }
  })

  it('emits only spans with classes', async () => {
    const html = await highlightCode('const a = 1\n', 'javascript')
    const tags = [...(html ?? '').matchAll(/<(\w+)/g)].map((m) => m[1])
    expect(new Set(tags)).toEqual(new Set(['span']))
  })

  it('reuses the engine across calls', async () => {
    // Second call for the same language must not re-import or throw on a
    // duplicate registerLanguage.
    expect(await highlightCode('x = 1', 'python')).not.toBeNull()
    expect(await highlightCode('y = 2', 'python')).not.toBeNull()
  })
})
