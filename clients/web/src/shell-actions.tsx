import { createContext } from 'preact'
import { useContext } from 'preact/hooks'

export interface ShellActions {
  jumpAction: (() => void) | null
  setJumpAction(action: (() => void) | null): void
  roomTitle: string | null
  roomInfoAction: (() => void) | null
  setRoomChrome(title: string | null, action: (() => void) | null): void
}

export const ShellActionsContext = createContext<ShellActions>({
  jumpAction: null,
  setJumpAction: () => {},
  roomTitle: null,
  roomInfoAction: null,
  setRoomChrome: () => {},
})

export function useShellActions(): ShellActions {
  return useContext(ShellActionsContext)
}
