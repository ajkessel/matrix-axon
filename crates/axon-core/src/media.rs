//! Shared, SDK-free media-thumbnail types.
//!
//! [`ThumbnailMethod`] and [`ThumbnailSpec`] mirror
//! `ruma::api::client::media::get_content_thumbnail::v3::Method` and the
//! dimensions of a homeserver thumbnail request, without either `axon-api`
//! (the thumbnail port) or `axon-media` (the cache-key input) depending on
//! `matrix-sdk`/`ruma`. The conversion to/from the SDK's own `Method` type
//! happens only at the `axon-sync` fetch boundary.

/// The homeserver-side thumbnail resizing method (Matrix spec: `crop` or
/// `scale`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThumbnailMethod {
    /// Crop the original to exactly the requested dimensions.
    Crop,
    /// Scale the original to fit within the requested dimensions, preserving
    /// aspect ratio. The Matrix spec default.
    Scale,
}

impl ThumbnailMethod {
    /// Stable lowercase representation (`"crop"` / `"scale"`), matching the
    /// Matrix spec's `method` query parameter. Used as a cache-key component
    /// by `axon-media`.
    pub fn as_str(self) -> &'static str {
        match self {
            ThumbnailMethod::Crop => "crop",
            ThumbnailMethod::Scale => "scale",
        }
    }
}

impl std::fmt::Display for ThumbnailMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The dimensions and resizing method a thumbnail request needs, bundled so
/// it can be threaded through the port/cache/fetcher layers as one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ThumbnailSpec {
    /// Desired width in pixels.
    pub width: u32,
    /// Desired height in pixels.
    pub height: u32,
    /// Resizing method.
    pub method: ThumbnailMethod,
}
