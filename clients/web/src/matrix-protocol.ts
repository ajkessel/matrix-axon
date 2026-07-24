export type MatrixProtocolRegistrationResult =
  { ok: true } | { ok: false; message: string }

export function matrixProtocolHandlerAvailable(
  navigatorLike: Navigator = navigator,
): boolean {
  return typeof navigatorLike.registerProtocolHandler === 'function'
}

export function matrixProtocolHandlerUrl(
  origin: string = window.location.origin,
): string {
  return `${origin}/?matrix=%s`
}

export function registerMatrixProtocolHandler(
  navigatorLike: Navigator = navigator,
  origin: string = window.location.origin,
): MatrixProtocolRegistrationResult {
  if (!matrixProtocolHandlerAvailable(navigatorLike)) {
    return {
      ok: false,
      message: 'This browser does not support protocol-handler registration.',
    }
  }
  try {
    navigatorLike.registerProtocolHandler(
      'matrix',
      matrixProtocolHandlerUrl(origin),
    )
    return { ok: true }
  } catch (cause) {
    return {
      ok: false,
      message: cause instanceof Error ? cause.message : String(cause),
    }
  }
}
