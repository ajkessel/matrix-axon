/** `1.4 MB`, `912 KB`, `48 B` — or null when no size is known. */
export function humanSize(bytes: number | undefined): string | null {
  if (bytes === undefined) {
    return null
  }
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  const rounded =
    unit === 0 || value >= 10 ? Math.round(value) : value.toFixed(1)
  return `${rounded} ${units[unit]}`
}
