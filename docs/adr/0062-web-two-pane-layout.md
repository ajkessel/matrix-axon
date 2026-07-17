# ADR 0062 — Web client two-pane layout

## Context

The web client (ADR 0046) is single-pane. `Shell()` renders a topbar plus one
`<main>` capped at `max-width: 56rem`, and `preact-iso`'s `<Router>` swaps
exactly one route component into it. The room list (`/`) and a room's timeline
(`/:accountId/rooms/:roomId`) therefore never coexist: opening a room unmounts
the list, discarding its scroll position and its session-only name and account
filters (ADR 0042 — those are deliberately never persisted, so an unmount is
the only thing that can lose them).

Every comparable client — Element, Slack, Discord, Teams — keeps a narrow room
list pinned beside the timeline. Matching that is a UI/UX milestone in its own
right, distinct from the M-W roadmap's feature ladder.

Two existing pieces constrain the design. The thread panel is a `position:
fixed` drawer floating over the whole viewport, opened purely by the `?thread=`
query param (ADR 0046's deep-link contract). And the client has no responsive
infrastructure at all: no width media queries, no `matchMedia`. ADR 0046's open
question 6 settled the posture as *desktop-first, non-broken at narrow widths*,
with mobile-web a stopgap rather than a target (ADR 0031).

## Decision

- **The room list becomes a persistent sidebar in the shell.** `RoomsPage` is
  split into `components/RoomList.tsx` (the sidebar) and `pages/RoomsIndex.tsx`
  (the `/` route's right pane, an empty-state placeholder). `RoomList` is
  mounted once by the shell and **never unmounted** — every mode change hides it
  with CSS instead. That is what preserves scroll position and the session-only
  filters, so those filters stay `useState` rather than moving to a store.

- **Layout mode is derived from the path.** `layoutMode(path)` returns `room`
  (a room URL), `rooms` (`/`), or `utility` (`/accounts`, `/settings`, 404), and
  the shell body carries it as a `mode-*` class. Utility pages are full-width
  with no sidebar, keeping the old centered 56rem column; they are not room
  surfaces and a sidebar beside a settings form buys nothing.

- **`ShellChrome` is a child of `LocationProvider`, not part of `Shell`.**
  `useLocation` reads a context that `Shell` only *renders*; a hook call in
  `Shell` itself would see no provider. The extra component is load-bearing,
  and it holds the topbar as well as the panes because both need the mode.

- **The thread panel becomes a real third column** at wide viewports, shrinking
  the timeline rather than covering the room list. The fixed overlay drawer
  remains the *default* rule and the inline column is layered on top of it in a
  `min-width` query — so the narrow path needs no property to be unset, and a
  viewport too small for three columns falls back for free.

- **The sidebar is fixed-width and collapsible.** Its width is a custom property
  (`--sidebar-width`); the collapsed flag is an additive `sidebarCollapsed`
  field on the `axon.settings` envelope. No version bump: `parse()` already
  defaults every missing field, which is exactly the migration contract the
  store's docstring promises. The toggle lives in the topbar, because a
  collapsed sidebar is `display: none` and a button inside it could never bring
  it back; it is omitted on utility pages, which have no sidebar to collapse.

- **Below 48rem, one pane shows, chosen by the route** — the list at `/`, the
  timeline at a room URL. This is precisely the pre-0062 behavior, so narrow
  screens gain no new navigation concepts and back-navigation stays the topbar
  "Rooms" link and the browser's back button. No drawer, no hamburger.

- **Breakpoints are literals, not tokens.** CSS custom properties are not
  permitted in `@media` conditions. The two values — `48rem` (single pane) and
  `64rem` (thread overlay fallback) — are written out in the three queries that
  need them and documented in one comment block at the top of the shell rules.

## Consequences

- The room list's scroll position and its name/account filters now survive room
  switches, which is the main user-visible win beyond the layout itself.

- `matchMedia` enters the codebase, in exactly one place: at single-pane widths
  the timeline replaces the room list outright, so `RoomPage` moves focus to the
  room title (`tabindex="-1"`) on open. Otherwise a keyboard or screen-reader
  user would be left on a link that no longer exists. Wide layouts keep both
  panes and must not steal focus, hence the query.

- jsdom implements neither `matchMedia` nor `localStorage` here, so the vitest
  config gains a `setupFiles` shim reporting `matches: false` — the wide
  two-pane layout, the right default for component tests.

- The breakpoint literals are duplicated across three media queries. Accepted:
  the alternative is a preprocessor step this client does not otherwise need.

- `.shell main` loses its `max-width`, which moves onto `.mode-utility main`.
  Any future full-width page gets the two-pane treatment by default and must opt
  into centering.

- The Playwright lane (ADR 0061) reaches rooms with `page.goto(roomUrl)` and
  never clicks a room row, so it needed no change — a useful signal that the
  deep-link contract, not the chrome, is what the e2e suite pins.

- Landmarks: the sidebar is `<nav aria-label="Rooms">` (a list of navigational
  links), the right pane stays `<main>`, the thread keeps its `<aside>`.
