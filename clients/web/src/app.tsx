import { LocationProvider, Route, Router, useLocation } from 'preact-iso'
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from 'preact/hooks'
import { ConnectionIndicator } from './components/ConnectionIndicator'
import { RoomList } from './components/RoomList'
import { SearchOverlay } from './components/SearchOverlay'
import { ShortcutsHelp } from './components/ShortcutsHelp'
import { UnreadThreadsPanel } from './components/UnreadThreadsPanel'
import { layoutMode, SINGLE_PANE_QUERY, useMediaQuery } from './layout'
import { withSearchParam } from './search-tokens'
import { ShellActionsContext } from './shell-actions'
import { AccountsPage } from './pages/AccountsPage'
import { NotFound } from './pages/NotFound'
import { RoomPage } from './pages/RoomPage'
import { RoomsIndex } from './pages/RoomsIndex'
import { SettingsPage } from './pages/SettingsPage'
import { perfMark, perfMarkFrames } from './perf'
import {
  createServices,
  ServicesContext,
  useServices,
  type AppServices,
} from './services'
import {
  hint,
  isApplePlatform,
  keyAria,
  KEYS,
  SHOW_HELP_EVENT,
  useShortcuts,
} from './shortcuts'
import { applyTheme } from './stores/settings'

/**
 * App root (ADR 0046, M-W3). History routing — signed off under ADR 0046
 * open question 5 — with the URL shape that is the deep-link contract:
 * `/:accountId/rooms/:roomId` plus `?thread=` / `?event=` query params.
 * A Tauri-conditional hash fallback is M-W12's problem if the `file://`
 * WebView needs one.
 *
 * `services` is injectable for tests; the app builds the real graph once.
 */
export function App({ services }: { services?: AppServices }) {
  // eslint-disable-next-line react-hooks/exhaustive-deps -- build the real graph exactly once
  const svc = useMemo(() => services ?? createServices(), [])

  useEffect(() => applyTheme(svc.settings, document.documentElement), [svc])
  useVisualViewportShell()
  useStandaloneKeyboardAccessoryInset()

  // Hold the live socket open only while signed in; sign-out tears it down
  // (M-W6, ADR 0061). Reconnect/backoff on unexpected drops arrives in step 3.
  useEffect(() => {
    if (!svc.auth.signedIn.value) {
      return
    }
    svc.live.start()
    return () => svc.live.stop()
  }, [svc, svc.auth.signedIn.value])

  return (
    <ServicesContext.Provider value={svc}>
      {svc.auth.signedIn.value ? (
        <Shell />
      ) : window.location.pathname === '/oauth/callback' ? (
        <OAuthCallback />
      ) : (
        <SignedOut />
      )}
    </ServicesContext.Provider>
  )
}

function useVisualViewportShell(): void {
  useEffect(() => {
    const root = document.documentElement
    const viewport = window.visualViewport
    const clear = () => {
      root.style.removeProperty('--app-viewport-top')
      root.style.removeProperty('--app-viewport-left')
      root.style.removeProperty('--app-viewport-width')
      root.style.removeProperty('--app-viewport-height')
    }
    if (viewport == null) {
      clear()
      return
    }
    const update = () => {
      root.style.setProperty('--app-viewport-top', `${viewport.offsetTop}px`)
      root.style.setProperty('--app-viewport-left', `${viewport.offsetLeft}px`)
      root.style.setProperty('--app-viewport-width', `${viewport.width}px`)
      root.style.setProperty('--app-viewport-height', `${viewport.height}px`)
    }
    update()
    viewport.addEventListener('resize', update)
    viewport.addEventListener('scroll', update)
    window.addEventListener('orientationchange', update)
    return () => {
      viewport.removeEventListener('resize', update)
      viewport.removeEventListener('scroll', update)
      window.removeEventListener('orientationchange', update)
      clear()
    }
  }, [])
}

