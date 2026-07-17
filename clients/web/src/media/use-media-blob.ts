import type { RefObject } from 'preact'
import { useEffect, useRef, useState } from 'preact/hooks'
import { perfMark } from '../perf'
import { useServices } from '../services'
import { observeVisible } from './intersection'
import type {
  MediaFailure,
  MediaHandle,
  ThumbnailRequest,
} from './media-service'

export interface MediaBlobState {
  status: 'idle' | 'loading' | 'ready' | 'error'
  url?: string
  error?: MediaFailure
}

/**
 * Lazily resolve an `mxc://` URI to an object URL for one mounted element
 * (ADR 0064). Attach the returned `ref` to the element that reserves the
 * image's space; the fetch starts when that element nears the viewport, and
 * the acquired handle is released on unmount so the blob can be revoked.
 *
 * jsdom has no `IntersectionObserver`, so there the fetch starts eagerly on
 * mount — component tests exercise the real load path without a stub.
 */
export function useMediaBlob<T extends HTMLElement = HTMLElement>(
  accountId: string,
  mxcUrl: string | null,
  options: { eager?: boolean; thumbnail?: ThumbnailRequest } = {},
): { ref: RefObject<T>; state: MediaBlobState } {
  const { media } = useServices()
  const ref = useRef<T>(null)
  const [state, setState] = useState<MediaBlobState>({ status: 'idle' })
  const { eager = false, thumbnail } = options
  const thumbnailWidth = thumbnail?.width
  const thumbnailHeight = thumbnail?.height
  const thumbnailMethod = thumbnail?.method

  useEffect(() => {
    if (mxcUrl === null) {
      setState({ status: 'idle' })
      return
    }

    let cancelled = false
    let started = false
    let handle: MediaHandle | null = null

    const start = () => {
      if (started) {
        return
      }
      started = true
      perfMark('media-blob:start', {
        accountId,
        mxcUrl,
        thumbnail:
          thumbnailWidth !== undefined && thumbnailHeight !== undefined,
        eager,
      })
      setState({ status: 'loading' })
      void media
        .acquire(
          accountId,
          mxcUrl,
          thumbnailWidth !== undefined && thumbnailHeight !== undefined
            ? {
                thumbnail: {
                  width: thumbnailWidth,
                  height: thumbnailHeight,
                  method: thumbnailMethod,
                },
              }
            : undefined,
        )
        .then((acquired) => {
          if (cancelled) {
            perfMark('media-blob:cancelled', {
              accountId,
              mxcUrl,
            })
            acquired.release()
            return
          }
          handle = acquired
          perfMark(
            acquired.result.ok ? 'media-blob:ready' : 'media-blob:error',
            {
              accountId,
              mxcUrl,
              thumbnail:
                thumbnailWidth !== undefined && thumbnailHeight !== undefined,
            },
          )
          setState(
            acquired.result.ok
              ? { status: 'ready', url: acquired.result.url }
              : { status: 'error', error: acquired.result.error },
          )
        })
    }

    let stopObserving: (() => void) | null = null
    if (
      eager ||
      typeof IntersectionObserver === 'undefined' ||
      ref.current === null
    ) {
      start()
    } else {
      stopObserving = observeVisible(ref.current, start)
    }

    return () => {
      cancelled = true
      stopObserving?.()
      handle?.release()
    }
  }, [
    media,
    accountId,
    mxcUrl,
    eager,
    thumbnailWidth,
    thumbnailHeight,
    thumbnailMethod,
  ])

  return { ref, state }
}
