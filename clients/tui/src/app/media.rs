//! Async media fetching for image events.
//!
//! [`spawn_image_fetch`] kicks off a background tokio task that downloads an
//! MXC URL via the Axon media proxy and sends the raw bytes back on a channel.
//! The main event loop decodes the bytes and stores the resulting image in the
//! cache; the render pass encodes it on demand using the terminal's protocol.

use tokio::sync::mpsc;
use uuid::Uuid;

use crate::api::AxonClient;

/// The result of one background image download: the MXC URL (cache key),
/// whether the source event used encrypted media (`content.file`), and either
/// the raw bytes or an error string.
pub(crate) type ImageFetchResult = (String, bool, Result<Vec<u8>, String>);

/// State of a single cached image.
pub(crate) enum ImageState {
    /// Download is in flight.
    Fetching,
    /// Bytes arrived and were decoded into a [`image::DynamicImage`].
    Ready(image::DynamicImage),
    /// The download or decode failed.
    Failed(String),
}

/// Spawn a tokio task that downloads `mxc_url` and sends the result on `tx`.
/// `is_encrypted` indicates the URL came from `content.file` (encrypted event);
/// it is forwarded unchanged so the result handler can give a better error when
/// the server returns raw ciphertext instead of a decoded image.
pub(crate) fn spawn_image_fetch(
    client: AxonClient,
    account_id: Uuid,
    mxc_url: String,
    is_encrypted: bool,
    tx: mpsc::UnboundedSender<ImageFetchResult>,
) {
    tokio::spawn(async move {
        let result = client
            .get_media(account_id, &mxc_url)
            .await
            .map_err(|e| e.to_string());
        let _ = tx.send((mxc_url, is_encrypted, result));
    });
}
