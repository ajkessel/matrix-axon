import { Marked, type Token, type Tokens } from 'marked'
import { sanitizeOutgoingHtml } from '../html/sanitize'

/**
 * Markdown-on-send (ADR 0046, M-W7), mirroring the TUI's
 * `markdown_to_html_if_detected` (`clients/tui/src/html.rs`): the composer is
 * a plain textarea; on send, text that *contains Markdown formatting* is
 * converted to HTML for `formatted_body`, while plain prose — paragraphs,
 * text, and line breaks only — is sent as a bare `body` with no formatted
 * part. The server carries both verbatim (it never interprets Markdown).
 *
 * Raw inline HTML in the input is escaped, not passed through: the composer
 * is a Markdown surface, not an HTML one (receiving clients sanitize anyway;
 * this just keeps our own output honest).
 */

const escapeHtml = (text: string): string =>
  text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')

const md = new Marked({ gfm: true })
md.use({
  renderer: {
    html: (token: Tokens.HTML | Tokens.Tag) => escapeHtml(token.text),
  },
})

/** Block-level tokens that plain prose produces. */
const PLAIN_BLOCKS = new Set(['paragraph', 'space'])
/** Inline tokens that plain prose produces (`br` = trailing-space break). */
const PLAIN_INLINE = new Set(['text', 'br', 'escape'])

function isPlain(tokens: Token[]): boolean {
  for (const token of tokens) {
    if (token.type === 'paragraph') {
      const inline = (token as Tokens.Paragraph).tokens ?? []
      if (inline.some((t) => !PLAIN_INLINE.has(t.type))) {
        return false
      }
    } else if (!PLAIN_BLOCKS.has(token.type)) {
      return false
    }
  }
  return true
}

/**
 * Convert `text` to HTML if it contains Markdown formatting; `null` when it
 * is plain prose (only paragraphs, text, and line breaks — the TUI's rule),
 * in which case the message is sent without a `formatted_body`.
 */
export function markdownToHtmlIfFormatted(text: string): string | null {
  const tokens = md.lexer(text)
  if (isPlain(tokens)) {
    return null
  }
  // Sanitized before it leaves (WCR-15): marked would pass a
  // `[x](javascript:…)` href through live. Receivers must sanitize anyway;
  // this keeps our own output honest.
  return sanitizeOutgoingHtml((md.parser(tokens) as string).trim())
}
