//! # Flexible EXIF Auto-Orientation
//!
//! This module auto-rotates/flips images based on EXIF orientation metadata,
//! and it also accommodates certain known brand quirks (like Canon sometimes
//! reversing orientation codes 6 ↔ 8).
//!
//! ## How it Works
//! 1. **Parse EXIF** from the file with [`rexif`].
//! 2. Collect the camera brand (`ExifTag::Make`) and the last orientation code (tag `0x0112`) found.
//! 3. Pass them to [`brand_invert_orientation`] to handle brand-specific quirks.
//! 4. Apply the **standard EXIF** rotation/flip in [`fix_orientation_standard`].
//! 5. Return the physically upright [`image::DynamicImage`].
//!
//! ## Adjusting for Upside-Down Images
//! - If you find that certain images are still upside down or reversed, you can remove or modify
//!   the brand-based quirk logic in [`brand_invert_orientation`].
//! - By default, we have a Canon quirk that swaps orientation codes **6 ↔ 8**.
//! - You can add your own brand checks or remove them if they cause more issues.
//!
//! ## Example
//! ```no_run
//! # use image::open;
//! # use std::path::Path;
//! # #[cfg(feature = "ssr")]
//! # fn example() -> Result<(), crate::optimizer::CreateImageError> {
//!     let path = Path::new("photo.jpg");
//!     let img = open(path)?; // read image from file
//!
//!     // Auto-orient
//!     let upright = crate::auto_orient_image::auto_orient_image(img, &path)?;
//!
//!     // Save the corrected image
//!     upright.save("photo_upright.jpg")?;
//!     Ok(())
//! # }
//! ```

#![cfg(feature = "ssr")] // Only compile on server side

use image::imageops::{flip_horizontal, flip_vertical, rotate180, rotate90, rotate270};
use image::DynamicImage;
use std::ffi::OsStr;
use std::path::Path;

/// Auto-orient an image by reading EXIF orientation from a file path.
///
/// - Detects the **camera make** (`ExifTag::Make`).
/// - Finds the **last** orientation tag (0x0112) in the file if multiple appear.
/// - Applies brand-based orientation quirks in [`brand_invert_orientation`].
/// - Finally, applies the standard EXIF transform with [`fix_orientation_standard`].
///
/// # Generic Usage
/// This function is generic over `P: AsRef<Path> + AsRef<OsStr>`, so you can pass in many path-like
/// types (e.g., `&PathBuf`, `&str`, etc.).
///
/// # Brand Quirk
/// By default, it swaps orientation **6 ↔ 8** for Canon. If that inverts incorrectly,
/// comment out or remove the lines in [`brand_invert_orientation`].
///
/// # Errors
/// Returns a [`CreateImageError`](crate::optimizer::CreateImageError) if reading the file fails;
/// otherwise, it returns `Ok` with either the **upright** image or the original image (if no orientation found).
pub fn auto_orient_image<P>(
    original: DynamicImage,
    source_path: &P,
) -> Result<DynamicImage, crate::optimizer::CreateImageError>
where
    P: AsRef<Path> + AsRef<OsStr>,
{
    // Attempt to parse EXIF data from the file
    let parsed = match rexif::parse_file(source_path) {
        Ok(p) => p,
        Err(_) => {
            // If no EXIF or parse error => return original image unaltered
            return Ok(original);
        }
    };

    let mut orientation_code: Option<u16> = None;
    let mut camera_make: Option<String> = None;

    // Search the entire EXIF for "Make" and Orientation
    for entry in &parsed.entries {
        // If it's the "Make" tag => brand name
        if entry.tag == rexif::ExifTag::Make {
            if let rexif::TagValue::Ascii(ref mk) = entry.value {
                camera_make = Some(mk.clone());
            }
        }
        // If it's Orientation => numeric tag 0x0112
        if entry.ifd.tag == 0x0112 {
            if let rexif::TagValue::U16(ref vals) = entry.value {
                if let Some(&val) = vals.first() {
                    orientation_code = Some(val);
                }
            }
        }
    }

    // If no orientation => nothing to do
    let mut code = match orientation_code {
        Some(c) => c,
        None => return Ok(original),
    };

    // Adjust orientation code based on brand quirks
    if let Some(make) = &camera_make {
        code = brand_invert_orientation(make, code);
    }

    // Apply the standard EXIF transforms
    let corrected = fix_orientation_standard(original, code.into());
    Ok(corrected)
}

/// Applies brand-specific orientation “quirks.”
///
/// By default, we handle **Canon** by swapping orientation 6 ↔ 8.
/// If your Canon images become upside down, remove these swaps.
/// You can add more brand cases as needed.
fn brand_invert_orientation(brand: &str, code: u16) -> u16 {
    // Make the brand comparison case-insensitive
    let brand_lower = brand.to_ascii_lowercase();
    println!("Brand: {brand_lower}, Orientation Code: {code}");

    // if brand_lower.contains("canon") {
    //     // Canon often needs 6 ↔ 8 swapped
    //     match code {
    //         6 => 8,
    //         8 => 6,
    //         _ => code,
    //     }
    // } else {
        // If other brand known to need special handling, do it here
        // e.g. "sony", "nikon", etc. in the future
    code
    // }
}

/// Standard EXIF orientation transforms for codes 1..8.
///
/// 1 = "no rotation"
/// 2 = flip horizontal
/// 3 = rotate 180°
/// 4 = flip vertical
/// 5 = rotate 90° + flip horizontal
/// 6 = rotate 90°
/// 7 = rotate 270° + flip horizontal
/// 8 = rotate 270°
fn fix_orientation_standard(img: DynamicImage, orientation: u32) -> DynamicImage {
    match orientation {
        2 => DynamicImage::from(flip_horizontal(&img)),
        3 => DynamicImage::from(rotate180(&img)),
        4 => DynamicImage::from(flip_vertical(&img)),
        5 => DynamicImage::from(rotate90(&flip_horizontal(&img))),
        6 => DynamicImage::from(rotate90(&img)),
        7 => DynamicImage::from(rotate270(&flip_horizontal(&img))),
        8 => DynamicImage::from(rotate270(&img)),
        // 1 or unknown => no rotation
        _ => img,
    }
}
