/**
 * Which of the three shell layouts the current path wants (ADR 0062):
 * `room` and `rooms` are the two-pane surfaces (sidebar + right pane), while
 * `utility` is the full-width centered column with no sidebar at all.
 *
 * Lives apart from `app.tsx` so the room list can consult it — the shell
 * imports the room list, so the room list cannot import the shell.
 */
export type LayoutMode = 'room' | 'rooms' | 'utility'

/**
 * The single-pane media query (ADR 0062): below this width only one pane
 * shows, chosen by the route. Must stay in lockstep with the `47.99rem` /
 * `48rem` breakpoints in `index.css` — this constant exists so JS callers
 * (`RoomPage`'s focus handoff) can't drift from the stylesheet (WCR-17).
 */
export const SINGLE_PANE_QUERY = '(max-width: 47.99rem)'

const ROOM_PATH = /^\/[^/]+\/rooms\/.+/

export function layoutMode(path: string): LayoutMode {
  if (ROOM_PATH.test(path)) {
    return 'room'
  }
  return path === '/' ? 'rooms' : 'utility'
}
