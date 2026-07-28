export function normalizeUserId(
  input: string,
  defaultServerName: string | null,
): string | null {
  const trimmed = input.trim()
  const fullUserId = splitUserId(trimmed.startsWith('@') ? trimmed : null)
  if (fullUserId !== null) {
    return fullUserId
  }
  const fullUserIdMissingSigil = splitUserId(`@${trimmed}`)
  if (fullUserIdMissingSigil !== null) {
    return fullUserIdMissingSigil
  }
  if (defaultServerName === null) {
    return null
  }
  const localpart = trimmed.startsWith('@') ? trimmed.slice(1) : trimmed
  if (!/^[^\s@:]+$/.test(localpart)) {
    return null
  }
  return `@${localpart.toLowerCase()}:${defaultServerName}`
}

function splitUserId(input: string | null): string | null {
  if (input === null || !input.startsWith('@')) {
    return null
  }
  const colon = input.indexOf(':')
  if (colon <= 1 || colon === input.length - 1) {
    return null
  }
  const localpart = input.slice(1, colon)
  const serverName = input.slice(colon + 1)
  if (!/^[^\s@:]+$/.test(localpart) || !validServerName(serverName)) {
    return null
  }
  return `@${localpart.toLowerCase()}:${serverName}`
}

function validServerName(serverName: string): boolean {
  if (serverName === '' || /\s/.test(serverName)) {
    return false
  }
  if (serverName.startsWith('[')) {
    const end = serverName.indexOf(']')
    if (end <= 1) {
      return false
    }
    const suffix = serverName.slice(end + 1)
    return suffix === '' || /^:\d+$/.test(suffix)
  }
  const colon = serverName.indexOf(':')
  if (colon === -1) {
    return true
  }
  return (
    colon > 0 &&
    serverName.indexOf(':', colon + 1) === -1 &&
    /^\d+$/.test(serverName.slice(colon + 1))
  )
}

export function parseUserIdList(
  input: string,
  defaultServerName: string | null,
): { ok: true; userIds: string[] } | { ok: false; message: string } {
  const raw = input
    .split(/[,\s]+/)
    .map((part) => part.trim())
    .filter((part) => part !== '')
  const userIds = []
  for (const part of raw) {
    const userId = normalizeUserId(part, defaultServerName)
    if (userId === null) {
      return {
        ok: false,
        message: `Enter invitees as Matrix user IDs, not "${part}".`,
      }
    }
    userIds.push(userId)
  }
  return { ok: true, userIds: [...new Set(userIds)] }
}
