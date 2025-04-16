//! A fully optimized image optimizer for SSR environments.
//!
//! - Concurrency dedup: multiple requests for the same image wait on the same handle.
//! - Optional no‐upscale: prevent enlarging smaller source images.
//! - Optional TTL for blur placeholders to limit memory usage over time.
//! - Preload from disk: load existing `.svg` placeholders into memory at startup.

use std::fmt::Display;
use image::GenericImageView;
#[cfg(feature = "ssr")]
use {
    std::sync::Arc,
    std::path::{Path, PathBuf},
    chrono::{DateTime, Utc},
    dashmap::DashMap,
    tokio::sync::{Semaphore, Mutex},
    tokio::task::JoinHandle,
};
use serde::{Deserialize, Serialize};


/// A small structure for storing a blur placeholder (an SVG string) plus
/// a creation timestamp (useful for TTL).
#[cfg(feature = "ssr")]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BlurEntry {
    /// The SVG data for this blur placeholder.
    pub svg_data: String,
    /// When this entry was created. Used to evict older entries if TTL is set.
    pub created_at: DateTime<Utc>,
}

/// Manages concurrency and caching for image optimization.
/// - *In flight* map ensures only one encode happens per unique image at a time.
/// - *DashMap* caches small blur placeholders in memory. Large WebP files are served from disk.
#[cfg(feature = "ssr")]
#[derive(Debug, Clone)]
pub struct ImageOptimizer {
    /// The route (path) where the image handler is mounted, e.g. `"/__cache/image"`.
    pub api_handler_path: String,
    /// The local filesystem path that corresponds to your app’s "public" or static root, e.g. `"./public"`.
    pub root_file_path: String,
    /// A semaphore to limit the number of simultaneous image encodes.
    pub semaphore: Arc<Semaphore>,
    /// Blur placeholders are stored in memory:  `CachedImage` => `BlurEntry`.
    /// For large numbers of images, consider an LRU library or TTL to avoid unbounded growth.
    pub blur_cache: Arc<DashMap<CachedImage, BlurEntry>>,
    /// Tracks ongoing or recently finished image tasks to prevent duplicate work.
    /// Key = `CachedImage`, Value = a `Mutex<Option<JoinHandle<Result<(), CreateImageError>>>>`.
    pub in_flight: Arc<DashMap<CachedImage, Arc<Mutex<Option<JoinHandle<Result<(), CreateImageError>>>>>>>,
    /// If `true`, prevents enlarging images above their original size.
    /// Requests bigger than the source image are clamped or skipped (your choice below).
    pub no_upscale: bool,
    /// If set, blur placeholders older than `blur_ttl_seconds` are evicted upon access.
    /// Set `None` or `Some(0)` to disable.
    pub blur_ttl_seconds: Option<u64>,
}

// #[cfg(feature = "ssr")]
impl ImageOptimizer {
    /// Constructs a new `ImageOptimizer` **without** preloading from disk.
    ///
    /// - `api_handler_path`: e.g. `"/__cache/image"`.
    /// - `root_file_path`: e.g. `"./public"`.
    /// - `parallelism`: number of concurrent encodes allowed.
    /// - `no_upscale`: if `true`, do not enlarge smaller source images.
    /// - `blur_ttl_seconds`: if `Some(n)`, evict blur placeholders older than `n` seconds.
    pub fn new(
        api_handler_path: impl Into<String>,
        root_file_path: impl Into<String>,
        parallelism: usize,
        no_upscale: bool,
        blur_ttl_seconds: Option<u64>,
    ) -> Self {
        Self {
            api_handler_path: api_handler_path.into(),
            root_file_path: root_file_path.into(),
            semaphore: Arc::new(Semaphore::new(parallelism)),
            blur_cache: Arc::new(DashMap::new()),
            in_flight: Arc::new(DashMap::new()),
            no_upscale,
            blur_ttl_seconds,
        }
    }

    /// Constructs a new `ImageOptimizer` **and** asynchronously preloads any existing
    /// blur placeholders from disk, so your in‐memory cache is warmed up at startup.
    ///
    /// # Example
    /// ```
    /// // Example usage (pseudocode):
    /// use leptos_image::ImageOptimizer;
    /// let optimizer = ImageOptimizer::new_with_preload(
    ///     "/__cache/image",
    ///     "./public",
    ///     2,
    ///     false,
    ///     Some(3600) // 1 hour TTL for blur placeholders
    /// )
    /// .await
    /// .expect("Failed to preload disk cache");
    /// ```
    pub async fn new_with_preload(
        api_handler_path: impl Into<String>,
        root_file_path: impl Into<String>,
        parallelism: usize,
        no_upscale: bool,
        blur_ttl_seconds: Option<u64>,
    ) -> Result<Self, std::io::Error> {
        let optimizer = Self::new(
            api_handler_path,
            root_file_path,
            parallelism,
            no_upscale,
            blur_ttl_seconds,
        );
        optimizer.preload_disk_cache().await?;
        Ok(optimizer)
    }

