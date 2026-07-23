import { useEffect, useMemo, useState } from 'preact/hooks'
import { inBackground } from '../api/client'
import { timelineEvent } from '../api/frames'
import { useServices } from '../services'
import type { MembersStore } from '../stores/members'
import type { RoomDto } from '../stores/room-list'
import type { ThreadUnreadStore } from '../stores/thread-unread'
import { createTimelineStore, type EventDto } from '../stores/timeline'
import type { TimelineEvent, TimelineStore } from '../stores/timeline'
import { Composer, type ComposerAutocompleteOption } from './Composer'
import { ErrorBanner } from './ErrorBanner'
import { EventBody } from './EventBody'
import { MessageEventRow } from './MessageEventRow'
import { UserAvatar } from './UserAvatar'
import { useMessageComposer, type ComposerAction } from './use-message-composer'

type ThreadCommandHandler = (
  body: string,
  timeline: TimelineStore,
  visibleEvents: readonly TimelineEvent[],
  isVisibleEvent: (event: TimelineEvent) => boolean,
  action: ComposerAction | null,
  setAction: (action: ComposerAction | null) => void,
  setReactionPickerEventId: (eventId: string | null) => void,
  formatComposerBody: ReturnType<
    typeof useMessageComposer
  >['formatComposerBody'],
) => boolean | Promise<boolean>

/**
 * One open thread (ADR 0046, M-W7; ADR 0032): the root event, the
 * thread-scoped timeline, and a composer that sends into the thread. Opened
 * via `?thread=<root_id>` — the deep-link contract — and closed by clearing
 * that query param (the parent owns the routing).
 */
