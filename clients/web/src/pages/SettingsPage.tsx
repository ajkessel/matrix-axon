import { BUILD_INFO } from '../build-info'
import { useServices } from '../services'
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
  const { auth, settings } = useServices()

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
