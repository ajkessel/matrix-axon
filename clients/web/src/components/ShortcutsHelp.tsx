import { keyLabel, shortcutLabel, SHORTCUTS, useShortcuts } from '../shortcuts'
import { SLASH_COMMANDS } from '../slash-commands'
import { useModalFocus } from './use-modal-focus'

/**
 * The help popup, opened with `?` or `/help`. Keyboard shortcuts are rendered
 * from `SHORTCUTS`; slash commands are rendered from `SLASH_COMMANDS`, keeping
 * both lists single-sourced without mixing commands into shortcut rows.
 */
export function ShortcutsHelp({ onClose }: { onClose: () => void }) {
  const { containerRef } = useModalFocus<HTMLDivElement>()
  useShortcuts(
    {
      Escape: (event) => {
        event.preventDefault()
        onClose()
      },
    },
    { whileTyping: true, capture: true },
  )

  return (
    <div
      ref={containerRef}
      class="overlay"
      role="dialog"
      aria-modal="true"
      aria-label="Help"
    >
      <div class="overlay-panel">
        <div class="overlay-head">
          <h2>Help</h2>
          <button type="button" class="ghost" onClick={onClose}>
            Close
          </button>
        </div>
        <section class="shortcut-group">
          <h3>Keyboard shortcuts</h3>
          {SHORTCUTS.map(({ group, rows }) => (
            <section key={group} class="shortcut-subgroup">
              <h4>{group}</h4>
              <dl class="shortcut-list">
                {rows.map((row, index) => (
                  <div key={`${group}-${index}`} class="shortcut-row">
                    <dt>
                      <kbd>
                        {typeof row.keys === 'string'
                          ? shortcutLabel(row.keys)
                          : keyLabel(row.keys)}
                      </kbd>
                    </dt>
                    <dd>{row.description}</dd>
                  </div>
                ))}
              </dl>
            </section>
          ))}
        </section>
        <section class="shortcut-group">
          <h3>Commands</h3>
          <dl class="shortcut-list">
            {SLASH_COMMANDS.map((command) => (
              <div key={command.name} class="shortcut-row">
                <dt>
                  <kbd>{command.usage}</kbd>
                </dt>
                <dd>{command.description}</dd>
              </div>
            ))}
          </dl>
        </section>
      </div>
    </div>
  )
}
