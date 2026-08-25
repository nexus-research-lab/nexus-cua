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
