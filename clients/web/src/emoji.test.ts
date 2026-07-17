import { describe, expect, it } from 'vitest'
import {
  currentEmojiEntries,
  emojiShortcodeSuggestions,
  resolveEmojiShortcode,
  type EmojiEntry,
  replaceEmojiShortcodes,
} from './emoji'

describe('emoji shortcode messages', () => {
  it('replaces known :shortcode: tokens with emoji', () => {
    expect(replaceEmojiShortcodes(':+1:')).toBe('👍')
    expect(replaceEmojiShortcodes('ship it :+1:')).toBe('ship it 👍')
  })

  it('resolves reaction shortcodes with or without colons', () => {
    expect(resolveEmojiShortcode('partying')).toBe('🥳')
    expect(resolveEmojiShortcode(':partying:')).toBe('🥳')
  })

  it('leaves unknown shortcode-shaped text literal', () => {
    expect(replaceEmojiShortcodes(':not_an_emoji:')).toBe(':not_an_emoji:')
  })

  it('uses a backslash to send a known shortcode literally', () => {
    expect(replaceEmojiShortcodes('\\:+1:')).toBe(':+1:')
    expect(replaceEmojiShortcodes('literal \\:smile:')).toBe('literal :smile:')
  })

  it('suggests emoji by shortcode and tag', () => {
    const plus = emojiShortcodeSuggestions(currentEmojiEntries(), '+')
    expect(plus[0].value).toBe(':+1:')
    expect(plus[0].label).toContain('👍')

    const smile = emojiShortcodeSuggestions(currentEmojiEntries(), 'smile')
    expect(smile.some((option) => option.value === ':smile:')).toBe(true)
  })

  it('keeps enough emoji matches available for keyboard scrolling', () => {
    const entries: EmojiEntry[] = Array.from({ length: 12 }, (_, index) => ({
      shortcodes: [`smile${index}`],
      annotation: `smile ${index}`,
      emoji: '😄',
      order: index,
      tags: ['smile'],
    }))

    const matches = emojiShortcodeSuggestions(entries, 's')

    expect(matches).toHaveLength(12)
    expect(matches.at(-1)?.value).toBe(':smile11:')
  })
})
