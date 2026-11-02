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
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedPoint {
    /// Horizontal normalized coordinate.
    #[schemars(range(min = 0, max = 1000))]
    pub x: u32,
    /// Vertical normalized coordinate.
    #[schemars(range(min = 0, max = 1000))]
    pub y: u32,
}

impl NormalizedPoint {
    /// Maps a valid normalized point to an exact image pixel, including its last pixel.
    pub fn to_pixel(self, size: PixelSize) -> Option<ScreenshotPoint> {
        if self.x > 1000 || self.y > 1000 || size.width == 0 || size.height == 0 {
            return None;
        }
        let axis = |value: u32, length: u32| {
            u32::try_from((u64::from(value) * u64::from(length - 1) + 500) / 1000).ok()
        };
        Some(ScreenshotPoint {
            x: axis(self.x, size.width)?,
            y: axis(self.y, size.height)?,
        })
    }

    /// Maps a source-image pixel into normalized action coordinates.
    pub fn from_pixel(point: ScreenshotPoint, size: PixelSize) -> Option<Self> {
        if !size.contains(point) {
            return None;
        }
        let axis = |value: u32, length: u32| {
            let extent = u64::from(length.saturating_sub(1).max(1));
            u32::try_from((u64::from(value) * 1000 + extent / 2) / extent).ok()
        };
        Some(Self {
            x: axis(point.x, size.width)?,
            y: axis(point.y, size.height)?,
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
            NormalizedPoint { x: 0, y: 0 }.to_pixel(size),
            Some(ScreenshotPoint { x: 0, y: 0 })
        );
        assert_eq!(
            NormalizedPoint { x: 1000, y: 1000 }.to_pixel(size),
            Some(ScreenshotPoint { x: 3839, y: 2159 })
        );
        assert!(NormalizedPoint { x: 1001, y: 0 }.to_pixel(size).is_none());
        let mapping = ScreenshotMapping {
            pixel_size: size,
            screen_bounds: ScreenRect {
                x: -1920.0,
                y: -1080.0,
                width: 1920.0,
                height: 1080.0,
            },
        };
        let point = NormalizedPoint { x: 500, y: 500 }.to_pixel(size).unwrap();
        assert_eq!(
            mapping.to_screen(point),
            Some(ScreenPoint {
                x: -960.0,
                y: -540.0
            })
        );
        assert_eq!(
            NormalizedPoint::from_pixel(point, size),
            Some(NormalizedPoint { x: 500, y: 500 })
        );
    }
}
