import { afterEach, describe, expect, it } from 'vitest'
import {
  isGalleryUngrouped,
  resetGalleryExpansion,
  setGalleryUngrouped,
} from './gallery-expansion'

afterEach(resetGalleryExpansion)

describe('gallery expansion memory', () => {
  it('starts grouped', () => {
    expect(isGalleryUngrouped(['$1', '$2'])).toBe(false)
  })

  it('remembers an ungrouped gallery', () => {
    setGalleryUngrouped(['$1', '$2'], true)
    expect(isGalleryUngrouped(['$1', '$2'])).toBe(true)
  })

  it('forgets it again on regroup', () => {
    setGalleryUngrouped(['$1', '$2'], true)
    setGalleryUngrouped(['$1', '$2'], false)
    expect(isGalleryUngrouped(['$1', '$2'])).toBe(false)
  })

  it('keeps galleries independent', () => {
    setGalleryUngrouped(['$1', '$2'], true)
    expect(isGalleryUngrouped(['$3', '$4'])).toBe(false)
  })

  it('survives the run gaining an image', () => {
    // Back-pagination can pull in an older image that joins the run, which
    // changes its first event. A key derived from that would silently
    // re-group a gallery the reader had opened.
    setGalleryUngrouped(['$2', '$3'], true)
    expect(isGalleryUngrouped(['$1', '$2', '$3'])).toBe(true)
  })

  it('survives the run splitting', () => {
    setGalleryUngrouped(['$1', '$2', '$3'], true)
    expect(isGalleryUngrouped(['$1'])).toBe(true)
    expect(isGalleryUngrouped(['$2', '$3'])).toBe(true)
  })

  it('regrouping one half does not regroup the other', () => {
    setGalleryUngrouped(['$1', '$2', '$3'], true)
    setGalleryUngrouped(['$1'], false)
    expect(isGalleryUngrouped(['$1'])).toBe(false)
    expect(isGalleryUngrouped(['$2', '$3'])).toBe(true)
  })
})
