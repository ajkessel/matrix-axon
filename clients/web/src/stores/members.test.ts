import { HttpResponse, http } from 'msw'
import { setupServer } from 'msw/node'
import { afterAll, afterEach, beforeAll, describe, expect, it } from 'vitest'
import { createApiClient } from '../api/client'
import { createMembersStore } from './members'

const BASE_URL = 'http://axon.test'
const ACCOUNT = '6b53f7f0-0000-4000-8000-000000000001'
const ROOM = '!room:example.org'

const server = setupServer()
beforeAll(() => server.listen({ onUnhandledRequest: 'error' }))
afterEach(() => server.resetHandlers())
afterAll(() => server.close())

function makeStore() {
  const api = createApiClient(
    {
      getToken: () => 'tok-test',
      onAuthFailure: () => {},
      LoginBootstrap: () => null,
    },
    BASE_URL,
  )
  return createMembersStore(api, ACCOUNT, ROOM)
}

describe('createMembersStore', () => {
  it('refreshes the roster after a partial invite failure', async () => {
    const invited: string[] = []
    let memberReads = 0
    server.use(
      http.post(
        `${BASE_URL}/v1/accounts/${ACCOUNT}/rooms/${encodeURIComponent(ROOM)}/invite`,
        async ({ request }) => {
          const body = (await request.json()) as { user_id: string }
          if (body.user_id === '@bob:example.org') {
            return HttpResponse.json(
              {
                error: {
                  code: 'bad_request',
                  message: 'invalid user id',
                },
              },
              { status: 400 },
            )
          }
          invited.push(body.user_id)
          return HttpResponse.json({ data: {} })
        },
      ),
      http.get(
        `${BASE_URL}/v1/accounts/${ACCOUNT}/rooms/${encodeURIComponent(ROOM)}/members`,
        () => {
          memberReads += 1
          return HttpResponse.json({
            data: [
              {
                user_id: '@alice:example.org',
                membership: 'invite',
                display_name: 'Alice',
                avatar_url: null,
              },
            ],
          })
        },
      ),
    )
    const store = makeStore()

    const result = await store.inviteUsers([
      '@alice:example.org',
      '@bob:example.org',
    ])

    expect(result).toEqual({
      ok: false,
      message: 'invalid user id',
      code: 'bad_request',
    })
    expect(invited).toEqual(['@alice:example.org'])
    expect(memberReads).toBe(1)
    expect(store.members.value.get('@alice:example.org')?.membership).toBe(
      'invite',
    )
    expect(store.error.value).toBe('invalid user id')
  })
})
