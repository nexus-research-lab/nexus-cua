//! Platform-neutral observation integrity helpers.

use nexus_cua_protocol::ScreenRect;
use nexus_cua_runtime::RgbaImage;
use sha2::{Digest, Sha256};

const VISUAL_GRID: u32 = 32;
const MAX_AVERAGE_LUMA_DIFFERENCE: u64 = 8;
const MATERIAL_LUMA_DIFFERENCE: u8 = 24;
const MAX_MATERIAL_CHANGE_RATIO_DENOMINATOR: usize = 5;

pub(crate) fn fingerprint(
    window_key: &str,
    screen_bounds: ScreenRect,
    image: Option<&RgbaImage>,
) -> String {
    let mut digest = Sha256::new();
    digest.update(window_key.as_bytes());
    digest.update(screen_bounds.x.to_bits().to_be_bytes());
    digest.update(screen_bounds.y.to_bits().to_be_bytes());
    digest.update(screen_bounds.width.to_bits().to_be_bytes());
    digest.update(screen_bounds.height.to_bits().to_be_bytes());
    let geometry = hex::encode(digest.finalize());
    image.map_or_else(
        || format!("geometry:{geometry}"),
        |image| {
            format!(
                "geometry:{geometry}:visual:{}",
                hex::encode(visual_signature(image))
            )
        },
    )
}

pub(crate) fn fingerprints_match(expected: &str, current: &str) -> bool {
    let Some((expected_geometry, expected_visual)) = expected.split_once(":visual:") else {
        return expected == current;
    };
    let Some((current_geometry, current_visual)) = current.split_once(":visual:") else {
        return false;
    };
    if expected_geometry != current_geometry {
        return false;
    }
    let (Ok(expected), Ok(current)) = (hex::decode(expected_visual), hex::decode(current_visual))
    else {
        return false;
    };
    if expected.is_empty() || expected.len() != current.len() {
        return false;
    }
    let mut total_difference = 0_u64;
    let mut material_changes = 0_usize;
    for (expected, current) in expected.iter().zip(&current) {
        let difference = expected.abs_diff(*current);
        total_difference += u64::from(difference);
        material_changes += usize::from(difference > MATERIAL_LUMA_DIFFERENCE);
    }
    let sample_count = u64::try_from(expected.len()).unwrap_or(u64::MAX);
    total_difference <= MAX_AVERAGE_LUMA_DIFFERENCE * sample_count
        && material_changes * MAX_MATERIAL_CHANGE_RATIO_DENOMINATOR <= expected.len()
}

pub(crate) fn contains_rect(outer: ScreenRect, inner: ScreenRect) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.x + inner.width <= outer.x + outer.width
        && inner.y + inner.height <= outer.y + outer.height
}

fn visual_signature(image: &RgbaImage) -> Vec<u8> {
    if image.width == 0 || image.height == 0 {
        return Vec::new();
    }
    let mut signature = Vec::with_capacity((VISUAL_GRID * VISUAL_GRID) as usize);
    for grid_y in 0..VISUAL_GRID {
        let y = ((u64::from(grid_y) * u64::from(image.height) + u64::from(VISUAL_GRID / 2))
            / u64::from(VISUAL_GRID))
        .min(u64::from(image.height - 1));
        for grid_x in 0..VISUAL_GRID {
            let x = ((u64::from(grid_x) * u64::from(image.width) + u64::from(VISUAL_GRID / 2))
                / u64::from(VISUAL_GRID))
            .min(u64::from(image.width - 1));
            let index = usize::try_from((y * u64::from(image.width) + x) * 4).unwrap_or(0);
            let pixel = image.pixels.get(index..index + 3).unwrap_or(&[0, 0, 0]);
            let luma =
                (u16::from(pixel[0]) * 54 + u16::from(pixel[1]) * 183 + u16::from(pixel[2]) * 19)
                    / 256;
            signature.push(u8::try_from(luma).unwrap_or(u8::MAX));
        }
    }
    signature
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDS: ScreenRect = ScreenRect {
        x: -20.0,
        y: 10.0,
        width: 100.0,
        height: 80.0,
    };

    fn image(value: u8) -> RgbaImage {
        RgbaImage {
            width: 32,
            height: 32,
            pixels: [value, value, value, u8::MAX].repeat(32 * 32),
        }
    }

    #[test]
    fn fingerprint_tracks_target_geometry() {
        let first = fingerprint("window:a", BOUNDS, None);
        let moved = fingerprint(
            "window:a",
            ScreenRect {
                x: BOUNDS.x + 1.0,
                ..BOUNDS
            },
            None,
        );
        assert_ne!(first, moved);
        assert!(!fingerprints_match(&first, &moved));
    }

    #[test]
    fn visual_match_tolerates_small_uniform_change() {
        let first = fingerprint("window:a", BOUNDS, Some(&image(100)));
        let second = fingerprint("window:a", BOUNDS, Some(&image(106)));
        assert!(fingerprints_match(&first, &second));
    }

    #[test]
    fn visual_match_rejects_material_change() {
        let first = fingerprint("window:a", BOUNDS, Some(&image(32)));
        let second = fingerprint("window:a", BOUNDS, Some(&image(220)));
        assert!(!fingerprints_match(&first, &second));
    }

    #[test]
    fn containment_handles_negative_desktop_coordinates() {
        assert!(contains_rect(
            BOUNDS,
            ScreenRect {
                x: -10.0,
                y: 20.0,
                width: 40.0,
                height: 30.0,
            }
        ));
    }
}
