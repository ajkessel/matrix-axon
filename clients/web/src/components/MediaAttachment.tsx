import { useState } from 'preact/hooks'
import { humanSize } from '../media/human-size'
import type { ParsedMedia } from '../media/parse-media'
import { useServices } from '../services'

const ICON: Record<ParsedMedia['kind'], string> = {
  file: '📄',
  audio: '🎵',
  video: '🎞️',
  image: '🖼️',
  sticker: '🖼️',
}

/**
 * A non-image attachment (file / audio / video) as a download card (ADR 0064).
 * No native `<video>`/`<audio>` element — those buffer the whole object into
 * memory — and no bytes are fetched until the user clicks Download. The
 * transient anchor is the sanctioned download path (`window.open` is forbidden
 * repo-wide for the Tauri shell, M-W12).
 */
export function MediaAttachment({
  accountId,
  media,
}: {
  accountId: string
  media: ParsedMedia
}) {
  const { media: service } = useServices()
  const [downloading, setDownloading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const size = humanSize(media.size)
  const downloadable = media.url !== null && media.url.startsWith('mxc://')

  const download = async () => {
    if (media.url === null) {
      return
    }
    setDownloading(true)
    setError(null)
    const result = await service.fetchBlobUrl(accountId, media.url)
    setDownloading(false)
    if (!result.ok) {
      setError('Download failed')
      return
    }
    const anchor = document.createElement('a')
    anchor.href = result.url
    anchor.download = media.filename
    document.body.appendChild(anchor)
    anchor.click()
    anchor.remove()
    // Give the browser time to start the download before releasing the blob.
    setTimeout(() => URL.revokeObjectURL(result.url), 60_000)
  }

  return (
    <div class="media-attachment">
      <span class="media-attachment-icon" aria-hidden="true">
        {ICON[media.kind]}
      </span>
      <span class="media-attachment-meta">
        <span class="media-attachment-name">{media.filename}</span>
        {size !== null && <span class="muted">{size}</span>}
        {error !== null && <span class="local-echo-status error">{error}</span>}
      </span>
      {downloadable && (
        <button
          type="button"
          class="ghost media-attachment-download"
          disabled={downloading}
          onClick={() => void download()}
        >
          {downloading ? 'Downloading…' : 'Download'}
        </button>
      )}
    </div>
  )
}
