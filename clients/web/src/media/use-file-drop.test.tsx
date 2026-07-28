import { cleanup, fireEvent, render } from '@testing-library/preact'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { useFileDrop } from './use-file-drop'

afterEach(cleanup)

/** A drag payload the hook should recognize as carrying a file. */
const withFiles = (files: File[] = []) => ({
  dataTransfer: { types: ['Files'], files },
})
/** Dragging selected text, not a file — the hook must ignore it entirely. */
const withText = { dataTransfer: { types: ['text/plain'], files: [] } }

function Harness({ onFile }: { onFile?: (files: FileList) => void }) {
  const { dragging, handlers } = useFileDrop(onFile ?? (() => {}))
  return (
    <div data-testid="pane" {...handlers}>
      {dragging && <span data-testid="overlay">Drop to attach</span>}
      <span data-testid="child">a timeline row</span>
    </div>
  )
}

describe('useFileDrop (ADR 0065)', () => {
  const png = () => new File(['bytes'], 'cat.png', { type: 'image/png' })

  it('stays armed while the cursor crosses child elements', () => {
    const { getByTestId, queryByTestId } = render(<Harness />)
    const pane = getByTestId('pane')
    const child = getByTestId('child')

    fireEvent.dragEnter(pane, withFiles())
    expect(queryByTestId('overlay')).not.toBeNull()

    // Entering a child fires `dragenter` on it *and* `dragleave` on the parent.
    // Counting enters against leaves is what stops the overlay flickering off
    // as the cursor moves across the timeline's rows.
    fireEvent.dragEnter(child, withFiles())
    fireEvent.dragLeave(pane, withFiles())
    expect(queryByTestId('overlay')).not.toBeNull()

    // Only the final leave, back out of the pane, disarms it.
    fireEvent.dragLeave(child, withFiles())
    expect(queryByTestId('overlay')).toBeNull()
  })

  it('ignores a drag that carries no file', () => {
    const onFile = vi.fn()
    const { getByTestId, queryByTestId } = render(<Harness onFile={onFile} />)
    const pane = getByTestId('pane')

    fireEvent.dragEnter(pane, withText)
    expect(queryByTestId('overlay')).toBeNull()

    fireEvent.drop(pane, withText)
    expect(onFile).not.toHaveBeenCalled()
  })

  it('passes every file of a multi-file drop through', () => {
    // ADR 0065 took only the first and reported the rest as skipped; ADR 0081
    // sends them all. The caps live in the staging hook, so deciding here what
    // to drop would put that rule in two places and let them disagree.
    const onFile = vi.fn()
    const { getByTestId, queryByTestId } = render(<Harness onFile={onFile} />)
    const pane = getByTestId('pane')
    const first = png()
    const rest = [png(), png()]

    fireEvent.dragEnter(pane, withFiles([first]))
    fireEvent.drop(pane, withFiles([first, ...rest]))

    expect(onFile).toHaveBeenCalledTimes(1)
    expect([...onFile.mock.calls[0][0]]).toHaveLength(3)
    // The overlay clears on drop, not on some later leave that never comes.
    expect(queryByTestId('overlay')).toBeNull()
  })

  it('re-arms cleanly for a second drag after a drop', () => {
    const { getByTestId, queryByTestId } = render(<Harness />)
    const pane = getByTestId('pane')

    fireEvent.dragEnter(pane, withFiles())
    fireEvent.drop(pane, withFiles([png()]))
    // A drop resets the counter; a stale count would leave the next drag needing
    // two leaves to disarm.
    fireEvent.dragEnter(pane, withFiles())
    expect(queryByTestId('overlay')).not.toBeNull()

    fireEvent.dragLeave(pane, withFiles())
    expect(queryByTestId('overlay')).toBeNull()
  })
})
