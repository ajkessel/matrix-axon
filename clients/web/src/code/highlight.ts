import DOMPurify from 'dompurify'
import { canonicalLanguage, loadLanguage } from './languages'

/**
 * Largest source we will highlight, in characters. `highlight.js` tokenises
 * synchronously on the main thread, so a big file is a visible stall — and a
 * 200 KB log is not something anyone reads with syntax colours anyway. Above
 * this the caller renders plain text, which is what it did before this
 * existed.
 */
export const HIGHLIGHT_MAX_CHARS = 128 * 1024

/** The engine, loaded once and shared. Kept out of the main bundle. */
let enginePromise: Promise<
  typeof import('highlight.js/lib/core').default
> | null = null

/** Grammars already registered on the shared engine. */
const registered = new Set<string>()

async function engine() {
  enginePromise ??= import('highlight.js/lib/core').then((mod) => mod.default)
  return enginePromise
}

/**
 * Highlight `source` as `language`, returning sanitized HTML for the inside of
 * a `<code>` element — or `null` when we cannot or should not highlight it, in
 * which case the caller renders the plain text it already has.
 *
 * The output is passed through DOMPurify even though `highlight.js` escapes
 * its input and emits only `<span class>`. That is belt-and-braces on purpose:
 * this is the one place in the app where a string becomes `innerHTML` without
 * going through `renderMatrixHtml`, and the cost is a few microseconds on a
 * dependency we already ship.
 */
export async function highlightCode(
  source: string,
  language: string,
): Promise<string | null> {
  const canonical = canonicalLanguage(language)
  if (canonical === null || source.length > HIGHLIGHT_MAX_CHARS) {
    return null
  }

  const hljs = await engine()
  if (!registered.has(canonical)) {
    const grammar = await loadLanguage(canonical)
    if (grammar === null) {
      return null
    }
    // Two callers can race to here; `registerLanguage` is idempotent, and the
    // set only exists to skip the dynamic import the second time.
    hljs.registerLanguage(canonical, grammar)
    registered.add(canonical)
  }

  try {
    const { value } = hljs.highlight(source, {
      language: canonical,
      ignoreIllegals: true,
    })
    return DOMPurify.sanitize(value, {
      ALLOWED_TAGS: ['span'],
      ALLOWED_ATTR: ['class'],
    })
  } catch {
    // A grammar that throws is a highlighting failure, not a rendering one.
    return null
  }
}