function useStandaloneKeyboardAccessoryInset(): void {
  useEffect(() => {
    const root = document.documentElement
    const standaloneMedia = window.matchMedia?.('(display-mode: standalone)')
    const clear = () => {
      root.style.removeProperty('--app-keyboard-accessory-inset')
      root.style.removeProperty('--app-standalone-composer-bottom-padding')
    }
    const update = () => {
      const active = document.activeElement
      const editableFocused =
        active instanceof HTMLElement &&
        (active.matches('textarea, input') ||
          active.getAttribute('contenteditable') === 'true')
      if (isApplePlatform() && isStandaloneDisplay(standaloneMedia)) {
        root.style.setProperty(
          '--app-standalone-composer-bottom-padding',
          '4px',
        )
        if (editableFocused) {
          root.style.setProperty('--app-keyboard-accessory-inset', '4px')
        } else {
          root.style.removeProperty('--app-keyboard-accessory-inset')
        }
      } else {
        clear()
      }
    }

    update()
    document.addEventListener('focusin', update)
    const updateAfterBlur = () => setTimeout(update)
    document.addEventListener('focusout', updateAfterBlur)
    standaloneMedia?.addEventListener?.('change', update)
    return () => {
      document.removeEventListener('focusin', update)
      document.removeEventListener('focusout', updateAfterBlur)
      standaloneMedia?.removeEventListener?.('change', update)
      clear()
    }
  }, [])
}

function isStandaloneDisplay(media: MediaQueryList | undefined): boolean {
  const navigatorStandalone = (
    navigator as Navigator & { standalone?: boolean }
  ).standalone
  return navigatorStandalone === true || media?.matches === true
}

function isReloadOrRestoreNavigation(): boolean {
  const [entry] = performance.getEntriesByType(
    'navigation',
  ) as PerformanceNavigationTiming[]
  if (entry !== undefined) {
    return entry.type === 'reload' || entry.type === 'back_forward'
  }
  const legacyNavigation = (
    performance as Performance & {
      navigation?: {
        type: number
        TYPE_RELOAD: number
        TYPE_BACK_FORWARD: number
      }
    }
  ).navigation
  return (
    legacyNavigation !== undefined &&
    (legacyNavigation.type === legacyNavigation.TYPE_RELOAD ||
      legacyNavigation.type === legacyNavigation.TYPE_BACK_FORWARD)
  )
}

/** The signed-out state: the auth provider's bootstrap UI. */
function SignedOut() {
  const { auth } = useServices()
  return (
    <main class="signin">
      <h1>axon</h1>
      <p>Sign in with SSO or a server-issued access token.</p>
      <auth.LoginBootstrap />
    </main>
  )
}

function OAuthCallback() {
  const { auth } = useServices()
  const [message, setMessage] = useState('Completing sign-in...')
  const [failed, setFailed] = useState(false)

  useEffect(() => {
    let cancelled = false
    void auth
      .completeOAuthRedirect(new URL(window.location.href))
      .then((result) => {
        if (cancelled || result.ok) {
          return
        }
        setFailed(true)
        setMessage(result.message)
      })
    return () => {
      cancelled = true
    }
  }, [auth])

  return (
    <main class="signin">
      <h1>axon</h1>
      <p class={failed ? 'error' : 'muted'}>{message}</p>
      {failed && <a href="/">Back to sign in</a>}
    </main>
  )
}

/** The signed-in shell. `LocationProvider` wraps the chrome, not the reverse. */
function Shell() {
  return (
    <LocationProvider>
      <ShellChrome />
    </LocationProvider>
  )
}

/**
 * Header plus the two panes (ADR 0062). Separate from `Shell` because
 * `useLocation` reads the context `Shell` itself only *renders* — a hook call
 * up there would see no provider — and both the topbar and the panes need the
 * current layout mode.
 *
 * The sidebar stays mounted in every mode and is hidden with CSS. Unmounting
 * it on a room switch would throw away its scroll position and the room list's
 * session-only name/account filters.
 */
