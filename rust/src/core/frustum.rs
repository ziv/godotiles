//! Six-plane frustum vs. tile height-column test, used by the eviction pass (Godot does not
//! report per-instance culling results). Planes follow Godot's convention: `normal · p = d`,
//! normals pointing OUTWARD, so a point is inside iff `normal · p − d <= 0` for all six.

use super::config::{MAX_WORLD_HEIGHT, MIN_WORLD_HEIGHT, WORLD_HEIGHT_MARGIN};
use glam::Vec3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plane {
    pub normal: Vec3,
    pub d: f32,
}

impl Plane {
    /// Signed distance; positive = on the side the normal points to (Godot's `distance_to`).
    pub fn distance_to(&self, p: Vec3) -> f32 {
        self.normal.dot(p) - self.d
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frustum {
    pub planes: [Plane; 6],
}

impl Frustum {
    /// True when the vertical column `[center ± half] × [MIN_WORLD_HEIGHT, MAX_WORLD_HEIGHT +
    /// margin]` intersects the frustum. Tests the AABB corner least along each outward normal:
    /// if even that corner is outside a plane, the whole box is.
    pub fn intersects_column(&self, center_x: f32, center_z: f32, half: f32) -> bool {
        let min = Vec3::new(center_x - half, MIN_WORLD_HEIGHT, center_z - half);
        let max = Vec3::new(
            center_x + half,
            MAX_WORLD_HEIGHT + WORLD_HEIGHT_MARGIN,
            center_z + half,
        );
        self.planes.iter().all(|p| {
            let c = Vec3::new(
                if p.normal.x > 0.0 { min.x } else { max.x },
                if p.normal.y > 0.0 { min.y } else { max.y },
                if p.normal.z > 0.0 { min.z } else { max.z },
            );
            p.distance_to(c) <= 0.0
        })
    }

    /// True when `p` is inside all six planes (for self-checks).
    pub fn contains_point(&self, p: Vec3) -> bool {
        self.planes.iter().all(|pl| pl.distance_to(p) <= 0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An axis-aligned box frustum `[-10, 10]³` expressed with outward normals.
    fn cube() -> Frustum {
        let p = |n: Vec3| Plane { normal: n, d: 10.0 };
        Frustum {
            planes: [
                p(Vec3::X),
                p(-Vec3::X),
                p(Vec3::Y),
                p(-Vec3::Y),
                p(Vec3::Z),
                p(-Vec3::Z),
            ],
        }
    }

    #[test]
    fn points_and_columns() {
        let f = cube();
        assert!(f.contains_point(Vec3::ZERO));
        assert!(!f.contains_point(Vec3::new(11.0, 0.0, 0.0)));
        assert!(f.intersects_column(0.0, 0.0, 1.0));
        assert!(f.intersects_column(10.5, 0.0, 1.0)); // overlaps the +x face
        assert!(!f.intersects_column(12.0, 0.0, 1.0)); // fully outside +x
        assert!(!f.intersects_column(0.0, 12.0, 1.0));
    }
}
