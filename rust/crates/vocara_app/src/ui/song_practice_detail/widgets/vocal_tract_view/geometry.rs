//! Small plane geometry for the outlines: the mockup generator's
//! Catmull–Rom smoothing, rotation about a point, arc-length resampling,
//! and the measures the views read off an outline.

use super::pose::Cm;

/// A Catmull–Rom spline through `pts`, `n` points per span, ending on the
/// last point (the mockup generator's `catmull`, its ends doubled).
pub fn catmull(pts: &[Cm], n: usize) -> Vec<Cm> {
    if pts.len() < 2 || n == 0 {
        return pts.to_vec();
    }
    let mut p = Vec::with_capacity(pts.len() + 2);
    p.push(pts[0]);
    p.extend_from_slice(pts);
    p.push(pts[pts.len() - 1]);
    let mut out = Vec::with_capacity((pts.len() - 1) * n + 1);
    for i in 1..p.len() - 2 {
        let (p0, p1, p2, p3) = (p[i - 1], p[i], p[i + 1], p[i + 2]);
        for k in 0..n {
            let t = k as f32 / n as f32;
            let (t2, t3) = (t * t, t * t * t);
            let f = |a: f32, b: f32, c: f32, d: f32| {
                0.5 * ((2.0 * b)
                    + (-a + c) * t
                    + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2
                    + (-a + 3.0 * b - 3.0 * c + d) * t3)
            };
            out.push(Cm::new(
                f(p0.x, p1.x, p2.x, p3.x),
                f(p0.y, p1.y, p2.y, p3.y),
            ));
        }
    }
    out.push(pts[pts.len() - 1]);
    out
}

/// `p` turned by `angle` radians (counter-clockwise) about `centre`.
pub fn rotate(p: Cm, centre: Cm, angle: f32) -> Cm {
    let (s, c) = angle.sin_cos();
    let d = p.minus(centre);
    Cm::new(centre.x + d.x * c - d.y * s, centre.y + d.x * s + d.y * c)
}

/// Every point of `pts` turned about `centre`.
pub fn rotate_all(pts: &[Cm], centre: Cm, angle: f32) -> Vec<Cm> {
    pts.iter().map(|p| rotate(*p, centre, angle)).collect()
}

/// Every point of `pts` moved by `d`.
pub fn translate_all(pts: &[Cm], d: Cm) -> Vec<Cm> {
    pts.iter().map(|p| p.plus(d)).collect()
}

/// The length along `pts` (closing back to the start when `closed`).
pub fn length(pts: &[Cm], closed: bool) -> f32 {
    let mut len: f32 = pts.windows(2).map(|w| w[0].dist(w[1])).sum();
    if closed && pts.len() > 2 {
        len += pts[pts.len() - 1].dist(pts[0]);
    }
    len
}

/// `n` points evenly spaced along `pts` (around it when `closed`, the
/// first point kept; end to end otherwise).
pub fn resample(pts: &[Cm], n: usize, closed: bool) -> Vec<Cm> {
    if pts.is_empty() || n == 0 {
        return Vec::new();
    }
    if pts.len() == 1 {
        return vec![pts[0]; n];
    }
    let mut path: Vec<Cm> = pts.to_vec();
    if closed {
        path.push(pts[0]);
    }
    let mut cum = Vec::with_capacity(path.len());
    let mut acc = 0.0;
    cum.push(0.0);
    for w in path.windows(2) {
        acc += w[0].dist(w[1]);
        cum.push(acc);
    }
    let total = acc.max(1e-6);
    let steps = if closed { n } else { (n - 1).max(1) };
    let mut out = Vec::with_capacity(n);
    let mut seg = 0;
    for i in 0..n {
        let s = total * i as f32 / steps as f32;
        while seg + 1 < cum.len() - 1 && cum[seg + 1] < s {
            seg += 1;
        }
        let span = (cum[seg + 1] - cum[seg]).max(1e-6);
        let t = ((s - cum[seg]) / span).clamp(0.0, 1.0);
        out.push(path[seg].lerp(path[seg + 1], t));
    }
    out
}

/// The mean of the points.
pub fn centroid(pts: &[Cm]) -> Cm {
    if pts.is_empty() {
        return Cm::default();
    }
    let sum = pts.iter().fold(Cm::default(), |a, p| a.plus(*p));
    sum.scale(1.0 / pts.len() as f32)
}

/// The smallest box holding the points: (min, max).
pub fn bounds(pts: &[Cm]) -> (Cm, Cm) {
    let mut lo = Cm::new(f32::INFINITY, f32::INFINITY);
    let mut hi = Cm::new(f32::NEG_INFINITY, f32::NEG_INFINITY);
    for p in pts {
        lo = Cm::new(lo.x.min(p.x), lo.y.min(p.y));
        hi = Cm::new(hi.x.max(p.x), hi.y.max(p.y));
    }
    (lo, hi)
}

/// The tongue's dorsum: its outline up to its frontmost point (see the
/// pose's docs).
pub fn dorsum(tongue: &[Cm]) -> &[Cm] {
    let front = tongue
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.x.total_cmp(&b.1.x))
        .map_or(0, |(i, _)| i);
    &tongue[..=front.min(tongue.len().saturating_sub(1))]
}

/// The point of `pts` nearest `to`, and its distance.
pub fn nearest(pts: &[Cm], to: Cm) -> (Cm, f32) {
    pts.iter()
        .map(|p| (*p, p.dist(to)))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap_or((to, f32::INFINITY))
}