function ShellChrome() {
  const location = useLocation()
  const { path, query } = location
  const { accounts, settings, threadUnread } = useServices()
  const mode = layoutMode(path)
  perfMark('shell:render', { path, mode })
  const collapsed = settings.sidebarCollapsed.value
  const [helpOpen, setHelpOpen] = useState(false)
  const [unreadThreadsOpen, setUnreadThreadsOpen] = useState(false)
  const [jumpAction, setJumpActionState] = useState<(() => void) | null>(null)
  const [roomChrome, setRoomChromeState] = useState<{
    title: string | null
    action: (() => void) | null
  }>({ title: null, action: null })
  const singlePane = useMediaQuery(SINGLE_PANE_QUERY)
  // The search overlay is URL-addressed (ADR 0066): mounted while `?search=`
  // is present (even empty), so a shared link restores it and Back closes it.
  const searchOpen = typeof query.search === 'string'
  const accountCount = accounts.accounts.value.length
  const accountsLoading = accounts.loading.value
  const accountsError = accounts.error.value
  const unreadThreadCount = threadUnread.count.value
  const roomTitleButton = useRef<HTMLButtonElement>(null)
  const startupThreadScrubbed = useRef(false)

  const openHelp = (event: KeyboardEvent) => {
    event.preventDefault()
    setHelpOpen(true)
  }

  const openSearch = (event?: KeyboardEvent) => {
    event?.preventDefault()
    if (!searchOpen) {
      location.route(withSearchParam(location.url, ''))
    }
  }
  const setJumpAction = useCallback((action: (() => void) | null) => {
    setJumpActionState(() => action)
  }, [])
  const setRoomChrome = useCallback(
    (title: string | null, action: (() => void) | null) => {
      setRoomChromeState({ title, action })
    },
    [],
  )
  const shellActions = useMemo(
    () => ({
      jumpAction,
      setJumpAction,
      roomTitle: roomChrome.title,
      roomInfoAction: roomChrome.action,
      setRoomChrome,
    }),
    [jumpAction, roomChrome, setJumpAction, setRoomChrome],
  )
  const mobileRoomChrome = mode === 'room' && singlePane

  useLayoutEffect(() => {
    perfMark('shell:layout-effect', { path, mode })
    perfMarkFrames('shell:post-layout')
    if (startupThreadScrubbed.current) {
      return
    }
    startupThreadScrubbed.current = true
    if (typeof query.thread !== 'string' || !isReloadOrRestoreNavigation()) {
      return
    }
    const params = new URLSearchParams(window.location.search)
    params.delete('thread')
    const nextQuery = params.toString()
    location.route(`${path}${nextQuery === '' ? '' : `?${nextQuery}`}`, true)
  }, [location, mode, path, query.thread])

  useEffect(() => {
    if (mobileRoomChrome) {
      roomTitleButton.current?.focus()
    }
  }, [mobileRoomChrome, path])

  useEffect(() => {
    const onShowHelp = () => setHelpOpen(true)
    window.addEventListener(SHOW_HELP_EVENT, onShowHelp)
    return () => window.removeEventListener(SHOW_HELP_EVENT, onShowHelp)
  }, [])
  useEffect(() => {
    if (accountsLoading) {
      void accounts.refresh()
    }
  }, [accounts, accountsLoading])
  useEffect(() => {
    if (
      !accountsLoading &&
      accountsError === null &&
      accountCount === 0 &&
      path !== '/accounts'
    ) {
      location.route('/accounts', true)
    }
  }, [accountCount, accountsError, accountsLoading, location, path])

  useShortcuts({
    // Bare characters, so `useShortcuts` already withholds them while typing.
    '?': openHelp,
    '/': openSearch,
  })
  useShortcuts(
    {
      // Search's modifier twin (ADR 0066), reachable from the composer.
      'mod+shift+f': (event) => {
        if (isApplePlatform()) {
          return
        }
        openSearch(event)
      },
      'mod+g': (event) => {
        if (!isApplePlatform()) {
          return
        }
        openSearch(event)
      },
      'mod+b': (event) => {
        if (mode === 'utility' || singlePane) {
          return // no sidebar here to collapse
        }
        event.preventDefault()
        settings.sidebarCollapsed.value = !collapsed
      },
      Escape: (event) => {
        if (path !== '/settings') {
          return
        }
        event.preventDefault()
        location.route('/')
      },
      // Help must be reachable from the composer, where focus usually is, and
      // `?` never can be — it has to reach the textarea as a character.
      // Ctrl+Shift+/ reports `event.key === '?'` on a US layout and `/` on
      // others, so accept every spelling rather than guess the layout.
      'mod+/': openHelp,
      'mod+shift+/': openHelp,
      'mod+shift+?': openHelp,
    },
    { whileTyping: true },
  )

  return (
    <ShellActionsContext.Provider value={shellActions}>
      <div class={`shell${mobileRoomChrome ? ' mobile-room-shell' : ''}`}>
        <header class="topbar">
          <a href="/" class="brand topbar-brand">
            axon
          </a>
          {mobileRoomChrome && (
            <a
              href="/"
              class="ghost topbar-icon-button topbar-rooms-button"
              aria-label="Rooms"
              title="Rooms"
            >
              <MenuIcon />
            </a>
          )}
          <nav class="topbar-nav">
            <a href="/settings" title="Settings" aria-label="Settings">
              <SettingsIcon />
              <span class="topbar-label">Settings</span>
            </a>
          </nav>
          {mobileRoomChrome && (
            <button
              ref={roomTitleButton}
              type="button"
              class="topbar-room-title"
              aria-label="Open room information"
              title="Room information"
              onClick={() => roomChrome.action?.()}
            >
              <span>{roomChrome.title ?? 'Room'}</span>
            </button>
          )}
          <ConnectionIndicator />
          {/* Utility pages have no sidebar to collapse. The toggle lives up here
              rather than in the sidebar because a collapsed sidebar is
              `display: none` and could never render its own way back. */}
          {mode !== 'utility' && !singlePane && (
            <button
              type="button"
              class="ghost"
              aria-expanded={!collapsed}
              aria-controls="room-sidebar"
              title={hint(
                collapsed ? 'Show rooms' : 'Hide rooms',
                KEYS.toggleSidebar,
              )}
              aria-keyshortcuts={keyAria(KEYS.toggleSidebar)}
              onClick={() => (settings.sidebarCollapsed.value = !collapsed)}
            >
              {collapsed ? 'Show rooms' : 'Hide rooms'}
            </button>
          )}
          {mode === 'room' && jumpAction !== null && !singlePane && (
            <button
              type="button"
              class="ghost"
              aria-haspopup="dialog"
              onClick={jumpAction}
            >
              Jump
            </button>
          )}
          <button
            type="button"
            class="ghost topbar-icon-button unread-threads-button"
            title="Unread threads"
            aria-label={
              unreadThreadCount === 0
                ? 'Unread threads'
                : `Unread threads, ${unreadThreadCount}`
            }
            aria-haspopup="dialog"
            onClick={() => setUnreadThreadsOpen(true)}
          >
            <ThreadIcon />
            {unreadThreadCount > 0 && (
              <span class="topbar-count-badge">{unreadThreadCount}</span>
            )}
            <span class="topbar-label">Threads</span>
          </button>
          <button
            type="button"
            class="ghost topbar-icon-button"
            title={hint('Search messages', KEYS.search)}
            aria-label="Search messages"
            aria-keyshortcuts={keyAria(KEYS.search)}
            aria-haspopup="dialog"
            onClick={() => openSearch()}
          >
            <SearchIcon />
            <span class="topbar-label">Search</span>
          </button>
          {/* Keyboard-free discovery: nothing else tells you the chords exist. */}
          <button
            type="button"
            class="ghost help-button topbar-icon-button"
            title={hint('Keyboard shortcuts', KEYS.showHelp)}
            aria-label="Keyboard shortcuts"
            aria-keyshortcuts={keyAria(KEYS.showHelp)}
            aria-haspopup="dialog"
            onClick={() => setHelpOpen(true)}
          >
            ?
          </button>
        </header>

        <div
          class={`shell-body mode-${mode}${collapsed ? ' sidebar-collapsed' : ''}`}
        >
          <nav id="room-sidebar" class="sidebar" aria-label="Rooms">
            <RoomList />
          </nav>
          <main>
            <Router>
              <Route path="/" component={RoomsIndex} />
              <Route path="/accounts" component={AccountsPage} />
              <Route path="/settings" component={SettingsPage} />
              <Route path="/:accountId/rooms/:roomId" component={RoomPage} />
              <Route default component={NotFound} />
            </Router>
          </main>
        </div>

        {searchOpen && <SearchOverlay />}
        {unreadThreadsOpen && (
          <UnreadThreadsPanel onClose={() => setUnreadThreadsOpen(false)} />
        )}
        {helpOpen && <ShortcutsHelp onClose={() => setHelpOpen(false)} />}
      </div>
    </ShellActionsContext.Provider>
  )
}

