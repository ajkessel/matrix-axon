import { describe, expect, it } from 'vitest'
import { inviteErrorMessage } from './invite'

describe('inviteErrorMessage', () => {
  it('keeps the useful already-in-room detail from upstream forbidden errors', () => {
    expect(
      inviteErrorMessage({
        ok: false,
        code: 'forbidden',
        message:
          'the server returned an error: [403 / M_FORBIDDEN] @j:bostoncoop.net is already in the room.',
      }),
    ).toBe('Could not invite users. @j:bostoncoop.net is already in the room.')
  })

  it('uses a permission-focused message for other forbidden invite errors', () => {
    expect(
      inviteErrorMessage({
        ok: false,
        code: 'forbidden',
        message: 'invite blocked',
      }),
    ).toBe(
      'Could not invite users. This account is not allowed to invite people to this room.',
    )
  })
})
