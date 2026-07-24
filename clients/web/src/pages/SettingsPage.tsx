import { useState } from 'preact/hooks'
import { BUILD_INFO } from '../build-info'
import {
  installOutcome,
  installPromptAvailable,
  promptInstallApp,
} from '../install-prompt'
import {
  matrixProtocolHandlerAvailable,
  registerMatrixProtocolHandler,
} from '../matrix-protocol'
import { useServices } from '../services'
import { currentPlatform, isApplePlatform } from '../shortcuts'
import type { Theme, TimeFormat } from '../stores/settings'
import { AccountLifecycle } from './AccountsPage'

const THEMES: { value: Theme; label: string }[] = [
  { value: 'system', label: 'System' },
  { value: 'light', label: 'Light' },
  { value: 'dark', label: 'Dark' },
]

const TIME_FORMATS: { value: TimeFormat; label: string }[] = [
  { value: '12h', label: '12-hour (3:05pm)' },
  { value: '24h', label: '24-hour (15:05)' },
]

/** Theme + (schema-versioned) local settings (ADR 0046, M-W3). */
export function SettingsPage() {
  const { auth, settings, rooms, deviceState } = useServices()
  const [markingRead, setMarkingRead] = useState(false)
  const [protocolMessage, setProtocolMessage] = useState<string | null>(null)

  const markAllRead = async () => {
    setMarkingRead(true)
    let current = rooms.rooms.value
    try {
      if (current.length === 0) {
        await rooms.refresh()
        current = rooms.rooms.value
      }
      const accounts = new Set(current.map((room) => room.account_id))
      await Promise.all(
        [...accounts].map((accountId) =>
          deviceState.markRoomSummariesRead(accountId, current),
        ),
      )
    } catch {
      // The device-state store keeps the optimistic read-marker cache and
      // requeues network-failed writes; the command should still clear local
      // badges instead of throwing from a fire-and-forget click handler.
    } finally {
      for (const room of current) {
        rooms.noteUnreadCounts(room.account_id, room.room_id, 0, 0)
      }
      setMarkingRead(false)
    }
  }

  const setMatrixProtocolHandler = (enabled: boolean) => {
    if (!enabled) {
      settings.matrixProtocolHandler.value = false
      setProtocolMessage(null)
      return
    }
    const result = registerMatrixProtocolHandler()
    if (result.ok) {
      settings.matrixProtocolHandler.value = true
      setProtocolMessage('Matrix link handling registered for this browser.')
    } else {
      settings.matrixProtocolHandler.value = false
      setProtocolMessage(result.message)
    }
  }

  return (
    <div class="page">
      <h1>Settings</h1>
      <section class="panel">
        <h2>Theme</h2>
        <div class="theme-picker" role="radiogroup" aria-label="Theme">
          {THEMES.map(({ value, label }) => (
            <label key={value}>
              <input
                type="radio"
                name="theme"
                value={value}
                checked={settings.theme.value === value}
                onChange={() => (settings.theme.value = value)}
              />
              {label}
            </label>
          ))}
        </div>
      </section>
      <section class="panel">
        <h2>Timeline</h2>
        <label class="setting-row">
          <input
            type="checkbox"
            checked={settings.showStateEvents.value}
            onChange={(event) =>
              (settings.showStateEvents.value = event.currentTarget.checked)
            }
          />
          Show state events
        </label>
        <p class="muted">
          Joins, leaves, topic and name changes. Hidden by default, as in the
          terminal client.
        </p>
        <label class="setting-row">
          <input
            type="checkbox"
            checked={settings.hideRedactedEvents.value}
            onChange={(event) =>
              (settings.hideRedactedEvents.value = event.currentTarget.checked)
            }
          />
          Hide deleted messages
        </label>
        <p class="muted">
          Remove redacted message placeholders from the timeline. Off by
          default.
        </p>
        <div
          class="theme-picker"
          role="radiogroup"
          aria-label="Timestamp format"
        >
          {TIME_FORMATS.map(({ value, label }) => (
            <label key={value}>
              <input
                type="radio"
                name="time-format"
                value={value}
                checked={settings.timeFormat.value === value}
                onChange={() => (settings.timeFormat.value = value)}
              />
              {label}
            </label>
          ))}
        </div>
        <p class="muted">Timestamp format for timeline messages.</p>
        <label class="setting-row">
          <input
            type="checkbox"
            checked={settings.developerMode.value}
            onChange={(event) =>
              (settings.developerMode.value = event.currentTarget.checked)
            }
          />
          Developer mode
        </label>
        <p class="muted">
          Adds per-event diagnostics to the timeline. Inspect panels show
          decrypted event content already returned by the Axon API.
        </p>
      </section>
      <section class="panel">
        <h2>Room list</h2>
        <label class="setting-row">
          <input
            type="checkbox"
            checked={settings.previewRoom.value}
            onChange={(event) =>
              (settings.previewRoom.value = event.currentTarget.checked)
            }
          />
          Preview room
        </label>
        <p class="muted">
          Show the latest message excerpt under each room name.
        </p>
        <button type="button" onClick={() => void markAllRead()}>
          {markingRead ? 'Marking…' : 'Mark all as read'}
        </button>
      </section>
      <InstallAppSettings />
      <section class="panel">
        <h2>Matrix links</h2>
        <label class="setting-row">
          <input
            type="checkbox"
            checked={settings.matrixProtocolHandler.value}
            disabled={!matrixProtocolHandlerAvailable()}
            onChange={(event) =>
              setMatrixProtocolHandler(event.currentTarget.checked)
            }
          />
          Handle <code>matrix:</code> links
        </label>
        <p class="muted">
          Registers this web origin as a browser handler for Matrix room links.
          Your browser may ask for permission.
        </p>
        {!matrixProtocolHandlerAvailable() && (
          <p class="muted">
            This browser does not support protocol-handler registration.
          </p>
        )}
        {protocolMessage !== null && <p class="muted">{protocolMessage}</p>}
      </section>
      <section class="panel">
        <h2>Accounts</h2>
        <AccountLifecycle />
      </section>
      <section class="panel">
        <h2>Session</h2>
        <button type="button" class="danger" onClick={() => auth.clearToken()}>
          Sign out
        </button>
        <p class="muted">
          Sign out clears this browser's Axon access and refresh tokens.
        </p>
      </section>
      <p class="muted">
        Settings are stored in this browser (<code>localStorage</code>), not on
        the server.
      </p>
      <footer class="settings-version muted">
        Web client <code>{BUILD_INFO.version}</code> · built{' '}
        <time dateTime={BUILD_INFO.builtAt}>{BUILD_INFO.builtAtLabel}</time>
        {' · '}
        <a href="/licenses">Open-source licenses</a>
      </footer>
    </div>
  )
}