function ThreadIcon() {
  return (
    <svg class="topbar-icon" viewBox="0 0 24 24" aria-hidden="true">
      <path
        d="M21 12a8 8 0 0 1-8 8H7l-4 3v-7a8 8 0 1 1 18-4Z"
        fill="none"
        stroke="currentColor"
        stroke-linecap="round"
        stroke-linejoin="round"
        stroke-width="2"
      />
      <path
        d="M8 11h8M8 15h5"
        fill="none"
        stroke="currentColor"
        stroke-linecap="round"
        stroke-width="2"
      />
    </svg>
  )
}

function MenuIcon() {
  return (
    <svg class="topbar-icon" viewBox="0 0 24 24" aria-hidden="true">
      <path
        d="M4 7h16M4 12h16M4 17h16"
        fill="none"
        stroke="currentColor"
        stroke-linecap="round"
        stroke-width="2"
      />
    </svg>
  )
}

function SearchIcon() {
  return (
    <svg class="topbar-icon" viewBox="0 0 24 24" aria-hidden="true">
      <path
        d="m21 21-4.35-4.35M11 18a7 7 0 1 1 0-14 7 7 0 0 1 0 14Z"
        fill="none"
        stroke="currentColor"
        stroke-linecap="round"
        stroke-linejoin="round"
        stroke-width="2"
      />
    </svg>
  )
}

