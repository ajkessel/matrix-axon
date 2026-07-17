/**
 * Bare-URL detection shared by both rendering paths: plain-text bodies
 * (VNode anchors in `FormattedBody`) and formatted bodies (DOM anchors in
 * `sanitize.ts`). Many clients — Element included — send bare URLs as plain
 * text even inside `formatted_body` and rely on the receiver to linkify.
 */

/** A bare http(s) URL; trailing sentence punctuation is trimmed after. */
const URL_PATTERN = /https?:\/\/[^\s<>"]+/g
/** Punctuation that reads as sentence structure, not part of the URL. */
const TRAILING_PUNCTUATION = /[.,;:!?)\]'"]+$/

export interface UrlMatch {
  url: string
  start: number
  /** End of the URL itself (trailing punctuation excluded). */
  end: number
}

/** All bare http(s) URLs in `text`, with sentence punctuation trimmed. */
export function matchUrls(text: string): UrlMatch[] {
  const matches: UrlMatch[] = []
  for (const match of text.matchAll(URL_PATTERN)) {
    let url = match[0]
    const trimmed = TRAILING_PUNCTUATION.exec(url)
    if (trimmed !== null) {
      url = url.slice(0, -trimmed[0].length)
    }
    if (url === 'http://' || url === 'https://') {
      continue
    }
    matches.push({ url, start: match.index, end: match.index + url.length })
  }
  return matches
}

/**
 * Linkify bare URLs in the text nodes under `root`, in place. Text already
 * inside an anchor, code, or preformatted block is left alone (no nested
 * links, no rewriting code samples).
 */
export function linkifyDomTree(root: Element): void {
  const doc = root.ownerDocument
  const walker = doc.createTreeWalker(root, NodeFilter.SHOW_TEXT)
  const textNodes: Text[] = []
  let node = walker.nextNode()
  while (node !== null) {
    textNodes.push(node as Text)
    node = walker.nextNode()
  }
  for (const textNode of textNodes) {
    if (textNode.parentElement?.closest('a, code, pre') != null) {
      continue
    }
    const text = textNode.data
    const matches = matchUrls(text)
    if (matches.length === 0) {
      continue
    }
    const fragment = doc.createDocumentFragment()
    let last = 0
    for (const { url, start, end } of matches) {
      if (start > last) {
        fragment.append(text.slice(last, start))
      }
      const anchor = doc.createElement('a')
      anchor.setAttribute('href', url)
      anchor.setAttribute('target', '_blank')
      anchor.setAttribute('rel', 'noopener noreferrer')
      anchor.textContent = url
      fragment.append(anchor)
      last = end
    }
    if (last < text.length) {
      fragment.append(text.slice(last))
    }
    textNode.replaceWith(fragment)
  }
}