function InstallAppSettings() {
  const [installing, setInstalling] = useState(false)
  const platform = detectInstallPlatform()
  const copy = installCopy(platform)
  const installed = isInstalledDisplay()

  const install = async () => {
    setInstalling(true)
    try {
      await promptInstallApp()
    } finally {
      setInstalling(false)
    }
  }

  return (
    <section class="panel">
      <h2>{copy.heading}</h2>
      {installed ? (
        <p class="muted">{copy.installed}</p>
      ) : installPromptAvailable.value ? (
        <>
          <button type="button" onClick={() => void install()}>
            {installing ? 'Opening…' : copy.button}
          </button>
          <InstallOutcomeMessage />
        </>
      ) : installOutcome.value !== 'idle' ? (
        <InstallOutcomeMessage />
      ) : platform === 'ios' ? (
        <ol class="install-steps">
          <li>Tap the Share button in Safari.</li>
          <li>Choose Add to Home Screen.</li>
          <li>Tap Add.</li>
        </ol>
      ) : platform === 'android' ? (
        <p class="muted">
          Open your browser menu and choose Add to home screen. Chrome will also
          show an install button here when it makes the prompt available.
        </p>
      ) : (
        <p class="muted">{copy.unavailable}</p>
      )}
    </section>
  )
}

function InstallOutcomeMessage() {
  switch (installOutcome.value) {
    case 'accepted':
      return <p class="muted">Install request accepted.</p>
    case 'dismissed':
      return <p class="muted">Install request dismissed.</p>
    case 'error':
      return <p class="muted">Install prompt could not be opened.</p>
    default:
      return null
  }
}

type InstallPlatform =
  'android' | 'ios' | 'linux' | 'macos' | 'windows' | 'other'

interface InstallCopy {
  heading: string
  button: string
  installed: string
  unavailable: string
}

function detectInstallPlatform(): InstallPlatform {
  const platform = currentPlatform().toLowerCase()
  const userAgent = navigator.userAgent.toLowerCase()
  const touchPoints = navigator.maxTouchPoints ?? 0
  if (/android/.test(userAgent)) {
    return 'android'
  }
  if (/\b(iphone|ipad|ipod)\b/.test(userAgent)) {
    return 'ios'
  }
  if (/win/.test(platform) || /windows/.test(userAgent)) {
    return 'windows'
  }
  if (isApplePlatform(platform, touchPoints)) {
    return 'macos'
  }
  if (/linux|x11/.test(platform) || /linux|x11/.test(userAgent)) {
    return 'linux'
  }
  return 'other'
}

function installCopy(platform: InstallPlatform): InstallCopy {
  switch (platform) {
    case 'android':
    case 'ios':
      return {
        heading: 'Home screen',
        button: 'Add to home screen',
        installed: 'Axon is already running from your home screen.',
        unavailable:
          'Home-screen install is available from supported mobile browsers.',
      }
    case 'windows':
      return {
        heading: 'Desktop app',
        button: 'Add to Start Menu',
        installed: 'Axon is already available from your Start menu.',
        unavailable:
          'Desktop app install is available from supported browsers.',
      }
    case 'macos':
      return {
        heading: 'Desktop app',
        button: 'Add to Applications',
        installed: 'Axon is already available from Applications.',
        unavailable:
          'Desktop app install is available from supported browsers.',
      }
    case 'linux':
      return {
        heading: 'Desktop app',
        button: 'Install desktop app',
        installed: 'Axon is already available from your app launcher.',
        unavailable:
          'Desktop app install is available from supported browsers.',
      }
    default:
      return {
        heading: 'Install app',
        button: 'Install Axon',
        installed: 'Axon is already installed as an app.',
        unavailable: 'App install is available from supported browsers.',
      }
  }
}

function isInstalledDisplay(): boolean {
  const navigatorStandalone = (
    navigator as Navigator & { standalone?: boolean }
  ).standalone
  return (
    navigatorStandalone === true ||
    window.matchMedia?.('(display-mode: standalone)').matches === true
  )
}
