import { describe, expect, it } from 'vitest'
import { fullReactionPickerPosition } from './MessageEventRow'

describe('fullReactionPickerPosition', () => {
  it('aligns bottom-left to the anchor top-right when there is room above', () => {
    expect(
      fullReactionPickerPosition({
        anchor: { top: 500, right: 300, bottom: 524 },
        dialog: { width: 400, height: 300 },
        viewport: { width: 1000, height: 800 },
      }),
    ).toEqual({ left: 300, top: 200 })
  })

  it('places the picker below the anchor when above would leave the viewport', () => {
    expect(
      fullReactionPickerPosition({
        anchor: { top: 100, right: 300, bottom: 124 },
        dialog: { width: 400, height: 300 },
        viewport: { width: 1000, height: 800 },
      }),
    ).toEqual({ left: 300, top: 132 })
  })

  it('clamps to viewport margins when neither preferred position fits', () => {
    expect(
      fullReactionPickerPosition({
        anchor: { top: 120, right: 950, bottom: 144 },
        dialog: { width: 400, height: 300 },
        viewport: { width: 500, height: 320 },
      }),
    ).toEqual({ left: 92, top: 8 })
  })
})
