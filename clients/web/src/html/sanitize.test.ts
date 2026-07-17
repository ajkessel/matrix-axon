import { describe, expect, it } from 'vitest'
import { renderMatrixHtml } from './sanitize'

describe('renderMatrixHtml', () => {
  it('keeps the Matrix HTML subset', () => {
    const html =
      '<h1>t</h1><blockquote><p><strong>b</strong> <em>i</em> <del>s</del></p></blockquote>' +
      '<ul><li>x</li></ul><ol start="3"><li>y</li></ol>' +
      '<table><thead><tr><th>h</th></tr></thead><tbody><tr><td>c</td></tr></tbody></table>' +
      '<pre><code class="language-rust">fn main() {}</code></pre><hr><br>'
    const out = renderMatrixHtml(html)
    for (const fragment of [
      '<h1>t</h1>',
      '<strong>b</strong>',
      '<ol start="3">',
      '<code class="language-rust">',
      '<td>c</td>',
    ]) {
      expect(out).toContain(fragment)
    }
  })

  it('removes script/style and event handlers', () => {
    expect(renderMatrixHtml('<script>alert(1)</script>hi')).toBe('hi')
    expect(renderMatrixHtml('<style>*{display:none}</style>hi')).toBe('hi')
    const out = renderMatrixHtml(
      '<p onclick="alert(1)" onmouseover="x()">t</p>',
    )
    expect(out).toBe('<p>t</p>')
  })

  it('blocks javascript: URLs but keeps https and mailto links', () => {
    expect(
      renderMatrixHtml('<a href="javascript:alert(1)">x</a>'),
    ).not.toContain('javascript:')
    const out = renderMatrixHtml('<a href="https://example.org">x</a>')
    expect(out).toContain('href="https://example.org"')
    expect(out).toContain('target="_blank"')
    expect(out).toContain('rel="noopener noreferrer"')
    expect(renderMatrixHtml('<a href="mailto:a@b.c">m</a>')).toContain(
      'href="mailto:a@b.c"',
    )
  })

  it('keeps generated room-pill links in-app', () => {
    const out = renderMatrixHtml(
      '<a class="room-pill" href="/acct/rooms/!ops%3Ahs">#Ops</a>',
    )

    expect(out).toContain('class="room-pill"')
    expect(out).toContain('href="/acct/rooms/!ops%3Ahs"')
    expect(out).not.toContain('target="_blank"')
  })

  it('keeps an mxc img, moving its src to data-mxc and dropping src (M-W8)', () => {
    const out = renderMatrixHtml(
      'before <img src="mxc://hs/x" alt="a diagram"> after',
    )
    const doc = new DOMParser().parseFromString(out, 'text/html')
    const img = doc.querySelector('img')
    expect(img).not.toBeNull()
    expect(img?.getAttribute('data-mxc')).toBe('mxc://hs/x')
    expect(img?.hasAttribute('src')).toBe(false)
    expect(img?.getAttribute('alt')).toBe('a diagram')
  })

  it('drops a remote img src entirely — no tracking-pixel load', () => {
    const out = renderMatrixHtml('<img src="https://evil/x.png" alt="pixel">')
    const doc = new DOMParser().parseFromString(out, 'text/html')
    const img = doc.querySelector('img')
    expect(img).not.toBeNull()
    expect(img?.hasAttribute('src')).toBe(false)
    expect(img?.hasAttribute('data-mxc')).toBe(false)
    expect(img?.getAttribute('alt')).toBe('pixel')
  })

  it('removes mx-reply fallbacks with their contents', () => {
    const out = renderMatrixHtml(
      '<mx-reply><blockquote>quoted fallback</blockquote></mx-reply>the actual reply',
    )
    expect(out).toBe('the actual reply')
  })

  it('maps data-mx-color and data-mx-bg-color to validated inline styles', () => {
    const out = renderMatrixHtml(
      '<span data-mx-color="#ff0000" data-mx-bg-color="#00ff00">c</span>',
    )
    expect(out).toContain('color: rgb(255, 0, 0)')
    expect(out).toContain('background-color: rgb(0, 255, 0)')

    // Non-hex payloads (a CSS injection attempt) are ignored.
    const bad = renderMatrixHtml(
      '<span data-mx-color="expression(alert(1))">c</span>',
    )
    expect(bad).not.toContain('style=')
  })

  it('maps legacy font color (Element /rainbow) to inline styles', () => {
    const out = renderMatrixHtml(
      '<font color="#ff0000">r</font><font color="#00ff00">g</font>',
    )
    expect(out).toContain('color: rgb(255, 0, 0)')
    expect(out).toContain('color: rgb(0, 255, 0)')

    // data-mx-color wins over the legacy attribute when both are present.
    const both = renderMatrixHtml(
      '<font color="#00ff00" data-mx-color="#0000ff">x</font>',
    )
    expect(both).toContain('color: rgb(0, 0, 255)')
    expect(both).not.toContain('rgb(0, 255, 0)')

    // Non-hex values are ignored; `color` is only honored on <font>.
    expect(renderMatrixHtml('<font color="red">x</font>')).not.toContain(
      'style=',
    )
    expect(renderMatrixHtml('<span color="#ff0000">x</span>')).not.toContain(
      'style=',
    )
  })

  it('turns data-mx-spoiler into an accessible click-to-reveal span', () => {
    const out = renderMatrixHtml(
      '<span data-mx-spoiler="CW: ending">he dies</span>',
    )
    expect(out).toContain('class="spoiler"')
    expect(out).toContain('title="CW: ending"')
    expect(out).toContain('role="button"')
    expect(out).toContain('aria-label="spoiler: CW: ending"')
    expect(out).toContain('he dies')

    expect(
      renderMatrixHtml('<span data-mx-spoiler>big reveal</span>'),
    ).toContain('aria-label="spoiler"')
  })

  it('strips classes other than code language-*', () => {
    expect(renderMatrixHtml('<p class="banner error">x</p>')).toBe('<p>x</p>')
    expect(renderMatrixHtml('<code class="language-js evil">x</code>')).toBe(
      '<code class="language-js">x</code>',
    )
    // A sender cannot pre-place the spoiler class without data-mx-spoiler.
    expect(renderMatrixHtml('<span class="spoiler">x</span>')).toBe(
      '<span>x</span>',
    )
  })

  it('linkifies bare URLs in formatted text nodes', () => {
    const out = renderMatrixHtml('see <b>https://example.org/x</b> now.')
    expect(out).toContain(
      '<a href="https://example.org/x" target="_blank" rel="noopener noreferrer">https://example.org/x</a>',
    )
  })

  it('does not linkify inside existing anchors, code, or pre', () => {
    expect(
      renderMatrixHtml('<a href="https://a.example">https://a.example</a>'),
    ).not.toContain('<a href="https://a.example"><a')
    expect(renderMatrixHtml('<code>https://example.org</code>')).toBe(
      '<code>https://example.org</code>',
    )
    expect(renderMatrixHtml('<pre>curl https://example.org</pre>')).toBe(
      '<pre>curl https://example.org</pre>',
    )
  })

  it('keeps unknown-tag text content (KEEP_CONTENT default)', () => {
    expect(
      renderMatrixHtml('<details><summary>s</summary>d</details>'),
    ).toContain('<summary>s</summary>')
    expect(renderMatrixHtml('<marquee>zoom</marquee>')).toBe('zoom')
  })
})
