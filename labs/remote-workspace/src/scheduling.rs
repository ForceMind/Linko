//! Deterministic admission planning, not a hardware encoder or frame-rate claim.
//! Duplicate views of one surface share one decode allocation. The consumer must
//! negotiate the requested profile; encoded bitrate needs runtime feedback.
use std::collections::BTreeMap;

use crate::geometry::{PixelSize, Rect};
use crate::session::SessionGate;
use crate::{Error, SurfaceKey};

pub const MAX_VIEWS: usize = 256;

pub struct ViewDemand<'a> {
    pub gate: &'a SessionGate,
    pub pixels: PixelSize,
    pub canvas_rect: Rect,
    pub focused: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Profile {
    pub max_edge: u32,
    pub fps: u16,
    pub bitrate: u64,
}

impl Profile {
    fn validate(self) -> Result<(), Error> {
        if self.max_edge == 0
            || self.max_edge > 16_384
            || self.fps == 0
            || self.fps > 120
            || self.bitrate == 0
            || self.bitrate > 1_000_000_000
        {
            return Err(Error::Capacity);
        }
        Ok(())
    }

    fn allocation(self, source: PixelSize) -> Allocation {
        let edge = source.width.max(source.height);
        let target = edge.min(self.max_edge);
        let size = PixelSize {
            width: ((u64::from(source.width) * u64::from(target)) / u64::from(edge)).max(1) as u32,
            height: ((u64::from(source.height) * u64::from(target)) / u64::from(edge)).max(1)
                as u32,
        };
        Allocation {
            size,
            fps: self.fps,
            bitrate: self.bitrate,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub decoders: usize,
    /// Reference allowance for three RGBA buffers per stream, not total driver
    /// or codec GPU memory. Native adapters must account for their overhead.
    pub decoded_bytes: u64,
    pub pixels_per_second: u64,
    pub bitrate: u64,
    pub focused: Profile,
    pub preview: Profile,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            decoders: 8,
            decoded_bytes: 128 * 1024 * 1024,
            pixels_per_second: 150_000_000,
            bitrate: 32_000_000,
            focused: Profile {
                max_edge: 1920,
                fps: 60,
                bitrate: 16_000_000,
            },
            preview: Profile {
                max_edge: 480,
                fps: 5,
                bitrate: 1_000_000,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Allocation {
    pub size: PixelSize,
    pub fps: u16,
    pub bitrate: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Usage {
    pub decoders: usize,
    pub decoded_bytes: u64,
    pub pixels_per_second: u64,
    pub bitrate: u64,
}

impl Usage {
    fn adding(self, allocation: Allocation) -> Self {
        let pixels = u64::from(allocation.size.width) * u64::from(allocation.size.height);
        // Bounded to 256 views, 16K dimensions, 120fps, and 1Gbps per profile.
        Self {
            decoders: self.decoders + 1,
            decoded_bytes: self.decoded_bytes + pixels * 4 * 3,
            pixels_per_second: self.pixels_per_second + pixels * u64::from(allocation.fps),
            bitrate: self.bitrate + allocation.bitrate,
        }
    }

    fn fits(self, budget: Budget) -> bool {
        self.decoders <= budget.decoders
            && self.decoded_bytes <= budget.decoded_bytes
            && self.pixels_per_second <= budget.pixels_per_second
            && self.bitrate <= budget.bitrate
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PauseReason {
    Unauthorized,
    Offscreen,
    ResourceLimit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Decision {
    Stream(Allocation),
    Pause(PauseReason),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlannedSurface {
    pub key: SurfaceKey,
    pub decision: Decision,
}

pub struct Plan {
    pub surfaces: Vec<PlannedSurface>,
    pub usage: Usage,
}

struct Candidate {
    key: SurfaceKey,
    pixels: PixelSize,
    revision: u64,
    view_epoch: u64,
    allowed: bool,
    visible: bool,
    focused: bool,
}

/// Recompute after viewport, consent, session or budget changes. A plan is a
/// preference snapshot; recheck authorization before each capture/submission.
pub fn plan(
    views: &[ViewDemand<'_>],
    viewport: Rect,
    budget: Budget,
    now: u64,
) -> Result<Plan, Error> {
    viewport.validate()?;
    budget.focused.validate()?;
    budget.preview.validate()?;
    if views.len() > MAX_VIEWS || budget.decoders > 64 {
        return Err(Error::Capacity);
    }
    let mut candidates = BTreeMap::<SurfaceKey, Candidate>::new();
    for view in views {
        view.pixels.validate()?;
        view.canvas_rect.validate()?;
        let key = view.gate.key();
        let visible = viewport.intersects(view.canvas_rect);
        let allowed = view.gate.can_view(now).is_ok();
        if let Some(existing) = candidates.get_mut(&key) {
            if existing.pixels != view.pixels
                || existing.revision != view.gate.revision()
                || existing.view_epoch != view.gate.view_epoch()
            {
                return Err(Error::StaleTarget);
            }
            existing.allowed &= allowed;
            existing.visible |= visible;
            existing.focused |= visible && view.focused;
        } else {
            candidates.insert(
                key,
                Candidate {
                    key,
                    pixels: view.pixels,
                    revision: view.gate.revision(),
                    view_epoch: view.gate.view_epoch(),
                    allowed,
                    visible,
                    focused: visible && view.focused,
                },
            );
        }
    }
    let mut candidates: Vec<_> = candidates.into_values().collect();
    candidates.sort_by_key(|c| (!c.allowed, !c.visible, !c.focused, c.key));
    let mut usage = Usage::default();
    let mut surfaces = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let decision = if !candidate.allowed {
            Decision::Pause(PauseReason::Unauthorized)
        } else if !candidate.visible {
            Decision::Pause(PauseReason::Offscreen)
        } else {
            let requested = if candidate.focused {
                budget.focused
            } else {
                budget.preview
            };
            let mut allocation = requested.allocation(candidate.pixels);
            if !usage.adding(allocation).fits(budget) && candidate.focused {
                allocation = budget.preview.allocation(candidate.pixels);
            }
            let next = usage.adding(allocation);
            if next.fits(budget) {
                usage = next;
                Decision::Stream(allocation)
            } else {
                Decision::Pause(PauseReason::ResourceLimit)
            }
        };
        surfaces.push(PlannedSurface {
            key: candidate.key,
            decision,
        });
    }
    Ok(Plan { surfaces, usage })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture_key;
    use crate::session::SurfaceKind;

    fn gate(seed: u8) -> SessionGate {
        let mut g = SessionGate::new(fixture_key(seed), SurfaceKind::Display, 1, false).unwrap();
        g.grant_view(0, 100).unwrap();
        g
    }

    fn rect() -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 1000.0,
        }
    }

    fn demand(gate: &SessionGate, focused: bool) -> ViewDemand<'_> {
        ViewDemand {
            gate,
            pixels: PixelSize {
                width: 1920,
                height: 1080,
            },
            canvas_rect: rect(),
            focused,
        }
    }

    #[test]
    fn focused_surface_has_priority_over_insertion_order() {
        let a = gate(1);
        let b = gate(2);
        let budget = Budget {
            decoders: 1,
            ..Budget::default()
        };
        let result = plan(&[demand(&a, false), demand(&b, true)], rect(), budget, 1).unwrap();
        assert_eq!(result.surfaces[0].key, b.key());
        assert!(matches!(result.surfaces[0].decision, Decision::Stream(_)));
        assert_eq!(result.usage.decoders, 1);
    }

    #[test]
    fn duplicate_views_share_one_decoder() {
        let a = gate(1);
        let result = plan(
            &[demand(&a, false), demand(&a, true)],
            rect(),
            Budget::default(),
            1,
        )
        .unwrap();
        assert_eq!(result.surfaces.len(), 1);
        assert_eq!(result.usage.decoders, 1);
    }

    #[test]
    fn offscreen_and_expired_surfaces_consume_nothing() {
        let a = gate(1);
        let mut hidden = demand(&a, true);
        hidden.canvas_rect.x = 2000.0;
        let result = plan(&[hidden], rect(), Budget::default(), 1).unwrap();
        assert_eq!(result.usage, Usage::default());
        assert_eq!(
            result.surfaces[0].decision,
            Decision::Pause(PauseReason::Offscreen)
        );
        let result = plan(&[demand(&a, true)], rect(), Budget::default(), 100).unwrap();
        assert_eq!(result.usage, Usage::default());
        assert_eq!(
            result.surfaces[0].decision,
            Decision::Pause(PauseReason::Unauthorized)
        );
    }

    #[test]
    fn budget_limits_and_preview_fallback_are_respected() {
        let a = gate(1);
        let budget = Budget {
            decoded_bytes: 2_000_000,
            bitrate: 1_000_000,
            ..Budget::default()
        };
        let result = plan(&[demand(&a, true)], rect(), budget, 1).unwrap();
        assert_eq!(result.usage.bitrate, 1_000_000);
        assert!(result.usage.decoded_bytes <= budget.decoded_bytes);
        let budget = Budget {
            decoders: 0,
            ..budget
        };
        let result = plan(&[demand(&a, true)], rect(), budget, 1).unwrap();
        assert_eq!(result.usage, Usage::default());
    }

    #[test]
    fn contradictory_geometry_for_same_surface_rejects() {
        let a = gate(1);
        let mut changed = demand(&a, true);
        changed.pixels.width = 100;
        assert!(matches!(
            plan(&[demand(&a, false), changed], rect(), Budget::default(), 1),
            Err(Error::StaleTarget)
        ));
    }

    #[test]
    fn changing_input_order_does_not_change_admission() {
        let a = gate(1);
        let b = gate(2);
        let forward = plan(
            &[demand(&a, false), demand(&b, false)],
            rect(),
            Budget::default(),
            1,
        );
        let backward = plan(
            &[demand(&b, false), demand(&a, false)],
            rect(),
            Budget::default(),
            1,
        );
        assert_eq!(forward.unwrap().surfaces, backward.unwrap().surfaces);
    }
}
