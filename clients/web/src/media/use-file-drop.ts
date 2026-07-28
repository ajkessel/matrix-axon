import { useCallback, useRef, useState } from 'preact/hooks'

/**
 * Drag-and-drop of a file onto a pane (ADR 0065). Scoped to the element it is
 * spread onto, not the window: the thread panel sits *beside* the room stream,
 * and a page-wide drop target would stage a file dropped on the thread into the
 * room's composer — sending it to the wrong place.
 *
 * A drop stages the file; it does not send. `dragging` drives the drop overlay.
 *
 * `dragenter`/`dragleave` fire for every child element crossed, so a plain
 * boolean flickers off as the cursor moves over the timeline's rows. Counting
 * enters against leaves is the usual fix.
 */
export function useFileDrop(onFiles: (files: FileList) => void): {
  dragging: boolean
  handlers: {
    onDragEnter(event: DragEvent): void
    onDragOver(event: DragEvent): void
    onDragLeave(event: DragEvent): void
    onDrop(event: DragEvent): void
  }
} {
  const [dragging, setDragging] = useState(false)
  const depth = useRef(0)

  const carriesFile = (event: DragEvent) =>
    Array.from(event.dataTransfer?.types ?? []).includes('Files')

  const reset = useCallback(() => {
    depth.current = 0
    setDragging(false)
  }, [])

  return {
    dragging,
    handlers: {
      onDragEnter(event) {
        if (!carriesFile(event)) {
          return
        }
        depth.current += 1
        setDragging(true)
      },
      onDragOver(event) {
        if (!carriesFile(event)) {
          return
        }
        // Without this the browser navigates to the dropped file.
        event.preventDefault()
      },
      onDragLeave(event) {
        if (!carriesFile(event)) {
          return
        }
        depth.current -= 1
        if (depth.current <= 0) {
          reset()
        }
      },
      onDrop(event) {
        if (!carriesFile(event)) {
          return
        }
        event.preventDefault()
        reset()
        // The whole list goes through (ADR 0081). Only the staging hook knows
        // the caps, so deciding here what to drop would put that rule in two
        // places and let them disagree.
        const files = event.dataTransfer?.files
        if (files !== undefined && files.length > 0) {
          onFiles(files)
        }
      },
    },
  }
}
