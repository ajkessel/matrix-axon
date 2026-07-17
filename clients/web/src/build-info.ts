export interface BuildInfo {
  version: string
  builtAt: string
  builtAtLabel: string
}

function formatBuiltAt(iso: string): string {
  const date = new Date(iso)
  if (Number.isNaN(date.getTime())) {
    return iso
  }
  return date.toLocaleString(undefined, {
    dateStyle: 'medium',
    timeStyle: 'short',
  })
}

export const BUILD_INFO: BuildInfo = {
  version: __AXON_WEB_VERSION__,
  builtAt: __AXON_WEB_BUILT_AT__,
  builtAtLabel: formatBuiltAt(__AXON_WEB_BUILT_AT__),
}
