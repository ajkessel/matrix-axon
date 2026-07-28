/**
 * `0:42`, `3:07`, `1:02:33` — or null when the sender declared no duration.
 * The input is Matrix's `info.duration`, in milliseconds.
 */
export function humanDuration(ms: number | undefined): string | null {
  if (ms === undefined || !Number.isFinite(ms) || ms <= 0) {
    return null
  }
  const total = Math.round(ms / 1000)
  const pad = (value: number) => String(value).padStart(2, '0')
  const seconds = total % 60
  const minutes = Math.floor(total / 60) % 60
  const hours = Math.floor(total / 3600)
  return hours > 0
    ? `${hours}:${pad(minutes)}:${pad(seconds)}`
    : `${minutes}:${pad(seconds)}`
}

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
