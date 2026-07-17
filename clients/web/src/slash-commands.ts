export const SLASH_COMMAND = {
  help: '/help',
  jump: '/jump',
  react: '/react',
  reply: '/reply',
  room: '/room',
  search: '/search',
  thread: '/thread',
  whereami: '/whereami',
} as const

export type SlashCommandName =
  (typeof SLASH_COMMAND)[keyof typeof SLASH_COMMAND]

export interface SlashCommandSpec {
  name: SlashCommandName
  usage: string
  description: string
}

export const SLASH_COMMANDS: SlashCommandSpec[] = [
  {
    name: SLASH_COMMAND.react,
    usage: '/react [emoji]',
    description: 'React to the latest visible message',
  },
  {
    name: SLASH_COMMAND.reply,
    usage: '/reply [message]',
    description: 'Reply to the latest visible message',
  },
  {
    name: SLASH_COMMAND.thread,
    usage: '/thread',
    description: 'Open a thread for the latest visible message',
  },
  {
    name: SLASH_COMMAND.room,
    usage: '/room <room>',
    description: 'Switch rooms by name, alias, ID, or number',
  },
  {
    name: SLASH_COMMAND.search,
    usage: '/search [filters] [text]',
    description: 'Search messages (this room by default; room:, sender:, …)',
  },
  {
    name: SLASH_COMMAND.jump,
    usage: '/jump [YYYY-MM-DD]',
    description: 'Jump to a date, or open the date picker',
  },
  {
    name: SLASH_COMMAND.whereami,
    usage: '/whereami',
    description: 'Show room information',
  },
  {
    name: SLASH_COMMAND.help,
    usage: '/help',
    description: 'Show this list',
  },
]

/** The usage line for a command, for the error a misused command answers with. */
export function slashCommandUsage(name: SlashCommandName): string {
  return SLASH_COMMANDS.find((command) => command.name === name)?.usage ?? name
}