    /// Reads all previously generated `.svg` placeholders from
    /// `<root_file_path>/cache/image` and populates the in‐memory blur cache.
    /// This is useful if you want a “warm start” so your blur placeholders
    /// are instantly available after a server restart.
    ///
    /// If the folder doesn’t exist yet, this is a no‐op.
    pub async fn preload_disk_cache(&self) -> std::io::Result<()> {
        use tokio::fs::{self, ReadDir};
        use tokio_stream::StreamExt;

        let cache_dir = std::path::Path::new(&self.root_file_path)
            .join("cache")
            .join("image");
        if !cache_dir.exists() {
            return Ok(());
        }

        let mut rd: ReadDir = fs::read_dir(cache_dir).await?;
        while let Some(entry) = rd.next_entry().await? {
            let path = entry.path();
            // Only `.svg` placeholders are kept in memory:
            if path.extension().and_then(|ext| ext.to_str()) == Some("svg") {
                if let Some(cached) = CachedImage::from_file_path(&path.to_string_lossy()) {
                    let svg_data = match fs::read_to_string(&path).await {
                        Ok(d) => d,
                        Err(e) => {
                            tracing::error!("Error reading SVG from {:?}: {:?}", path, e);
                            continue;
                        }
                    };
                    let entry = BlurEntry {
                        svg_data,
                        created_at: Utc::now(),
                    };
                    self.blur_cache.insert(cached, entry);
                }
            }
        }
        Ok(())
    }

