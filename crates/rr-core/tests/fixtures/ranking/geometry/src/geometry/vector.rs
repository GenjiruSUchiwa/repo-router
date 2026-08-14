//! Vector arithmetic over points.

use crate::geometry::shapes::Point;

/// Computes the dot product of two vectors.
pub fn dot_product(left: &Point, right: &Point) -> f64 {
    left.x * right.x + left.y * right.y
}

/// Scales a vector to unit length.
pub fn normalize_vector(vector: &Point) -> Point {
    let length = dot_product(vector, vector).sqrt();
    Point {
        x: vector.x / length,
        y: vector.y / length,
    }
}
