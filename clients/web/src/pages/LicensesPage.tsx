import { THIRD_PARTY_LICENSES } from '../thirdparty'

/**
 * Third-party open-source disclosure. The list is generated at build time from
 * the pnpm production dependency tree (see the `axon-thirdparty-licenses`
 * plugin in vite.config.ts) and linked from the Settings footer.
 *
 * The explicit "Back to settings" link matters: the web client is an
 * installable standalone PWA, where the browser back button may be absent
 * (notably iOS), so navigation must not depend on browser chrome.
 */
export function LicensesPage() {
  const licenses = THIRD_PARTY_LICENSES
  const count = licenses.length

  return (
    <div class="page">
      <h1>Open-source licenses</h1>
      <p class="muted">
        The Axon web client is built with the open-source software listed below.
        We are grateful to its authors. Each package is distributed under its
        own license, reproduced in full.
      </p>
      <p class="muted">
        {count === 0
          ? 'License information is unavailable for this build.'
          : `${count} third-party ${count === 1 ? 'package' : 'packages'}.`}
      </p>
      {licenses.map((pkg) => (
        <details key={`${pkg.name}@${pkg.version}`} class="license-entry">
          <summary>
            <span class="license-name">{pkg.name}</span>{' '}
            <span class="muted">
              {pkg.version} · {pkg.license}
            </span>
          </summary>
          {pkg.homepage ? (
            <p class="muted">
              <a href={pkg.homepage} target="_blank" rel="noopener noreferrer">
                {pkg.homepage}
              </a>
            </p>
          ) : null}
          <pre class="license-text">{pkg.text}</pre>
        </details>
      ))}
      <p>
        <a href="/settings">← Back to settings</a>
      </p>
    </div>
  )
}
