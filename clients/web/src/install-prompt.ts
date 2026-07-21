import { signal } from '@preact/signals'

type InstallOutcome = 'idle' | 'accepted' | 'dismissed' | 'installed' | 'error'

export interface BeforeInstallPromptEvent extends Event {
  readonly platforms: string[]
  readonly userChoice: Promise<{
    outcome: 'accepted' | 'dismissed'
    platform: string
  }>
  prompt(): Promise<void>
}

let deferredInstallPrompt: BeforeInstallPromptEvent | null = null

export const installPromptAvailable = signal(false)
export const installOutcome = signal<InstallOutcome>('idle')

export function setupInstallPromptCapture(target: Window = window): () => void {
  const onBeforeInstallPrompt = (event: Event) => {
    event.preventDefault()
    deferredInstallPrompt = event as BeforeInstallPromptEvent
    installPromptAvailable.value = true
    installOutcome.value = 'idle'
  }
  const onAppInstalled = () => {
    deferredInstallPrompt = null
    installPromptAvailable.value = false
    installOutcome.value = 'installed'
  }

  target.addEventListener('beforeinstallprompt', onBeforeInstallPrompt)
  target.addEventListener('appinstalled', onAppInstalled)
  return () => {
    target.removeEventListener('beforeinstallprompt', onBeforeInstallPrompt)
    target.removeEventListener('appinstalled', onAppInstalled)
  }
}

export async function promptInstallApp(): Promise<void> {
  const prompt = deferredInstallPrompt
  if (prompt === null) {
    installPromptAvailable.value = false
    return
  }

  try {
    await prompt.prompt()
    const choice = await prompt.userChoice
    deferredInstallPrompt = null
    installPromptAvailable.value = false
    installOutcome.value =
      choice.outcome === 'accepted' ? 'accepted' : 'dismissed'
  } catch {
    installOutcome.value = 'error'
  }
}

export function resetInstallPromptForTest(): void {
  deferredInstallPrompt = null
  installPromptAvailable.value = false
  installOutcome.value = 'idle'
}
