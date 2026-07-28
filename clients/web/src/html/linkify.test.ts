import { describe, expect, it } from 'vitest'
import { matchUrls } from './linkify'

describe('matchUrls', () => {
  it.each([
    'matrix:r/a:bostoncoop.net',
    'matrix:room/a:bostoncoop.net',
    'matrix:r/ops%3Aexample.org?via=example.org',
  ])('matches bare Matrix room URI %s', (url) => {
    expect(matchUrls(`join ${url} now`)).toEqual([
      { url, start: 5, end: 5 + url.length },
    ])
  })
})