export function ThreadPanel({
  accountId,
  roomId,
  rootId,
  rootEvent,
  ownUserId,
  members,
  rooms,
  roomTitles,
  threadUnread,
  onCommand,
  roomCompletions,
  onClose,
}: {
  accountId: string
  roomId: string
  rootId: string
  rootEvent: EventDto | undefined
  ownUserId: string | null
  members: MembersStore
  rooms: readonly RoomDto[]
  roomTitles: ReadonlyMap<string, string>
  threadUnread: ThreadUnreadStore
  onCommand?: ThreadCommandHandler
  roomCompletions?(query: string): ComposerAutocompleteOption[]
  onClose: () => void
}) {
  const { api, deviceState, live, media, settings } = useServices()
  const hideRedacted = settings.hideRedactedEvents.value
  const thread = useMemo(
    () => createTimelineStore(api, media, accountId, roomId, rootId),
    [api, media, accountId, roomId, rootId],
  )
  const [root, setRoot] = useState<EventDto | undefined>(rootEvent)
  const [reactionPickerEventId, setReactionPickerEventId] = useState<
    string | null
  >(null)
  const {
    setAction,
    banner: composerBanner,
    attachable,
    attachment,
    stage,
    clearAttachment,
    dragging,
    dropHandlers,
    formatComposerBody,
    mentionCompletions,
    roomReferenceCompletions,
    emojiCompletions,
    submitMessage,
    action,
  } = useMessageComposer({
    timeline: thread,
    accountId,
    members,
    rooms,
    roomTitles,
    ownUserId,
    attachmentScope: `${accountId} ${roomId} ${rootId}`,
  })

  useEffect(() => {
    void thread.loadLatest()
  }, [thread])

  useEffect(() => {
    if (thread.loading.value) {
      return
    }
    const latest = thread.events.value.findLast(
      (event) => !event.event_id.startsWith('local:'),
    )
    if (latest === undefined) {
      return
    }
    deviceState.advanceThreadReadMarker(
      accountId,
      roomId,
      rootId,
      latest.event_id,
      latest.origin_ts,
    )
    threadUnread.markThreadRead(accountId, roomId, rootId)
  }, [
    thread.loading.value,
    thread.events.value,
    deviceState,
    threadUnread,
    accountId,
    roomId,
    rootId,
  ])

  // Live replies while the panel is open, mirroring RoomPage's two effects
  // (WCR-06): the store self-filters to this room and thread, so every
  // timeline frame can be offered. Without this an open thread went silently
  // stale — the main timeline hides thread members, so nothing else showed
  // an incoming reply.
  useEffect(() => {
    return live.subscribe((frame) => {
      const event = timelineEvent(frame)
      if (event !== null) {
        thread.ingestLive(event)
      }
    })
  }, [live, thread])

  // Reconnect gap-fill (ADR 0061): the bus is lossy, so re-read the thread's
  // head after a drop. `reconnects` starts at 0; the initial load above is
  // not doubled.
  useEffect(() => {
    if (live.reconnects.value === 0) {
      return
    }
    void thread.loadLatest()
  }, [live.reconnects.value, thread])

  useEffect(() => {
    if (root !== undefined) {
      return
    }
    let cancelled = false
    inBackground(
      api
        .GET('/v1/accounts/{account_id}/events/{event_id}', {
          params: { path: { account_id: accountId, event_id: rootId } },
        })
        .then(({ data }) => {
          if (!cancelled && data !== undefined) {
            setRoot(data.data)
          }
        }),
    )
    return () => {
      cancelled = true
    }
  }, [api, accountId, rootId, root])

  const visibleEvents = thread.events.value.filter(
    (event) => !hideRedacted || !event.redacted,
  )
  const isVisibleEvent = (event: TimelineEvent): boolean =>
    !hideRedacted || !event.redacted

  return (
    <aside
      class="thread-panel"
      aria-label="Thread"
      {...(attachable ? dropHandlers : {})}
    >
      {dragging && (
        <div class="drop-overlay" aria-hidden="true">
          <p>Drop to attach</p>
        </div>
      )}
      <div class="overlay-head">
        <h2>Thread</h2>
        <button type="button" class="ghost" onClick={onClose}>
          Close
        </button>
      </div>

      <div class="thread-root">
        {root !== undefined ? (
          <>
            <UserAvatar
              accountId={accountId}
              userId={root.sender}
              displayName={members.displayName(root.sender)}
              member={members.members.value.get(root.sender)}
            />
            <span class="thread-root-copy">
              <span class="event-sender">
                {members.displayName(root.sender)}
              </span>{' '}
              <EventBody
                event={root}
                senderDisplay={members.displayName(root.sender)}
              />
            </span>
          </>
        ) : (
          <span class="muted">
            root <code>{rootId}</code>
          </span>
        )}
      </div>

      <ErrorBanner error={thread.error} />

      <div class="thread-timeline">
        {thread.loading.value ? (
          <p class="muted">Loading thread…</p>
        ) : (
          <>
            {!thread.atStart.value && (
              <button
                type="button"
                class="timeline-edge"
                disabled={thread.loadingOlder.value}
                onClick={() => void thread.loadOlder()}
              >
                {thread.loadingOlder.value ? 'Loading…' : 'Load older replies'}
              </button>
            )}
            <div class="timeline-list-shell">
              <ol class="event-list thread-list">
                {visibleEvents.map((event) => (
                  <MessageEventRow
                    key={event.event_id}
                    event={event}
                    timeline={thread}
                    members={members}
                    accountId={accountId}
                    ownUserId={ownUserId}
                    settings={settings}
                    reactionPickerOpen={
                      reactionPickerEventId === event.event_id
                    }
                    onSetReactionPicker={setReactionPickerEventId}
                    onReply={(event) => setAction({ kind: 'reply', event })}
                    onEdit={(event) => setAction({ kind: 'edit', event })}
                    showThreadAction={false}
                  />
                ))}
              </ol>
            </div>
          </>
        )}
      </div>

      <Composer
        placeholder="Reply in thread"
        autoFocus
        height={settings.messageComposerHeight.value}
        onHeightChange={(height) =>
          (settings.messageComposerHeight.value = height)
        }
        key={
          action?.kind === 'edit'
            ? action.event.event_id
            : `send:${accountId}:${roomId}:${rootId}`
        }
        banner={composerBanner}
        initialValue={
          action?.kind === 'edit'
            ? (action.draft ?? action.event.body ?? '')
            : ''
        }
        mentionCompletions={mentionCompletions}
        roomReferenceCompletions={roomReferenceCompletions}
        emojiCompletions={emojiCompletions}
        onCommand={
          onCommand === undefined
            ? undefined
            : (body) =>
                onCommand(
                  body,
                  thread,
                  visibleEvents,
                  isVisibleEvent,
                  action,
                  setAction,
                  setReactionPickerEventId,
                  formatComposerBody,
                )
        }
        roomCompletions={roomCompletions}
        onAttach={attachable ? stage : undefined}
        attachment={
          attachment === null
            ? undefined
            : { ...attachment, onRemove: clearAttachment }
        }
        // The store is thread-scoped, so `thread_root` is already implied.
        onSubmit={submitMessage}
      />
    </aside>
  )
}
