import type { ComposerAutocompleteOption } from './components/Composer'

export const EMOJI_PICKER_DATA_SOURCE = '/emoji-picker-data-en.json'

interface RawEmojiEntry {
  shortcodes?: unknown
  annotation?: unknown
  emoji?: unknown
  order?: unknown
  tags?: unknown
}

export interface EmojiEntry {
  shortcodes: string[]
  annotation: string
  emoji: string
  order: number
  tags: string[]
}

const SHORTCODE = /^[A-Za-z0-9_+-]+$/
const SHORTCODE_TOKEN = /^:([A-Za-z0-9_+-]+):/
const MAX_EMOJI_SUGGESTIONS = 64

const FALLBACK_EMOJI_ENTRIES: readonly EmojiEntry[] = [
  {
    shortcodes: ['+1', 'thumbsup', 'yes'],
    annotation: 'thumbs up',
    emoji: '👍',
    order: 353,
    tags: ['+1', 'good', 'hand', 'like', 'thumb', 'up', 'yes'],
  },
  {
    shortcodes: ['smile', 'grinning_face_with_closed_eyes'],
    annotation: 'grinning face with smiling eyes',
    emoji: '😄',
    order: 3,
    tags: ['face', 'grin', 'happy', 'laugh', 'smile', 'smiling'],
  },
  {
    shortcodes: ['hooray', 'partying', 'partying_face'],
    annotation: 'partying face',
    emoji: '🥳',
    order: 76,
    tags: [
      'bday',
      'birthday',
      'celebrate',
      'celebration',
      'excited',
      'face',
      'happy',
      'hat',
      'hooray',
      'horn',
      'party',
      'partying',
    ],
  },
]

let cachedEmojiEntries: readonly EmojiEntry[] = FALLBACK_EMOJI_ENTRIES
let loaded = false
let pending: Promise<readonly EmojiEntry[]> | null = null

export function currentEmojiEntries(): readonly EmojiEntry[] {
  return cachedEmojiEntries
}

export function loadEmojiEntries(): Promise<readonly EmojiEntry[]> {
  if (loaded) {
    return Promise.resolve(cachedEmojiEntries)
  }
  pending ??= fetch(EMOJI_PICKER_DATA_SOURCE)
    .then((response) => {
      if (!response.ok) {
        throw new Error(`emoji data fetch failed: ${response.status}`)
      }
      return response.json() as Promise<unknown>
    })
    .then((data) => {
      const parsed = parseEmojiData(data)
      if (parsed.length > 0) {
        cachedEmojiEntries = parsed
      }
      loaded = true
      return cachedEmojiEntries
    })
    .catch(() => {
      loaded = true
      return cachedEmojiEntries
    })
  return pending
}

export function emojiShortcodeSuggestions(
  entries: readonly EmojiEntry[],
  query: string,
): ComposerAutocompleteOption[] {
  const normalized = query.toLowerCase()
  return entries
    .filter((entry) =>
      emojiSearchFields(entry).some((field) =>
        field.toLowerCase().startsWith(normalized),
      ),
    )
    .slice(0, MAX_EMOJI_SUGGESTIONS)
    .map((entry) => {
      const shortcode =
        entry.shortcodes.find((candidate) =>
          candidate.toLowerCase().startsWith(normalized),
        ) ?? entry.shortcodes[0]
      return {
        value: `:${shortcode}:`,
        label: `${normalizeEmoji(entry.emoji)} :${shortcode}:`,
        description: entry.annotation,
      }
    })
}

export function replaceEmojiShortcodes(
  text: string,
  entries: readonly EmojiEntry[] = cachedEmojiEntries,
): string {
  const emojiByShortcode = new Map<string, string>()
  for (const entry of entries) {
    for (const shortcode of entry.shortcodes) {
      const key = shortcode.toLowerCase()
      if (!emojiByShortcode.has(key)) {
        emojiByShortcode.set(key, normalizeEmoji(entry.emoji))
      }
    }
  }

  let result = ''
  let index = 0
  while (index < text.length) {
    if (text[index] === '\\' && text[index + 1] === ':') {
      const escaped = SHORTCODE_TOKEN.exec(text.slice(index + 1))?.[0]
      if (escaped !== undefined) {
        result += escaped
        index += escaped.length + 1
        continue
      }
    }
    if (text[index] === ':') {
      const match = SHORTCODE_TOKEN.exec(text.slice(index))
      if (match !== null) {
        const shortcode = match[1]
        const emoji = emojiByShortcode.get(shortcode.toLowerCase())
        result += emoji ?? match[0]
        index += match[0].length
        continue
      }
    }
    result += text[index]
    index += 1
  }
  return result
}

export function resolveEmojiShortcode(
  input: string,
  entries: readonly EmojiEntry[] = cachedEmojiEntries,
): string | null {
  const shortcode = input.trim().replace(/^:/, '').replace(/:$/, '')
  if (!SHORTCODE.test(shortcode)) {
    return null
  }
  const normalized = shortcode.toLowerCase()
  for (const entry of entries) {
    if (
      entry.shortcodes.some(
        (candidate) => candidate.toLowerCase() === normalized,
      )
    ) {
      return normalizeEmoji(entry.emoji)
    }
  }
  return null
}

function parseEmojiData(raw: unknown): EmojiEntry[] {
  if (!Array.isArray(raw)) {
    return []
  }
  return raw
    .map((entry) => normalizeEntry(entry as RawEmojiEntry))
    .filter((entry): entry is EmojiEntry => entry !== null)
    .sort((a, b) => a.order - b.order)
}

function normalizeEntry(entry: RawEmojiEntry): EmojiEntry | null {
  if (typeof entry.emoji !== 'string' || !Array.isArray(entry.shortcodes)) {
    return null
  }
  const shortcodes = entry.shortcodes.filter(
    (shortcode): shortcode is string =>
      typeof shortcode === 'string' && SHORTCODE.test(shortcode),
  )
  if (shortcodes.length === 0) {
    return null
  }
  return {
    emoji: entry.emoji,
    shortcodes,
    annotation: typeof entry.annotation === 'string' ? entry.annotation : '',
    order:
      typeof entry.order === 'number' ? entry.order : Number.MAX_SAFE_INTEGER,
    tags: Array.isArray(entry.tags)
      ? entry.tags.filter((tag): tag is string => typeof tag === 'string')
      : [],
  }
}

function emojiSearchFields(entry: EmojiEntry): string[] {
  return [...entry.shortcodes, entry.annotation, ...entry.tags]
}

function normalizeEmoji(emoji: string): string {
  return emoji.replaceAll('\uFE0F', '')
}
