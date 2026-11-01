//! Quaternion helpers, written in (w, x, y, z) order, matching the operation
//! order of the Python implementation and of scipy's rotation conversions.

pub type Quat = [f64; 4];
pub type Vec3 = [f64; 3];

#[inline]
pub fn dot(a: &Quat, b: &Quat) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3]
}

#[inline]
pub fn negate(q: Quat) -> Quat {
    [-q[0], -q[1], -q[2], -q[3]]
}

#[inline]
pub fn conjugate(q: &Quat) -> Quat {
    [q[0], -q[1], -q[2], -q[3]]
}

#[inline]
pub fn norm(q: &Quat) -> f64 {
    (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt()
}

#[inline]
pub fn normalize(q: &Quat) -> Quat {
    let n = norm(q);
    [q[0] / n, q[1] / n, q[2] / n, q[3] / n]
}

/// scipy's `Rotation.from_quat` normalizes in the order (x, y, z, w) of its own
/// quaternion array, which sums the four squares in that order.
#[inline]
pub fn norm_xyzw(q: &Quat) -> f64 {
    (q[1] * q[1] + q[2] * q[2] + q[3] * q[3] + q[0] * q[0]).sqrt()
}

#[inline]
pub fn normalize_xyzw(q: &Quat) -> Quat {
    let n = norm_xyzw(q);
    [q[0] / n, q[1] / n, q[2] / n, q[3] / n]
}

/// The Hamilton product as `gyro_doctor.core.repaired_rates_to_quaternions`
/// writes it.
#[inline]
pub fn hamilton_app(a: &Quat, b: &Quat) -> Quat {
    let (w0, x0, y0, z0) = (a[0], a[1], a[2], a[3]);
    let (w1, x1, y1, z1) = (b[0], b[1], b[2], b[3]);
    [
        w0 * w1 - x0 * x1 - y0 * y1 - z0 * z1,
        w0 * x1 + x0 * w1 + y0 * z1 - z0 * y1,
        w0 * y1 - x0 * z1 + y0 * w1 + z0 * x1,
        w0 * z1 + x0 * y1 - y0 * x1 + z0 * w1,
    ]
}

/// The Hamilton product in the order scipy's `compose_quat` evaluates it.
#[inline]
pub fn hamilton_scipy(p: &Quat, q: &Quat) -> Quat {
    let cx = p[2] * q[3] - p[3] * q[2];
    let cy = p[3] * q[1] - p[1] * q[3];
    let cz = p[1] * q[2] - p[2] * q[1];
    [
        p[0] * q[0] - p[1] * q[1] - p[2] * q[2] - p[3] * q[3],
        p[0] * q[1] + q[0] * p[1] + cx,
        p[0] * q[2] + q[0] * p[2] + cy,
        p[0] * q[3] + q[0] * p[3] + cz,
    ]
}

/// scipy's `_quat_canonical`: the sign that puts w at or above zero, with the
/// same tie breaks on w, x, y, then z.
#[inline]
pub fn canonical(q: &Quat) -> Quat {
    let w = q[0];
    let x = q[1];
    let y = q[2];
    let z = q[3];
    let flip = w < 0.0
        || (w == 0.0 && x < 0.0)
        || (w == 0.0 && x == 0.0 && y < 0.0)
        || (w == 0.0 && x == 0.0 && y == 0.0 && z < 0.0);
    if flip { negate(*q) } else { *q }
}

/// Flip each quaternion whose dot product with the one before it is negative.
pub fn canonicalize_sequence(quaternions: &mut [Quat]) {
    for i in 1..quaternions.len() {
        if dot(&quaternions[i], &quaternions[i - 1]) < 0.0 {
            quaternions[i] = negate(quaternions[i]);
        }
    }
}

/// scipy `Rotation.from_rotvec(rotvec).as_quat()` in (w, x, y, z) order.
#[inline]
pub fn from_rotvec(rotvec: &Vec3) -> Quat {
    let angle = (rotvec[0] * rotvec[0] + rotvec[1] * rotvec[1] + rotvec[2] * rotvec[2])
        .sqrt();
    let small_angle = angle <= 1e-3;
    let angle2 = angle * angle;
    let small_scale = 0.5 - angle2 / 48.0 + angle2 * angle2 / 3840.0;
    let div_angle = angle + if small_angle { 1.0 } else { 0.0 };
    let large_scale = (angle / 2.0).sin() / div_angle;
    let scale = if small_angle { small_scale } else { large_scale };
    [
        (angle / 2.0).cos(),
        rotvec[0] * scale,
        rotvec[1] * scale,
        rotvec[2] * scale,
    ]
}

/// scipy `Rotation.from_quat(q).as_rotvec()`.
#[inline]
pub fn to_rotvec(q: &Quat) -> Vec3 {
    let q = canonical(q);
    let ax_norm = (q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    let angle = 2.0 * ax_norm.atan2(q[0]);
    let small_angle = angle <= 1e-3;
    let angle2 = angle * angle;
    let small_scale = 2.0 + angle2 / 12.0 + 7.0 * angle2 * angle2 / 2880.0;
    let div_sin = (angle / 2.0).sin() + if small_angle { 1.0 } else { 0.0 };
    let large_scale = angle / div_sin;
    let scale = if small_angle { small_scale } else { large_scale };
    [q[1] * scale, q[2] * scale, q[3] * scale]
}

/// scipy's `Rotation` composition, which normalizes the composed quaternion in its
/// own (x, y, z, w) order.
#[inline]
pub fn compose_scipy(a: &Quat, b: &Quat) -> Quat {
    normalize_xyzw(&hamilton_scipy(a, b))
}

/// The rotation vector from the orientation `a` to the orientation `b`, as
/// scipy computes `(R_a.inv() * R_b).as_rotvec()`.
#[inline]
pub fn relative_rotvec(a: &Quat, b: &Quat) -> Vec3 {
    let a = normalize_xyzw(a);
    let b = normalize_xyzw(b);
    let relative = compose_scipy(&conjugate(&a), &b);
    to_rotvec(&relative)
}

#[inline]
pub fn equal_exact(a: &Quat, b: &Quat) -> bool {
    a[0] == b[0] && a[1] == b[1] && a[2] == b[2] && a[3] == b[3]
}
