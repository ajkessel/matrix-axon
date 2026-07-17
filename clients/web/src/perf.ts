const STORAGE_KEY = 'axon.perf'

let enabled: boolean | null = null

export function perfEnabled(): boolean {
  if (enabled !== null) {
    return enabled
  }
  const params = new URLSearchParams(window.location.search)
  if (params.get('perf') === '1') {
    enabled = true
    try {
      window.sessionStorage.setItem(STORAGE_KEY, '1')
    } catch {
      // Private-mode storage failures should not break the app.
    }
    return true
  }
  try {
    enabled = window.sessionStorage.getItem(STORAGE_KEY) === '1'
  } catch {
    enabled = false
  }
  return enabled
}

export function perfMark(
  name: string,
  detail?: Record<string, string | number | boolean | null>,
): void {
  if (!perfEnabled()) {
    return
  }
  const markName = `axon:${name}`
  try {
    if (detail === undefined) {
      performance.mark(markName)
    } else {
      performance.mark(markName, { detail })
    }
  } catch {
    performance.mark(markName)
  }
}

export function perfMarkFrames(name: string): void {
  if (!perfEnabled()) {
    return
  }
  perfMark(`${name}:now`)
  requestAnimationFrame(() => {
    perfMark(`${name}:raf1`)
    requestAnimationFrame(() => perfMark(`${name}:raf2`))
  })
}
