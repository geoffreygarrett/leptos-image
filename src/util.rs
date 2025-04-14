#[cfg(feature = "ssr")]
use image::imageops::{flip_horizontal, flip_vertical, rotate180, rotate90, rotate270};
use std::path::Path;

#[cfg(feature = "ssr")]
pub fn auto_orient_image(
    original: image::DynamicImage,
    source_path: &Path,
) -> Result<image::DynamicImage, crate::optimizer::CreateImageError> {
    // Attempt to read EXIF data via rexif
    match rexif::parse_file(source_path) {
        Ok(parsed) => {
            println!("Parsed EXIF data: {:#?}", parsed);

            // We’ll keep track of the *last* orientation we find
            let mut orientation_code: Option<u16> = None;

            for entry in parsed.entries {
                // Print debug info
                println!("Entry: {:?}", entry);

                // Check numeric tag 0x0112 (Orientation)
                if entry.ifd.tag == 0x0112 {
                    if let rexif::TagValue::U16(vals) = entry.value {
                        if let Some(&val) = vals.first() {
                            orientation_code = Some(val);
                            // Don’t return; keep scanning if the file has multiple orientation entries
                        }
                    }
                }
            }

            // If we found an orientation code, apply it
            if let Some(code) = orientation_code {
                println!("Found orientation (U16): {code}");
                // NOTE: If Canon is reversed, invert 6 ↔ 8.
                // By default, orientation=6 is “rotate90” (standard EXIF).
                // If that’s wrong on your images, swap them:
                return Ok(fix_orientation_canon_reversed(original, code.into()));
            }

            // If no Orientation tag found => return the image unchanged
            Ok(original)
        }
        // If parse fails or no EXIF => return original image
        Err(_) => Ok(original),
    }
}

/// Adjust the orientation. This version *inverts* orientation 6 & 8, in case Canon’s label is reversed.
/// If your images end up *more* sideways, swap them back!
#[cfg(feature = "ssr")]
fn fix_orientation_canon_reversed(img: image::DynamicImage, orientation: u32) -> image::DynamicImage {
    match orientation {
        // 2 => mirror horizontal
        2 => image::DynamicImage::from(flip_horizontal(&img)),

        // 3 => rotate 180
        3 => image::DynamicImage::from(rotate180(&img)),

        // 4 => mirror vertical
        4 => image::DynamicImage::from(flip_vertical(&img)),

        // 5 => rotate 90 + mirror horizontal (rare)
        5 => image::DynamicImage::from(rotate90(&flip_horizontal(&img))),

        // 6 (Canon says “Rotated to left”) => rotate LEFT 90 = rotate270
        // (Standard EXIF would do rotate90, but we invert it here)
        6 => image::DynamicImage::from(rotate270(&img)),

        // 7 => rotate 270 + mirror horizontal
        7 => image::DynamicImage::from(rotate270(&flip_horizontal(&img))),

        // 8 (Canon says “Rotated to right”) => rotate RIGHT 90 = rotate90
        // (Standard EXIF would do rotate270, but we invert it here)
        8 => image::DynamicImage::from(rotate90(&img)),

        // 1 or anything else => no rotation
        _ => img,
    }
}
