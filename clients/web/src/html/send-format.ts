import { sanitizeOutgoingHtml } from './sanitize'

export interface FormattedMessage {
  body: string
  formattedBody: string | null
}

const RAINBOW_SATURATION = 1
const RAINBOW_LIGHTNESS = 0.5

export function rawHtmlMessage(html: string): FormattedMessage {
  const formattedBody = sanitizeOutgoingHtml(html)
  const body = stripHtmlToPlain(formattedBody) || html
  return { body, formattedBody }
}

export function literalMessage(text: string): FormattedMessage {
  return { body: text, formattedBody: null }
}

export function rainbowMessage(text: string): FormattedMessage {
  return { body: text, formattedBody: rainbowHtml(text) }
}

export function spoilerMessage(
  reason: string | null,
  text: string,
): FormattedMessage {
  const escapedText = escapeHtml(text)
  const formattedBody =
    reason === null
      ? `<span data-mx-spoiler>${escapedText}</span>`
      : `<span data-mx-spoiler="${escapeHtml(reason)}">${escapedText}</span>`
  return {
    body:
      reason === null ? `${text} (Spoiler)` : `${reason}: ${text} (Spoiler)`,
    formattedBody,
  }
}

function stripHtmlToPlain(html: string): string {
  const parsed = new DOMParser().parseFromString(html, 'text/html')
  return (parsed.body.textContent ?? '').trim()
}

function rainbowHtml(text: string): string {
  const chars = Array.from(text)
  if (chars.length === 0) {
    return ''
  }
  return chars
    .map((char, index) => {
      const hue = (index / chars.length) * 360
      const [r, g, b] = hslToRgb(hue, RAINBOW_SATURATION, RAINBOW_LIGHTNESS)
      return `<font color="#${hex(r)}${hex(g)}${hex(b)}">${escapeHtml(char)}</font>`
    })
    .join('')
}

function hslToRgb(h: number, s: number, l: number): [number, number, number] {
  const c = (1 - Math.abs(2 * l - 1)) * s
  const x = c * (1 - Math.abs(((h / 60) % 2) - 1))
  const m = l - c / 2
  let r1 = c
  let g1 = 0
  let b1 = 0
  if (h >= 60 && h < 120) {
    r1 = x
    g1 = c
  } else if (h >= 120 && h < 180) {
    r1 = 0
    g1 = c
    b1 = x
  } else if (h >= 180 && h < 240) {
    r1 = 0
    g1 = x
    b1 = c
  } else if (h >= 240 && h < 300) {
    r1 = x
    b1 = c
  } else if (h >= 300) {
    g1 = 0
    b1 = x
  } else {
    g1 = x
  }
  return [
    Math.round((r1 + m) * 255),
    Math.round((g1 + m) * 255),
    Math.round((b1 + m) * 255),
  ]
}

function hex(value: number): string {
  return value.toString(16).padStart(2, '0')
}

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
}
