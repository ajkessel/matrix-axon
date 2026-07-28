import { describe, expect, it } from 'vitest'
import {
  SLASH_COMMAND,
  canonicalSlashCommandName,
  slashCommandSpecForInput,
} from './slash-commands'

describe('slash command aliases', () => {
  it('resolves aliases through the shared command metadata', () => {
    expect(canonicalSlashCommandName('/switch')).toBe(SLASH_COMMAND.room)
    expect(canonicalSlashCommandName('/+')).toBe(SLASH_COMMAND.react)
    expect(canonicalSlashCommandName('/?')).toBe(SLASH_COMMAND.help)
    expect(canonicalSlashCommandName('/ut')).toBe(SLASH_COMMAND.unreadthreads)
    expect(canonicalSlashCommandName('/rooms')).toBe(SLASH_COMMAND.refresh)
    expect(canonicalSlashCommandName('/part')).toBe(SLASH_COMMAND.leave)
    expect(canonicalSlashCommandName('/dm')).toBe(SLASH_COMMAND.dm)
    expect(canonicalSlashCommandName('/create')).toBe(SLASH_COMMAND.create)
    expect(canonicalSlashCommandName('/find')).toBe(SLASH_COMMAND.find)
    expect(canonicalSlashCommandName('/invite')).toBe(SLASH_COMMAND.invite)
    expect(canonicalSlashCommandName('/cancel')).toBe(SLASH_COMMAND.cancel)
    expect(canonicalSlashCommandName('/join')).toBe(SLASH_COMMAND.join)
    expect(canonicalSlashCommandName('/knock')).toBe(SLASH_COMMAND.knock)
  })

  it('returns command specs for canonical names and aliases', () => {
    expect(slashCommandSpecForInput('/room')?.name).toBe(SLASH_COMMAND.room)
    expect(slashCommandSpecForInput('/switch')?.name).toBe(SLASH_COMMAND.room)
    expect(slashCommandSpecForInput('/+')?.name).toBe(SLASH_COMMAND.react)
    expect(slashCommandSpecForInput('/part')?.name).toBe(SLASH_COMMAND.leave)
    expect(slashCommandSpecForInput('/join')?.usage).toBe(
      '/join [room-or-matrix-link]',
    )
    expect(slashCommandSpecForInput('/dm')?.usage).toBe('/dm [user]')
    expect(slashCommandSpecForInput('/create')?.usage).toBe('/create')
    expect(slashCommandSpecForInput('/find')?.usage).toBe('/find')
    expect(slashCommandSpecForInput('/invite')?.usage).toBe('/invite <user...>')
    expect(slashCommandSpecForInput('/cancel')?.usage).toBe('/cancel <user>')
    expect(slashCommandSpecForInput('/knock')?.usage).toBe(
      '/knock <room-or-matrix-link> [reason]',
    )
    expect(slashCommandSpecForInput('/missing')).toBeUndefined()
  })
})