function SettingsIcon() {
  return (
    <svg class="topbar-icon" viewBox="0 0 24 24" aria-hidden="true">
      <path
        d="M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7Z"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
      />
      <path
        d="M19.4 15a1.8 1.8 0 0 0 .36 1.98l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06A1.8 1.8 0 0 0 15 19.4a1.8 1.8 0 0 0-1 .55V20a2 2 0 1 1-4 0v-.09a1.8 1.8 0 0 0-1-.55 1.8 1.8 0 0 0-1.98.36l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.8 1.8 0 0 0 4.6 15a1.8 1.8 0 0 0-.55-1H4a2 2 0 1 1 0-4h.09a1.8 1.8 0 0 0 .55-1 1.8 1.8 0 0 0-.36-1.98l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.8 1.8 0 0 0 9 4.6a1.8 1.8 0 0 0 1-.55V4a2 2 0 1 1 4 0v.09a1.8 1.8 0 0 0 1 .55 1.8 1.8 0 0 0 1.98-.36l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.8 1.8 0 0 0 19.4 9c.2.34.39.68.55 1H20a2 2 0 1 1 0 4h-.09a1.8 1.8 0 0 0-.55 1Z"
        fill="none"
        stroke="currentColor"
        stroke-linecap="round"
        stroke-linejoin="round"
        stroke-width="2"
      />
    </svg>
  )
}