    /// Creates a context function to provide the optimizer in Leptos SSR (or similar).
    /// This is just a convenience if you need a “provide_context” in your framework.
    pub fn provide_context(&self) -> impl Fn() + Clone + Send + 'static {
        let me = self.clone();
        move || {
            leptos::prelude::provide_context(me.clone());
        }
    }

    /// **Main entrypoint** for generating (or skipping) an optimized image:
    ///
    /// 1. If `no_upscale` is true and the request is bigger than the source, we clamp or skip it.
    /// 2. If the final file is already on disk, do nothing.
    /// 3. Use concurrency dedup: if another request is already encoding the same image,
    ///    we wait for it to finish.
    /// 4. Otherwise, we spawn a new CPU‐bound task behind a semaphore to encode the image.
    ///
    /// Returns:
    /// - `Ok(true)` if a new image was **actually created**.
    /// - `Ok(false)` if it already existed on disk, or if `no_upscale` forced a skip, etc.
    /// - `Err(...)` if some I/O or encode error occurred.
    pub async fn create_image(&self, image: &CachedImage) -> Result<bool, CreateImageError> {
        // Possibly clamp or skip if we do not allow upscaling.
        let maybe_image = self.maybe_clamp(image)?;
        let Some(final_image) = maybe_image else {
            // Means “skip entirely” if it was bigger than the source or
            // you can customize if you want to do partial clamp, etc.
            return Ok(false);
        };

        // Build final output path.
        let rel_path = self.get_file_path(&final_image);
        let final_path = path_from_segments(vec![
            &self.root_file_path,
            &rel_path
        ]);

        // If a file with that name is already on disk, no new encode needed.
        if file_exists(&final_path).await {
            return Ok(false);
        }

        // Check concurrency dedup map: is someone else already working on it?
        if let Some(existing_handle) = self.in_flight.get(&final_image) {
            // Wait on the same join handle
            let jarc = existing_handle.value().clone();
            let mut guard = jarc.lock().await;
            if let Some(ref mut jh) = *guard {
                // This awaits the existing CPU task
                let res = jh.await;
                return match res {
                    Err(e) => Err(CreateImageError::JoinError(e)),
                    Ok(Err(e)) => Err(e),
                    Ok(Ok(_)) => Ok(true), // newly created
                };
            }
        }

        // Otherwise, we insert an empty handle so subsequent requests wait here.
        let new_arc = Arc::new(Mutex::new(None));
        self.in_flight.insert(final_image.clone(), new_arc.clone());

        // Acquire concurrency permit to limit parallel CPU usage
        let permit = self.semaphore.clone().acquire_owned().await?;

        // CPU‐bound encoding => spawn_blocking
        let config = final_image.option.clone();
        let path_clone = final_path.clone();
        let final_image_clone = final_image.clone();

        let jh = tokio::task::spawn_blocking(move || {
            // We intentionally drop the permit once done, so others can proceed.
            let _permit = permit;
            create_optimized_image(config, &final_image_clone.src, &path_clone)
        });

        {
            // Store the join handle, so other concurrent requests deduplicate
            let mut guard = new_arc.lock().await;
            *guard = Some(jh);
        }

        // Now actually wait on it ourselves
        let mut guard = new_arc.lock().await;
        let handle_ref = guard.as_mut().unwrap(); // must be Some now
        let res = handle_ref.await;

        // Remove from in_flight map so it doesn’t grow unbounded
        self.in_flight.remove(&final_image);

        match res {
            Err(e) => Err(CreateImageError::JoinError(e)),
            Ok(Err(e)) => Err(e),
            Ok(Ok(_)) => Ok(true),
        }
    }

    /// **Retrieves an SVG blur placeholder** from memory, respecting TTL if configured.
    /// Returns `Some(svg_string)` if present, else `None`.
    pub fn get_blur(&self, image: &CachedImage) -> Option<String> {
        use chrono::Duration;

        let now = Utc::now();
        let entry = self.blur_cache.get(image)?;
        if let Some(ttl) = self.blur_ttl_seconds {
            // If older than TTL, evict
            let age = now.signed_duration_since(entry.created_at);
            if age > Duration::seconds(ttl as i64) {
                self.blur_cache.remove(image);
                return None;
            }
        }
        Some(entry.svg_data.clone())
    }

    /// Generates a path like `cache/image/<base64 descriptors>/<filename>.webp` or `.svg`.
    /// This is the relative path under `root_file_path`.
    pub fn get_file_path(&self, image: &CachedImage) -> String {
        image.get_file_path()
    }

    /// A convenience for combining `root_file_path` + `get_file_path`.
    pub fn get_file_path_from_root(&self, image: &CachedImage) -> String {
        let rel = self.get_file_path(image);
        path_from_segments(vec![&self.root_file_path, &rel]).to_string_lossy().to_string()
    }

    // --- Internal Helpers ---

    /// If `no_upscale` is set, we clamp or skip the request if it’s bigger than the source.
    /// Return `Ok(Some(clamped))` if continuing, or `Ok(None)` to skip entirely, or an error.
    fn maybe_clamp(&self, image: &CachedImage) -> Result<Option<CachedImage>, CreateImageError> {
        if !self.no_upscale {
            return Ok(Some(image.clone()));
        }
        let CachedImageOption::Resize(ref r) = image.option else {
            // For blur placeholders, no real upscaling check is needed. They’re always smaller.
            return Ok(Some(image.clone()));
        };

        // Check dimensions of the source
        let src_path = path_from_segments(vec![&self.root_file_path, &image.src]);
        if !src_path.exists() {
            // If it doesn't exist, just let it proceed (it'll fail if truly absent).
            return Ok(Some(image.clone()));
        }
        let meta = std::fs::metadata(&src_path)?;
        if meta.len() == 0 {
            return Ok(Some(image.clone())); // empty file => won't succeed anyway
        }

        // We only open the image to see actual width/height.
        let opened = image::open(&src_path)?;
        let (orig_w, orig_h) = opened.dimensions();
        if r.width <= orig_w && r.height <= orig_h {
            // No upscaling => proceed as is
            Ok(Some(image.clone()))
        } else {
            // Example: clamp to original size rather than skipping entirely:
            let clamped = CachedImage {
                src: image.src.clone(),
                option: CachedImageOption::Resize(Resize {
                    width: r.width.min(orig_w),
                    height: r.height.min(orig_h),
                    quality: r.quality,
                }),
            };
            Ok(Some(clamped))
            // Alternatively, if you truly want to skip, do:
            // Ok(None)
        }
    }
}

