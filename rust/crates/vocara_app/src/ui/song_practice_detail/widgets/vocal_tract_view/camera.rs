//! The 3D view's camera: an orthographic view of the head turned about its
//! vertical axis (yaw) and tipped toward the viewer (pitch), at one uniform
//! scale fitted to the well and a zoom. World coordinates are the 2D
//! view's centimetres with z across the head (the subject's right is +z):
//! at yaw 0 the camera looks at the subject's right side, as the 2D
//! cutaway does, the face to the right.

use std::f32::consts::FRAC_PI_2;

use iced::{Point, Size};

/// A point in the head, in cm: x forward, y up, z to the subject's right.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct P3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl P3 {
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }
}

/// The views the preset control offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Preset {
    Side,
    ThreeQuarter,
    Front,
}

impl Preset {
    pub const ALL: [Preset; 3] = [Preset::Side, Preset::ThreeQuarter, Preset::Front];

    pub fn label(self) -> &'static str {
        match self {
            Preset::Side => "Side",
            Preset::ThreeQuarter => "¾",
            Preset::Front => "Front",
        }
    }

    /// Its yaw and pitch (radians). The pitch looks down a little, so the
    /// airway's rings read as rings.
    pub fn angles(self) -> (f32, f32) {
        match self {
            Preset::Side => (0.15, 0.12),
            Preset::ThreeQuarter => (0.62, 0.3),
            Preset::Front => (FRAC_PI_2, 0.12),
        }
    }
}

/// Where the camera looks from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub yaw: f32,
    pub pitch: f32,
    pub zoom: f32,
}

/// How far a drag turns the head (radians per pixel).
pub const TURN_PER_PX: f32 = 0.01;
/// The pitch's limits (radians).
pub const PITCH_LIMIT: f32 = 1.2;
/// The zoom's limits.
pub const ZOOM_MIN: f32 = 0.6;
pub const ZOOM_MAX: f32 = 2.5;

impl Camera {
    pub fn preset(p: Preset) -> Self {
        let (yaw, pitch) = p.angles();
        Self {
            yaw,
            pitch,
            zoom: 1.0,
        }
    }

    /// Turned by a drag of (`dx`, `dy`) px: across turns the head about its
    /// vertical axis, down tips its top toward the viewer.
    pub fn turned(self, dx: f32, dy: f32) -> Self {
        Self {
            yaw: wrap(self.yaw + dx * TURN_PER_PX),
            pitch: (self.pitch + dy * TURN_PER_PX).clamp(-PITCH_LIMIT, PITCH_LIMIT),
            ..self
        }
    }

    /// Zoomed by a scroll of `lines` (up zooms in).
    pub fn zoomed(self, lines: f32) -> Self {
        Self {
            zoom: (self.zoom * (lines * 0.12).exp()).clamp(ZOOM_MIN, ZOOM_MAX),
            ..self
        }
    }

    /// The preset this camera shows, if it shows one (its zoom aside).
    pub fn matching(self) -> Option<Preset> {
        Preset::ALL.into_iter().find(|p| {
            let (yaw, pitch) = p.angles();
            wrap(self.yaw - yaw).abs() < 0.02 && (self.pitch - pitch).abs() < 0.02
        })
    }
}

/// An angle in (−π, π].
pub fn wrap(a: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    let w = (a + PI).rem_euclid(TAU) - PI;
    if w <= -PI {
        w + TAU
    } else {
        w
    }
}

/// What the 3D view fits to its well: the head from the foot of the larynx
/// to above the eyes (the crown runs out of the frame, as in the 2D
/// view), and its widest turn.
pub const SCENE_CENTRE: P3 = P3::new(1.0, 9.2, 0.0);
pub const SCENE_HALF_HEIGHT: f32 = 13.6;
pub const SCENE_HALF_WIDTH: f32 = 10.0;

/// A camera over a well: projects head points to the well's pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Projection {
    /// Pixels per cm (the fitted scale times the zoom).
    pub scale: f32,
    /// Where the scene's centre lands.
    pub centre: Point,
    sy: f32,
    cy: f32,
    sp: f32,
    cp: f32,
}

/// The scale (px per cm) that fits the scene in `size` with a margin,
/// before the zoom: one scale for both axes.
pub fn fitted_scale(size: Size) -> f32 {
    let w = size.width / (2.0 * SCENE_HALF_WIDTH);
    let h = size.height / (2.0 * SCENE_HALF_HEIGHT);
    w.min(h).max(0.0)
}

impl Projection {
    pub fn new(camera: Camera, size: Size) -> Self {
        let (sy, cy) = camera.yaw.sin_cos();
        let (sp, cp) = camera.pitch.sin_cos();
        Self {
            scale: fitted_scale(size) * camera.zoom,
            centre: Point::new(size.width / 2.0, size.height / 2.0),
            sy,
            cy,
            sp,
            cp,
        }
    }

    /// `p` in the camera's frame (cm, about the scene's centre): x right, y
    /// up, z toward the viewer.
    pub fn view(&self, p: P3) -> P3 {
        let (x, y, z) = (
            p.x - SCENE_CENTRE.x,
            p.y - SCENE_CENTRE.y,
            p.z - SCENE_CENTRE.z,
        );
        // Yaw: the face (+x) turns toward the viewer (+z).
        let x1 = x * self.cy - z * self.sy;
        let z1 = x * self.sy + z * self.cy;
        // Pitch: the crown (+y) tips toward the viewer.
        let y2 = y * self.cp - z1 * self.sp;
        let z2 = y * self.sp + z1 * self.cp;
        P3::new(x1, y2, z2)
    }

    /// `p` on screen, and its depth (cm toward the viewer).
    pub fn project(&self, p: P3) -> (Point, f32) {
        let v = self.view(p);
        (
            Point::new(
                self.centre.x + v.x * self.scale,
                self.centre.y - v.y * self.scale,
            ),
            v.z,
        )
    }

    pub fn point(&self, p: P3) -> Point {
        self.project(p).0
    }
}
