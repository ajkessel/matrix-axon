import { describe, expect, it } from 'vitest'
import { formatEventTime } from './EventStatus'

describe('formatEventTime', () => {
  it('renders compact 12-hour row timestamps', () => {
    expect(formatEventTime(new Date(2026, 6, 16, 7, 26).getTime())).toBe(
      '7:26am',
    )
    expect(formatEventTime(new Date(2026, 6, 16, 17, 5).getTime())).toBe(
      '5:05pm',
    )
    expect(formatEventTime(new Date(2026, 6, 16, 0, 0).getTime())).toBe(
      '12:00am',
    )
    expect(formatEventTime(new Date(2026, 6, 16, 12, 0).getTime())).toBe(
      '12:00pm',
    )
  })
})