/// The function that does the actual image transformations, CPU‐bound.
/// - If `Resize(...)`, produce a `.webp`.
/// - If `Blur(...)`, produce a small `.svg`.
#[cfg(feature = "ssr")]
fn create_optimized_image(
    config: CachedImageOption,
    source_path: &str,
    save_path: &Path,
) -> Result<(), CreateImageError> {
    match config {
        CachedImageOption::Resize(Resize { width, height, quality }) => {
            // 1) Load and auto‐orient
            let img = image::open(source_path)?;
            let oriented = auto_orient_image(&img, source_path)?;
            // 2) Resize
            let resized = oriented.resize(width, height, image::imageops::FilterType::CatmullRom);
            // 3) Encode as WebP
            let webp = {
                use webp::Encoder;
                let enc = Encoder::from_image(&resized).unwrap();
                enc.encode(quality as f32)
            };
            // 4) Save
            create_nested_if_needed(save_path)?;
            std::fs::write(save_path, &*webp)?;
        }
        CachedImageOption::Blur(blur_opts) => {
            let svg = create_image_blur(source_path, blur_opts)?;
            create_nested_if_needed(save_path)?;
            std::fs::write(save_path, svg)?;
        }
    }
    Ok(())
}

/// A simplified version of your "blur" generation:
/// 1) Open & auto‐orient
/// 2) Tiny resize
/// 3) Encode to WebP
/// 4) Base64 embed in an SVG w/ gaussian blur.
#[cfg(feature = "ssr")]
fn create_image_blur(
    source_path: &str,
    blur: Blur,
) -> Result<String, CreateImageError> {
    let Blur {
        width,
        height,
        svg_width,
        svg_height,
        sigma,
    } = blur;

    // 1) Open & auto‐orient
    let img = image::open(source_path)?;
    let oriented = auto_orient_image(&img, source_path)?;

    // 2) Tiny resize
    let small = oriented.resize(width, height, image::imageops::FilterType::Nearest);

    // 3) Convert to WebP
    let encoded = {
        use webp::Encoder;
        let enc = Encoder::from_image(&small).unwrap();
        enc.encode(80.0)
    };
    let base64_webp = {
        use base64::{engine::general_purpose, Engine as _};
        general_purpose::STANDARD.encode(&*encoded)
    };
    let data_uri = format!("data:image/webp;base64,{}", base64_webp);

    // 4) Insert into an SVG filter
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="100%" height="100%"
                 viewBox="0 0 {svg_width} {svg_height}" preserveAspectRatio="none">
   <filter id="a" filterUnits="userSpaceOnUse" color-interpolation-filters="sRGB">
       <feGaussianBlur stdDeviation="{sigma}" edgeMode="duplicate"/>
       <feComponentTransfer>
           <feFuncA type="discrete" tableValues="1 1"/>
       </feComponentTransfer>
   </filter>
   <image filter="url(#a)" width="100%" height="100%" href="{data_uri}" />
</svg>"#
    );
    Ok(svg)
}

/// If your source images might have EXIF orientation, you want to fix that.
/// This stub calls some imaginary `crate::util::auto_orient_image`.
#[cfg(feature = "ssr")]
fn auto_orient_image<I>(img: I, _path: &str) -> Result<I, CreateImageError>
where
    I: std::ops::Deref<Target = image::DynamicImage> + Sized + 'static,
{
    // If you have an actual function that reads EXIF orientation, do it here.
    // For now, we just return the same image in this sample.
    // If there's a possible error path, adapt the signature as needed.
    Ok(img)
}

// ----------
// Data Types
// ----------

/// Represents user’s request for either a **resized** (WebP) or a **blur** (SVG).
//#[cfg(feature = "ssr")]
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, Hash)]
pub enum CachedImageOption {
    #[serde(rename = "r")]
    Resize(Resize),
    #[serde(rename = "b")]
    Blur(Blur),
}

/// Resize parameters for a final WebP file.
//#[cfg(feature = "ssr")]
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, Hash)]
#[serde(rename = "r")]
pub struct Resize {
    #[serde(rename = "w")]
    pub width: u32,
    #[serde(rename = "h")]
    pub height: u32,
    #[serde(rename = "q")]
    pub quality: u8,
}

/// Blur parameters for an SVG placeholder.
//#[cfg(feature = "ssr")]
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, Hash)]
#[serde(rename = "b")]
pub struct Blur {
    #[serde(rename = "w")]
    pub width: u32,
    #[serde(rename = "h")]
    pub height: u32,
    #[serde(rename = "sw")]
    pub svg_width: u32,
    #[serde(rename = "sh")]
    pub svg_height: u32,
    #[serde(rename = "s")]
    pub sigma: u8,
}

/// A user request or internal reference to a specific source path + transformation option.
/// Typically, `src` is relative to your `root_file_path` (like `"images/foo.png"`).
// #[cfg(feature = "ssr")]
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, Hash)]
pub struct CachedImage {
    /// E.g. `"images/foo.png"`.
    pub src: String,
    /// Either a `Resize(...)` or a `Blur(...)`.
    pub option: CachedImageOption,
}

