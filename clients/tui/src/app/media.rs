//! Bounded background work for Matrix image events.
//!
//! Downloads, decoding, EXIF correction, resizing, and terminal-protocol
//! encoding all happen outside the TUI event/render loop.

use std::sync::Arc;
use std::{io::Cursor, num::NonZeroU64};

use image::{ImageReader, Limits};
use ratatui::layout::Size;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;
use ratatui_image::Resize;
use tokio::sync::{mpsc, Semaphore};
use uuid::Uuid;

use crate::api::AxonClient;

pub(crate) const IMAGE_CACHE_LIMIT: usize = 16;
pub(crate) const PROTOCOL_CACHE_LIMIT: usize = 32;
pub(crate) const MEDIA_WORKERS: usize = 4;
const MAX_DECODED_PIXELS: u64 = 40_000_000;
const MAX_DECODE_ALLOC_BYTES: u64 = 200 * 1024 * 1024;
const MAX_CACHED_IMAGE_DIMENSION: u32 = 1600;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct MediaKey {
    pub(crate) account_id: Uuid,
    pub(crate) mxc_url: String,
}

impl MediaKey {
    pub(crate) fn new(account_id: Uuid, mxc_url: String) -> Self {
        Self {
            account_id,
            mxc_url,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ProtocolKey {
    pub(crate) media: MediaKey,
    pub(crate) size: Size,
}

pub(crate) enum ImageState {
    Fetching,
    Ready(Arc<image::DynamicImage>),
    Failed(String),
}

pub(crate) enum ProtocolState {
    Encoding,
    Ready(Protocol),
    Failed(String),
}

pub(crate) enum MediaResult {
    Image {
        key: MediaKey,
        outcome: Result<Arc<image::DynamicImage>, String>,
    },
    Protocol {
        key: ProtocolKey,
        outcome: Result<Protocol, String>,
    },
}

pub(crate) fn spawn_image_fetch(
    client: AxonClient,
    key: MediaKey,
    is_encrypted: bool,
    workers: Arc<Semaphore>,
    tx: mpsc::Sender<MediaResult>,
) {
    tokio::spawn(async move {
        let Ok(_permit) = workers.acquire_owned().await else {
            return;
        };
        let outcome = match client.get_media(key.account_id, &key.mxc_url).await {
            Ok(bytes) => {
                let url = key.mxc_url.clone();
                tokio::task::spawn_blocking(move || decode_image(bytes, is_encrypted, &url))
                    .await
                    .unwrap_or_else(|err| Err(format!("image decode task failed: {err}")))
            }
            Err(err) => Err(err.to_string()),
        };
        let _ = tx.send(MediaResult::Image { key, outcome }).await;
    });
}

pub(crate) fn spawn_protocol_encode(
    picker: Picker,
    image: Arc<image::DynamicImage>,
    key: ProtocolKey,
    workers: Arc<Semaphore>,
    tx: mpsc::Sender<MediaResult>,
) {
    tokio::spawn(async move {
        let Ok(_permit) = workers.acquire_owned().await else {
            return;
        };
        let encode_key = key.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            picker
                .new_protocol((*image).clone(), encode_key.size, Resize::Fit(None))
                .map_err(|err| err.to_string())
        })
        .await
        .unwrap_or_else(|err| Err(format!("image encode task failed: {err}")));
        let _ = tx.send(MediaResult::Protocol { key, outcome }).await;
    });
}

fn decode_image(
    bytes: Vec<u8>,
    is_encrypted: bool,
    mxc_url: &str,
) -> Result<Arc<image::DynamicImage>, String> {
    match decode_image_with_limits(&bytes) {
        Ok(image) => Ok(Arc::new(image)),
        Err(err) => {
            let format = super::sniff_format(&bytes);
            if is_encrypted && format.starts_with("unknown") {
                Err(format!(
                    "encrypted media - server could not decrypt ({mxc_url})"
                ))
            } else {
                Err(format!("{err} ({format}) - {mxc_url}"))
            }
        }
    }
}

fn decode_image_with_limits(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|err| err.to_string())?;
    let format = reader
        .format()
        .ok_or_else(|| "unsupported image format".to_owned())?;
    let (width, height) = reader.into_dimensions().map_err(|err| err.to_string())?;
    validate_image_dimensions(width, height)?;

    let mut limits = Limits::default();
    limits.max_image_width = Some(width);
    limits.max_image_height = Some(height);
    limits.max_alloc = Some(MAX_DECODE_ALLOC_BYTES);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    let image = reader.decode().map_err(|err| err.to_string())?;
    let image = super::apply_exif_orientation(image, bytes);
    // thumbnail() upscales small images; only downscale when the image exceeds the cap.
    Ok(
        if image.width() > MAX_CACHED_IMAGE_DIMENSION || image.height() > MAX_CACHED_IMAGE_DIMENSION
        {
            image.thumbnail(MAX_CACHED_IMAGE_DIMENSION, MAX_CACHED_IMAGE_DIMENSION)
        } else {
            image
        },
    )
}

fn validate_image_dimensions(width: u32, height: u32) -> Result<(), String> {
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if NonZeroU64::new(pixels).is_none() || pixels > MAX_DECODED_PIXELS {
        return Err(format!(
            "image dimensions {width}x{height} exceed the decode limit"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_images_with_excessive_decoded_dimensions() {
        assert!(validate_image_dimensions(6000, 6000).is_ok()); // 36 MP — under new limit
        assert!(validate_image_dimensions(7000, 6000).is_err()); // 42 MP — over limit
        assert!(validate_image_dimensions(0, 100).is_err());
    }
}
