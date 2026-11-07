//! Canvas placement is independent from the source OS's display topology.
use crate::Error;

const MAX_COORDINATE: f64 = 1_000_000_000.0;
const MAX_DIMENSION: u32 = 16_384;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn validate(self) -> Result<(), Error> {
        if !self.x.is_finite()
            || !self.y.is_finite()
            || self.x.abs() > MAX_COORDINATE
            || self.y.abs() > MAX_COORDINATE
        {
            return Err(Error::InvalidGeometry);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn validate(self) -> Result<(), Error> {
        Point {
            x: self.x,
            y: self.y,
        }
        .validate()?;
        if !self.width.is_finite()
            || !self.height.is_finite()
            || self.width <= 0.0
            || self.height <= 0.0
        {
            return Err(Error::InvalidGeometry);
        }
        Point {
            x: self.x + self.width,
            y: self.y + self.height,
        }
        .validate()
    }

    /// Half-open edges avoid ambiguous ownership at adjacent panels.
    pub fn contains(self, point: Point) -> bool {
        point.x >= self.x
            && point.y >= self.y
            && point.x < self.x + self.width
            && point.y < self.y + self.height
    }

    pub fn intersects(self, other: Self) -> bool {
        self.x < other.x + other.width
            && other.x < self.x + self.width
            && self.y < other.y + other.height
            && other.y < self.y + self.height
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PixelSize {
    pub width: u32,
    pub height: u32,
}

impl PixelSize {
    pub fn validate(self) -> Result<(), Error> {
        if self.width == 0
            || self.height == 0
            || self.width > MAX_DIMENSION
            || self.height > MAX_DIMENSION
        {
            return Err(Error::InvalidGeometry);
        }
        Ok(())
    }

    pub fn rgba_bytes(self) -> Result<u64, Error> {
        self.validate()?;
        Ok(u64::from(self.width) * u64::from(self.height) * 4)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Rotation {
    None,
    Clockwise90,
    HalfTurn,
    Clockwise270,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// Canvas coordinate visible at the viewport's top-left corner.
    pub origin: Point,
    pub zoom: f64,
}

impl Camera {
    pub fn validate(self) -> Result<(), Error> {
        self.origin.validate()?;
        if !self.zoom.is_finite() || !(0.05..=20.0).contains(&self.zoom) {
            return Err(Error::InvalidGeometry);
        }
        Ok(())
    }

    pub fn to_canvas(self, viewport: Point) -> Result<Point, Error> {
        self.validate()?;
        viewport.validate()?;
        let result = Point {
            x: self.origin.x + viewport.x / self.zoom,
            y: self.origin.y + viewport.y / self.zoom,
        };
        result.validate()?;
        Ok(result)
    }
}

/// Native geometry supplied by a trusted platform registry, never a window handle.
/// Any geometry change MUST increment revision; UI or peer input cannot choose it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SourceGeometry {
    pub pixels: PixelSize,
    /// Native logical coordinates; negative monitor origins are permitted.
    pub logical: Rect,
    pub rotation: Rotation,
    pub revision: u64,
}

impl SourceGeometry {
    pub fn validate(self) -> Result<(), Error> {
        self.pixels.validate()?;
        self.logical.validate()?;
        if self.revision == 0 {
            return Err(Error::InvalidGeometry);
        }
        Ok(())
    }

    pub fn display_size(self) -> PixelSize {
        match self.rotation {
            Rotation::Clockwise90 | Rotation::Clockwise270 => PixelSize {
                width: self.pixels.height,
                height: self.pixels.width,
            },
            _ => self.pixels,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MappedPoint {
    pub normalized: Point,
    pub logical: Point,
    pub pixel_x: u32,
    pub pixel_y: u32,
}

/// Fit a rotated source inside a canvas panel, rejecting letterbox clicks.
/// This does not change the source resolution, window bounds, or display layout.
pub fn map_pointer(
    camera: Camera,
    panel: Rect,
    source: SourceGeometry,
    viewport_point: Point,
) -> Result<MappedPoint, Error> {
    panel.validate()?;
    source.validate()?;
    let p = camera.to_canvas(viewport_point)?;
    let size = source.display_size();
    let scale = (panel.width / f64::from(size.width)).min(panel.height / f64::from(size.height));
    let content = Rect {
        x: panel.x + (panel.width - f64::from(size.width) * scale) / 2.0,
        y: panel.y + (panel.height - f64::from(size.height) * scale) / 2.0,
        width: f64::from(size.width) * scale,
        height: f64::from(size.height) * scale,
    };
    if !content.contains(p) {
        return Err(Error::OutsideContent);
    }
    let u = (p.x - content.x) / content.width;
    let v = (p.y - content.y) / content.height;
    let (x, y) = match source.rotation {
        Rotation::None => (u, v),
        Rotation::Clockwise90 => (v, 1.0 - u),
        Rotation::HalfTurn => (1.0 - u, 1.0 - v),
        Rotation::Clockwise270 => (1.0 - v, u),
    };
    // An inclusive zero edge may rotate to normalized one. Clamp to the final
    // addressable pixel, never a point outside the source's native surface.
    let x = x.clamp(0.0, 1.0 - 1.0 / f64::from(source.pixels.width));
    let y = y.clamp(0.0, 1.0 - 1.0 / f64::from(source.pixels.height));
    Ok(MappedPoint {
        normalized: Point { x, y },
        logical: Point {
            x: source.logical.x + x * source.logical.width,
            y: source.logical.y + y * source.logical.height,
        },
        pixel_x: (x * f64::from(source.pixels.width)).floor() as u32,
        pixel_y: (y * f64::from(source.pixels.height)).floor() as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> SourceGeometry {
        SourceGeometry {
            pixels: PixelSize {
                width: 200,
                height: 100,
            },
            logical: Rect {
                x: -100.0,
                y: 0.0,
                width: 100.0,
                height: 50.0,
            },
            rotation: Rotation::None,
            revision: 1,
        }
    }

    fn camera() -> Camera {
        Camera {
            origin: Point { x: 0.0, y: 0.0 },
            zoom: 1.0,
        }
    }

    fn panel() -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        }
    }

    #[test]
    fn retina_negative_origin_and_letterboxing() {
        let mapped = map_pointer(camera(), panel(), source(), Point { x: 50.0, y: 50.0 });
        let mapped = mapped.unwrap();
        assert_eq!((mapped.pixel_x, mapped.pixel_y), (100, 50));
        assert_eq!(mapped.logical, Point { x: -50.0, y: 25.0 });
        assert_eq!(
            map_pointer(camera(), panel(), source(), Point { x: 50.0, y: 24.0 }),
            Err(Error::OutsideContent)
        );
    }

    #[test]
    fn pan_and_zoom_do_not_change_source_coordinates() {
        let mut c = camera();
        c.origin = Point { x: 25.0, y: 25.0 };
        c.zoom = 2.0;
        let a = map_pointer(c, panel(), source(), Point { x: 50.0, y: 50.0 }).unwrap();
        let b = map_pointer(camera(), panel(), source(), Point { x: 50.0, y: 50.0 });
        assert_eq!(a, b.unwrap());
    }

    #[test]
    fn all_rotations_map_center_and_stay_in_pixel_bounds() {
        for rotation in [
            Rotation::None,
            Rotation::Clockwise90,
            Rotation::HalfTurn,
            Rotation::Clockwise270,
        ] {
            let mut s = source();
            s.rotation = rotation;
            let p = map_pointer(camera(), panel(), s, Point { x: 50.0, y: 50.0 });
            assert_eq!(p.unwrap().normalized, Point { x: 0.5, y: 0.5 });
            for x in 0..100 {
                for y in 0..100 {
                    if let Ok(p) = map_pointer(
                        camera(),
                        panel(),
                        s,
                        Point {
                            x: f64::from(x),
                            y: f64::from(y),
                        },
                    ) {
                        assert!(p.pixel_x < s.pixels.width && p.pixel_y < s.pixels.height);
                    }
                }
            }
        }
    }

    #[test]
    fn rejects_nan_infinity_zero_size_and_invalid_zoom() {
        for zoom in [f64::NAN, f64::INFINITY, 0.0, -1.0, 21.0] {
            assert!(Camera { zoom, ..camera() }.validate().is_err());
        }
        let mut s = source();
        s.pixels.width = 0;
        assert!(s.validate().is_err());
        let mut p = panel();
        p.width = f64::INFINITY;
        assert!(p.validate().is_err());
    }

    #[test]
    fn right_and_bottom_edges_are_excluded() {
        assert!(!panel().contains(Point { x: 100.0, y: 50.0 }));
        assert!(!panel().contains(Point { x: 50.0, y: 100.0 }));
    }
}