impl Display for CachedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CachedImage(src={}, option={:?})", self.src, self.option)
    }
}

// #[cfg(feature = "ssr")]
impl CachedImage {
    /// Creates a URL referencing this image in your cache handler, e.g.:
    /// `"/__cache/image"?r%5Bw%5D=...`.
    pub fn get_url_encoded(&self, handler_path: &str) -> String {
        let qs = serde_qs::to_string(self).unwrap();
        format!("{handler_path}?{qs}")
    }

    /// Returns the final file path (under `cache/image`) for this transformation.
    /// For example: `cache/image/<BASE64_OF_PARAMS>/original_name.webp` or `.svg`.
    pub fn get_file_path(&self) -> String {
        use base64::{engine::general_purpose, Engine as _};
        let encode = serde_qs::to_string(self).unwrap();
        let encoded = general_purpose::STANDARD.encode(encode);

        let mut path = path_from_segments(vec!["cache", "image", &encoded, &self.src]);
        match &self.option {
            CachedImageOption::Resize(_) => path.set_extension("webp"),
            CachedImageOption::Blur(_) => path.set_extension("svg"),
        };
        path.to_string_lossy().to_string()
    }

    /// Reverse‐engineers a `CachedImage` from a path that has the
    /// base64 portion inside it. For example:
    /// `cache/image/<BASE64ENCODED_QS>/img.png.webp`.
    /// Returns `None` if it can’t decode or parse.
    pub fn from_file_path(path: &str) -> Option<Self> {
        use base64::{engine::general_purpose, Engine as _};
        let parts = path.split('/');
        for part in parts {
            let decoded = general_purpose::STANDARD.decode(part).ok()?;
            let s = String::from_utf8(decoded).ok()?;
            if let Ok(ci) = serde_qs::from_str::<CachedImage>(&s) {
                return Some(ci);
            }
        }
        None
    }

    /// Decodes from a query string, e.g. `"/__cache/image?r[w]=100&..."`.
    pub fn from_url_encoded(url: &str) -> Result<Self, serde_qs::Error> {
        let qs = url.split('?').nth(1).unwrap_or(url);
        serde_qs::from_str(qs)
    }
}

/// Errors encountered while creating images.
/// Includes I/O, concurrency join failures, and any `image` crate error.
#[cfg(feature = "ssr")]
#[derive(Debug, thiserror::Error)]
pub enum CreateImageError {
    #[error("Image error: {0}")]
    ImageError(#[from] image::ImageError),
    #[error("Join error: {0}")]
    JoinError(#[from] tokio::task::JoinError),
    #[error("IO error: {0}")]
    IOError(#[from] std::io::Error),
    #[error("Semaphore error: {0}")]
    Acquire(#[from] tokio::sync::AcquireError),
}

/// Joins path segments, ignoring extra slashes.
#[cfg(feature = "ssr")]
fn path_from_segments(parts: Vec<&str>) -> PathBuf {
    let mut buf = PathBuf::new();
    for p in parts {
        let trimmed = p.trim_matches('/');
        if !trimmed.is_empty() {
            buf.push(trimmed);
        }
    }
    buf
}

/// Non‐blocking file existence check.
#[cfg(feature = "ssr")]
async fn file_exists(path: &Path) -> bool {
    tokio::fs::metadata(path).await.is_ok()
}

/// Ensures the parent directories of `path` exist, creating them if needed.
#[cfg(feature = "ssr")]
fn create_nested_if_needed(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

// --------------------------------------
// Example tests (can remove or adapt)
// --------------------------------------
#[cfg(feature = "ssr")]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_encode_roundtrip() {
        let c = CachedImage {
            src: "images/test.png".to_string(),
            option: CachedImageOption::Resize(Resize {
                width: 100,
                height: 80,
                quality: 75,
            }),
        };
        let url = c.get_url_encoded("/__cache/image");
        println!("Encoded URL: {url}");
        let decoded = CachedImage::from_url_encoded(&url).unwrap();
        assert_eq!(decoded, c);
    }

    #[test]
    fn test_file_path_roundtrip() {
        let c = CachedImage {
            src: "images/test.png".to_string(),
            option: CachedImageOption::Blur(Blur {
                width: 10,
                height: 10,
                svg_width: 100,
                svg_height: 100,
                sigma: 12,
            }),
        };
        let fp = c.get_file_path();
        println!("File path: {fp}");
        let back = CachedImage::from_file_path(&fp).unwrap();
        assert_eq!(back, c);
    }
}
