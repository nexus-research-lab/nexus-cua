//! Explicit logical-screen and captured-image coordinate spaces.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A point in logical, top-left-origin screen coordinates.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenPoint {
    /// Horizontal logical position, possibly negative on secondary displays.
    pub x: f64,
    /// Vertical logical position, possibly negative on secondary displays.
    pub y: f64,
}

/// A rectangle in logical, top-left-origin screen coordinates.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenRect {
    /// Logical left edge.
    pub x: f64,
    /// Logical top edge.
    pub y: f64,
    /// Non-negative logical width.
    pub width: f64,
    /// Non-negative logical height.
    pub height: f64,
}

impl ScreenRect {
    /// Returns whether a logical screen point lies within this rectangle.
    pub fn contains(self, point: ScreenPoint) -> bool {
        point.x >= self.x
            && point.y >= self.y
            && point.x <= self.x + self.width
            && point.y <= self.y + self.height
    }
}

/// An integer pixel coordinate in the exact screenshot artifact.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenshotPoint {
    /// Zero-based horizontal pixel coordinate.
    pub x: u32,
    /// Zero-based vertical pixel coordinate.
    pub y: u32,
}

/// A top-left-origin point normalized to the inclusive range 0..1000.
/// This coordinate space is independent of captured or model-resized image dimensions.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedPoint {
    /// Finite horizontal coordinate in the inclusive range 0..1000.
    #[schemars(range(min = 0, max = 1000))]
    pub x: f64,
    /// Finite vertical coordinate in the inclusive range 0..1000.
    #[schemars(range(min = 0, max = 1000))]
    pub y: f64,
}

impl NormalizedPoint {
    /// Maps finite normalized coordinates to pixels, rounding only here.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn to_pixel(self, size: PixelSize) -> Option<ScreenshotPoint> {
        if !self.x.is_finite()
            || !self.y.is_finite()
            || !(0.0..=1000.0).contains(&self.x)
            || !(0.0..=1000.0).contains(&self.y)
            || size.width == 0
            || size.height == 0
        {
            return None;
        }
        // Validated coordinates map into [0, length-1], which fits u32.
        let axis =
            |value: f64, length: u32| (value * f64::from(length - 1) / 1000.0).round() as u32;
        Some(ScreenshotPoint {
            x: axis(self.x, size.width),
            y: axis(self.y, size.height),
        })
    }

    /// Preserves sub-unit precision when projecting a crop pixel into the source.
    pub fn from_pixel(point: ScreenshotPoint, size: PixelSize) -> Option<Self> {
        if !size.contains(point) {
            return None;
        }
        let axis = |value: u32, length: u32| {
            f64::from(value) * 1000.0 / f64::from(length.saturating_sub(1).max(1))
        };
        Some(Self {
            x: axis(point.x, size.width),
            y: axis(point.y, size.height),
        })
    }
}

/// Exact physical dimensions of a screenshot artifact.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PixelSize {
    /// Pixel columns.
    pub width: u32,
    /// Pixel rows.
    pub height: u32,
}

impl PixelSize {
    /// Returns whether a pixel coordinate addresses this image.
    pub fn contains(self, point: ScreenshotPoint) -> bool {
        point.x < self.width && point.y < self.height
    }
}

/// Checked affine mapping between one screenshot and logical screen space.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenshotMapping {
    /// Logical screen rectangle represented by the image.
    pub screen_bounds: ScreenRect,
    /// Exact image dimensions.
    pub pixel_size: PixelSize,
}

impl ScreenshotMapping {
    /// Converts an in-bounds screenshot pixel to a logical screen point.
    pub fn to_screen(self, point: ScreenshotPoint) -> Option<ScreenPoint> {
        if !self.pixel_size.contains(point)
            || self.screen_bounds.width <= 0.0
            || self.screen_bounds.height <= 0.0
        {
            return None;
        }
        Some(ScreenPoint {
            x: self.screen_bounds.x
                + f64::from(point.x) * self.screen_bounds.width / f64::from(self.pixel_size.width),
            y: self.screen_bounds.y
                + f64::from(point.y) * self.screen_bounds.height
                    / f64::from(self.pixel_size.height),
        })
    }
}

#[cfg(test)]
mod normalized_tests {
    use super::*;

    #[test]
    fn normalized_mapping_covers_retina_edges_and_rejects_outside_points() {
        let size = PixelSize {
            width: 3840,
            height: 2160,
        };
        assert_eq!(
            NormalizedPoint { x: 0.0, y: 0.0 }.to_pixel(size),
            Some(ScreenshotPoint { x: 0, y: 0 })
        );
        assert_eq!(
            NormalizedPoint {
                x: 1000.0,
                y: 1000.0
            }
            .to_pixel(size),
            Some(ScreenshotPoint { x: 3839, y: 2159 })
        );
        assert!(
            NormalizedPoint { x: 1001.0, y: 0.0 }
                .to_pixel(size)
                .is_none()
        );
        let mapping = ScreenshotMapping {
            pixel_size: size,
            screen_bounds: ScreenRect {
                x: -1920.0,
                y: -1080.0,
                width: 1920.0,
                height: 1080.0,
            },
        };
        let point = NormalizedPoint { x: 500.0, y: 500.0 }
            .to_pixel(size)
            .unwrap();
        assert_eq!(
            mapping.to_screen(point),
            Some(ScreenPoint {
                x: -960.0,
                y: -540.0
            })
        );
        assert_eq!(
            NormalizedPoint::from_pixel(point, size)
                .unwrap()
                .to_pixel(size),
            Some(point)
        );
    }
}

#[cfg(test)]
mod precision_tests {
    use super::*;
    #[test]
    fn every_axis_pixel_round_trips_at_4k_and_laptop_resolution() {
        for size in [
            PixelSize {
                width: 3840,
                height: 2160,
            },
            PixelSize {
                width: 1366,
                height: 768,
            },
            PixelSize {
                width: 1,
                height: 1,
            },
        ] {
            for x in 0..size.width {
                let point = ScreenshotPoint { x, y: 0 };
                assert_eq!(
                    NormalizedPoint::from_pixel(point, size)
                        .unwrap()
                        .to_pixel(size),
                    Some(point)
                );
            }
            for y in 0..size.height {
                let point = ScreenshotPoint { x: 0, y };
                assert_eq!(
                    NormalizedPoint::from_pixel(point, size)
                        .unwrap()
                        .to_pixel(size),
                    Some(point)
                );
            }
        }
    }
    #[test]
    fn nonfinite_and_out_of_bounds_coordinates_never_map() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.1, 1000.1] {
            assert!(
                NormalizedPoint { x: value, y: 0.0 }
                    .to_pixel(PixelSize {
                        width: 3840,
                        height: 2160
                    })
                    .is_none()
            );
        }
    }
}
