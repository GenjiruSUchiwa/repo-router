//! Planar shapes and their measurements.

/// A point in the plane.
pub struct Point {
    /// Horizontal coordinate.
    pub x: f64,
    /// Vertical coordinate.
    pub y: f64,
}

/// Computes the area of a circle with the given radius.
pub fn area_of_circle(radius: f64) -> f64 {
    std::f64::consts::PI * radius * radius
}

/// Computes the perimeter of a circle with the given radius.
pub fn perimeter_of_circle(radius: f64) -> f64 {
    2.0 * std::f64::consts::PI * radius
}
