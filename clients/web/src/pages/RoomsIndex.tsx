/**
 * The `/` route's right pane (ADR 0062): the room list itself lives in the
 * shell's sidebar, so this page is only the "nothing selected yet" state.
 * At narrow widths the shell hides this pane and shows the sidebar alone.
 */
export function RoomsIndex() {
  return (
    <div class="page rooms-index">
      <p class="muted">Select a room to start reading.</p>
    </div>
  )
}
