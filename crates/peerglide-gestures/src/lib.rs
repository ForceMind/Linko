//! Bounded, direction-preserving single-stroke recognition. No hooks or actions.
//!
//! Coordinates are logical pixels, x right and y down, in one frozen coordinate
//! space. DPI conversion, input ownership and cancellation belong to the caller.
//! Matching a trajectory does not authorize or execute anything. Synthetic tests
//! are regression evidence, not human false-trigger or physical mouse acceptance.

use std::{collections::BTreeMap, time::Duration};

pub const MAX_SAMPLES: usize = 2048;
pub const MAX_TEMPLATES: usize = 64;
const RESAMPLED: usize = 32;
const MAX_COORDINATE: f64 = 1_000_000.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}
impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
    fn valid(self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.x.abs() <= MAX_COORDINATE
            && self.y.abs() <= MAX_COORDINATE
    }
    fn distance(self, other: Self) -> f64 {
        (self.x - other.x).hypot(self.y - other.y)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub point: Point,
    /// Offset from stroke start using the caller's local monotonic clock.
    pub elapsed: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GestureId(pub u32);

/// Raw template paths. Multiple templates may share an ID; ambiguity is between
/// distinct IDs, not two examples of the same gesture.
#[derive(Clone, Debug)]
pub struct Template {
    pub id: GestureId,
    pub points: Vec<Point>,
}

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub jitter: f64,
    pub min_extent: f64,
    pub min_length: f64,
    pub max_length: f64,
    pub max_duration: Duration,
    /// Mean distance in normalized units, not a probability/confidence score.
    pub max_distance: f64,
    /// Required distance advantage over the next distinct ID.
    pub min_margin: f64,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            jitter: 2.0,
            min_extent: 24.0,
            min_length: 40.0,
            max_length: 20_000.0,
            max_duration: Duration::from_millis(2500),
            max_distance: 0.12,
            min_margin: 0.04,
        }
    }
}
impl Config {
    fn valid(self) -> bool {
        [
            self.jitter,
            self.min_extent,
            self.min_length,
            self.max_length,
            self.max_distance,
            self.min_margin,
        ]
        .iter()
        .all(|v| v.is_finite())
            && self.jitter > 0.0
            && self.jitter < self.min_extent
            && self.min_extent > 0.0
            && self.min_length >= self.min_extent
            && self.max_length > self.min_length
            && self.max_length <= 1_000_000.0
            && self.max_duration > Duration::ZERO
            && self.max_duration <= Duration::from_secs(10)
            && self.max_distance > 0.0
            && self.max_distance < 0.5
            && self.min_margin > 0.0
            && self.min_margin < 0.5
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildError {
    InvalidConfig,
    TemplateCount,
    InvalidTemplate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    SampleCount,
    InvalidCoordinate,
    NonMonotonicTime,
    Duration,
    TooShort,
    TooLong,
    NoMatch,
    Ambiguous,
    Cancelled,
}

#[derive(Clone, Copy, Debug)]
pub struct Match {
    pub id: GestureId,
    pub distance: f64,
    pub runner_up_distance: Option<f64>,
}

struct CompiledTemplate {
    id: GestureId,
    points: [Point; RESAMPLED],
}
pub struct Recognizer {
    config: Config,
    templates: Vec<CompiledTemplate>,
}

/// One-shot, bounded worker-side collector. Overflow, invalid input or explicit
/// cancellation poisons the whole stroke: a valid prefix cannot become an action.
/// Construct outside native callbacks; this is not an input ownership broker.
pub struct Stroke<'a> {
    recognizer: &'a Recognizer,
    samples: Vec<Sample>,
    length: f64,
    failure: Option<Rejection>,
}
impl Stroke<'_> {
    pub fn push(&mut self, sample: Sample) -> Result<(), Rejection> {
        if let Some(failure) = self.failure {
            return Err(failure);
        }
        let previous = self.samples.last();
        let length = self.length + previous.map_or(0.0, |p| p.point.distance(sample.point));
        let failure = if self.samples.len() == MAX_SAMPLES {
            Some(Rejection::SampleCount)
        } else if !sample.point.valid() {
            Some(Rejection::InvalidCoordinate)
        } else if previous.is_some_and(|p| sample.elapsed < p.elapsed) {
            Some(Rejection::NonMonotonicTime)
        } else if sample.elapsed >= self.recognizer.config.max_duration {
            Some(Rejection::Duration)
        } else if length > self.recognizer.config.max_length {
            Some(Rejection::TooLong)
        } else {
            None
        };
        if let Some(failure) = failure {
            self.failure = Some(failure);
            return Err(failure);
        }
        self.samples.push(sample);
        self.length = length;
        Ok(())
    }
    pub fn cancel(&mut self) {
        if self.failure.is_none() {
            self.failure = Some(Rejection::Cancelled);
        }
    }
    /// Release/cancellation clock is supplied even if no movement arrived lately.
    pub fn finish(self, elapsed: Duration) -> Result<Match, Rejection> {
        if let Some(failure) = self.failure {
            return Err(failure);
        }
        if self.samples.last().is_some_and(|p| elapsed < p.elapsed) {
            return Err(Rejection::NonMonotonicTime);
        }
        if elapsed >= self.recognizer.config.max_duration {
            return Err(Rejection::Duration);
        }
        self.recognizer.recognize(&self.samples)
    }
}

impl Recognizer {
    /// Bounded compiled vocabulary, not a rule or action grant.
    pub fn template_ids(&self) -> impl Iterator<Item = GestureId> + '_ {
        self.templates.iter().map(|template| template.id)
    }

    pub fn config(&self) -> Config {
        self.config
    }
    pub fn stroke(&self) -> Stroke<'_> {
        Stroke {
            recognizer: self,
            samples: Vec::with_capacity(MAX_SAMPLES),
            length: 0.0,
            failure: None,
        }
    }
    pub fn new(config: Config, templates: Vec<Template>) -> Result<Self, BuildError> {
        if !config.valid() {
            return Err(BuildError::InvalidConfig);
        }
        if templates.is_empty() || templates.len() > MAX_TEMPLATES {
            return Err(BuildError::TemplateCount);
        }
        let mut compiled = Vec::with_capacity(templates.len());
        for template in templates {
            if template.points.len() < 2
                || template.points.len() > MAX_SAMPLES
                || template.points.iter().any(|p| !p.valid())
            {
                return Err(BuildError::InvalidTemplate);
            }
            let points = normalize(&template.points).ok_or(BuildError::InvalidTemplate)?;
            compiled.push(CompiledTemplate {
                id: template.id,
                points,
            });
        }
        Ok(Self {
            config,
            templates: compiled,
        })
    }

    /// Bounded preprocessing, 32 comparisons per template and bounded ranking; no I/O
    /// and no early partial match.
    /// The entire slice is validated before jitter filtering. Rejection never
    /// returns a guessed ID or an action to execute.
    pub fn recognize(&self, samples: &[Sample]) -> Result<Match, Rejection> {
        if samples.len() < 2 || samples.len() > MAX_SAMPLES {
            return Err(Rejection::SampleCount);
        }
        let mut previous_time = Duration::ZERO;
        for sample in samples {
            if !sample.point.valid() {
                return Err(Rejection::InvalidCoordinate);
            }
            if sample.elapsed < previous_time {
                return Err(Rejection::NonMonotonicTime);
            }
            if sample.elapsed >= self.config.max_duration {
                return Err(Rejection::Duration);
            }
            previous_time = sample.elapsed;
        }
        let raw_length: f64 = samples
            .windows(2)
            .map(|pair| pair[0].point.distance(pair[1].point))
            .sum();
        if raw_length > self.config.max_length {
            return Err(Rejection::TooLong);
        }
        let mut filtered = Vec::with_capacity(samples.len());
        filtered.push(samples[0].point);
        for sample in &samples[1..] {
            if filtered.last().unwrap().distance(sample.point) >= self.config.jitter {
                filtered.push(sample.point);
            }
        }
        let last = samples.last().unwrap().point;
        if filtered.last().unwrap().distance(last) > f64::EPSILON {
            filtered.push(last);
        }
        let length = path_length(&filtered);
        if length > self.config.max_length {
            return Err(Rejection::TooLong);
        }
        if length < self.config.min_length || extent(&filtered) < self.config.min_extent {
            return Err(Rejection::TooShort);
        }
        let points = normalize(&filtered).ok_or(Rejection::TooShort)?;
        let mut best_by_id = BTreeMap::<GestureId, f64>::new();
        for template in &self.templates {
            let distance = points
                .iter()
                .zip(template.points.iter())
                .map(|(a, b)| a.distance(*b))
                .sum::<f64>()
                / RESAMPLED as f64;
            best_by_id
                .entry(template.id)
                .and_modify(|best| *best = best.min(distance))
                .or_insert(distance);
        }
        let mut ranked: Vec<_> = best_by_id.into_iter().collect();
        ranked.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        let (id, distance) = ranked[0];
        if distance > self.config.max_distance {
            return Err(Rejection::NoMatch);
        }
        let runner_up_distance = ranked.get(1).map(|(_, d)| *d);
        if runner_up_distance.is_some_and(|d| d - distance < self.config.min_margin) {
            return Err(Rejection::Ambiguous);
        }
        Ok(Match {
            id,
            distance,
            runner_up_distance,
        })
    }
}

fn path_length(points: &[Point]) -> f64 {
    points
        .windows(2)
        .map(|pair| pair[0].distance(pair[1]))
        .sum()
}
fn bounds(points: &[Point]) -> (Point, Point) {
    let mut lo = points[0];
    let mut hi = lo;
    for p in points {
        lo.x = lo.x.min(p.x);
        lo.y = lo.y.min(p.y);
        hi.x = hi.x.max(p.x);
        hi.y = hi.y.max(p.y);
    }
    (lo, hi)
}
fn extent(points: &[Point]) -> f64 {
    let (lo, hi) = bounds(points);
    (hi.x - lo.x).max(hi.y - lo.y)
}
/// Uniform scale + translation only: no rotation, reflection, axis stretching,
/// path reversal, corner reordering or cyclic starting-point search.
fn normalize(points: &[Point]) -> Option<[Point; RESAMPLED]> {
    let length = path_length(points);
    let scale = extent(points);
    if length <= f64::EPSILON || scale <= f64::EPSILON {
        return None;
    }
    let (lo, hi) = bounds(points);
    let center = Point::new((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0);
    let mut result = [Point::new(0.0, 0.0); RESAMPLED];
    let mut segment = 1;
    let mut traversed = 0.0;
    for (i, output) in result.iter_mut().enumerate() {
        let target = length * i as f64 / (RESAMPLED - 1) as f64;
        while segment < points.len() - 1 {
            let distance = points[segment - 1].distance(points[segment]);
            if traversed + distance >= target && distance > f64::EPSILON {
                break;
            }
            traversed += distance;
            segment += 1;
        }
        let a = points[segment - 1];
        let b = points[segment];
        let distance = a.distance(b);
        let fraction = if distance > f64::EPSILON {
            ((target - traversed) / distance).clamp(0.0, 1.0)
        } else {
            1.0
        };
        *output = Point::new(
            (a.x + (b.x - a.x) * fraction - center.x) / scale,
            (a.y + (b.y - a.y) * fraction - center.y) / scale,
        );
    }
    Some(result)
}

/// Starter vocabulary, not enabled bindings. IDs 1..8 are stable in this module.
pub fn starter_templates() -> Vec<Template> {
    let paths: &[&[(f64, f64)]] = &[
        &[(0., 0.), (-100., 0.)],
        &[(0., 0.), (100., 0.)],
        &[(0., 0.), (0., -100.)],
        &[(0., 0.), (0., 100.)],
        &[(0., 0.), (0., 100.), (100., 100.)],
        &[(0., 0.), (0., 100.), (-100., 100.)],
        &[(0., 0.), (0., -100.), (100., -100.)],
        &[(0., 0.), (0., -100.), (-100., -100.)],
    ];
    paths
        .iter()
        .enumerate()
        .map(|(i, path)| Template {
            id: GestureId(i as u32 + 1),
            points: path.iter().map(|&(x, y)| Point::new(x, y)).collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(points: &[Point]) -> Vec<Sample> {
        points
            .iter()
            .enumerate()
            .map(|(i, &point)| Sample {
                point,
                elapsed: Duration::from_millis(i as u64 * 10),
            })
            .collect()
    }
    fn recognizer() -> Recognizer {
        Recognizer::new(Config::default(), starter_templates()).unwrap()
    }

    #[test]
    fn one_shot_strokes_cannot_recognize_a_prefix_after_failure_or_cancellation() {
        let r = recognizer();
        let valid = samples(&[Point::new(0., 0.), Point::new(100., 0.)]);
        let prepared = || {
            let mut stroke = r.stroke();
            for &sample in &valid {
                stroke.push(sample).unwrap();
            }
            stroke
        };
        assert_eq!(
            prepared().finish(Duration::from_millis(20)).unwrap().id,
            GestureId(2)
        );
        let mut cancelled = prepared();
        cancelled.cancel();
        assert_eq!(cancelled.push(valid[1]), Err(Rejection::Cancelled));
        assert_eq!(
            cancelled.finish(Duration::from_millis(20)).unwrap_err(),
            Rejection::Cancelled
        );
        assert_eq!(
            prepared()
                .finish(Config::default().max_duration)
                .unwrap_err(),
            Rejection::Duration
        );
        assert_eq!(
            prepared().finish(Duration::ZERO).unwrap_err(),
            Rejection::NonMonotonicTime
        );
        let mut invalid = prepared();
        assert_eq!(
            invalid.push(Sample {
                point: Point::new(f64::NAN, 0.),
                elapsed: Duration::from_millis(20)
            }),
            Err(Rejection::InvalidCoordinate)
        );
        assert_eq!(invalid.push(valid[1]), Err(Rejection::InvalidCoordinate));
        assert_eq!(
            invalid.finish(Duration::from_millis(20)).unwrap_err(),
            Rejection::InvalidCoordinate
        );
        let mut overflow = r.stroke();
        for i in 0..MAX_SAMPLES {
            overflow
                .push(Sample {
                    point: Point::new(i as f64 * 100. / MAX_SAMPLES as f64, 0.),
                    elapsed: Duration::from_micros(i as u64 * 100),
                })
                .unwrap();
        }
        assert_eq!(overflow.push(valid[1]), Err(Rejection::SampleCount));
        assert_eq!(
            overflow.finish(Duration::from_millis(500)).unwrap_err(),
            Rejection::SampleCount
        );
    }

    #[test]
    fn orientation_and_stroke_order_survive_normalization() {
        let recognizer = recognizer();
        for template in starter_templates() {
            assert_eq!(
                recognizer.recognize(&samples(&template.points)).unwrap().id,
                template.id
            );
            let transformed: Vec<_> = template
                .points
                .iter()
                .map(|p| Point::new(p.x * 3.5 - 4000., p.y * 3.5 + 2000.))
                .collect();
            assert_eq!(
                recognizer.recognize(&samples(&transformed)).unwrap().id,
                template.id
            );
        }
        // Same corner geometry, opposite writing order must not keep its ID.
        let corner = starter_templates().remove(4);
        let mut reversed = corner.points.clone();
        reversed.reverse();
        assert_ne!(
            recognizer.recognize(&samples(&reversed)).ok().map(|m| m.id),
            Some(corner.id)
        );
        let reflected: Vec<_> = corner
            .points
            .iter()
            .map(|p| Point::new(-p.x, p.y))
            .collect();
        assert_eq!(
            recognizer.recognize(&samples(&reflected)).unwrap().id,
            GestureId(6)
        );
        let line = [Point::new(0., 0.), Point::new(0., 100.)];
        assert_eq!(
            recognizer.recognize(&samples(&line)).unwrap().id,
            GestureId(4)
        );
    }

    #[test]
    fn sampling_rate_duplicates_and_short_terminal_segment_are_stable() {
        let r = recognizer();
        for count in [2, 50, 125, 500, 1000, MAX_SAMPLES] {
            let trace: Vec<_> = (0..count)
                .map(|i| Sample {
                    point: Point::new(i as f64 * 100. / (count - 1) as f64, 0.),
                    elapsed: Duration::from_micros(i as u64 * 1_000_000 / (count - 1) as u64),
                })
                .collect();
            assert_eq!(r.recognize(&trace).unwrap().id, GestureId(2));
        }
        let trace = samples(&[
            Point::new(0., 0.),
            Point::new(0., 0.),
            Point::new(99., 0.),
            Point::new(100., 0.),
        ]);
        assert_eq!(r.recognize(&trace).unwrap().id, GestureId(2));
    }

    #[test]
    fn invalid_tail_time_and_resource_limits_reject_without_partial_match() {
        let r = recognizer();
        let valid = samples(&[Point::new(0., 0.), Point::new(100., 0.)]);
        for invalid in [f64::NAN, f64::INFINITY, 1_000_001.] {
            let mut trace = valid.clone();
            trace.push(Sample {
                point: Point::new(invalid, 0.),
                elapsed: Duration::from_millis(20),
            });
            assert_eq!(
                r.recognize(&trace).unwrap_err(),
                Rejection::InvalidCoordinate
            );
        }
        let mut trace = valid.clone();
        trace.push(Sample {
            point: Point::new(101., 0.),
            elapsed: Duration::ZERO,
        });
        assert_eq!(
            r.recognize(&trace).unwrap_err(),
            Rejection::NonMonotonicTime
        );
        trace = valid.clone();
        trace[1].elapsed = Config::default().max_duration;
        assert_eq!(r.recognize(&trace).unwrap_err(), Rejection::Duration);
        assert_eq!(r.recognize(&[]).unwrap_err(), Rejection::SampleCount);
        assert_eq!(
            r.recognize(&vec![valid[0]; MAX_SAMPLES + 1]).unwrap_err(),
            Rejection::SampleCount
        );
        assert_eq!(
            r.recognize(&samples(&[Point::new(0., 0.), Point::new(1., 1.)]))
                .unwrap_err(),
            Rejection::TooShort
        );
        let huge = samples(&[Point::new(-10_001., 0.), Point::new(10_001., 0.)]);
        assert_eq!(r.recognize(&huge).unwrap_err(), Rejection::TooLong);
        // Tiny oscillations removed by jitter must still count toward the raw budget.
        let r = Recognizer::new(
            Config {
                max_length: 50.,
                ..Config::default()
            },
            starter_templates(),
        )
        .unwrap();
        let jitter: Vec<_> = (0..100).map(|i| Point::new((i % 2) as f64, 0.)).collect();
        assert_eq!(
            r.recognize(&samples(&jitter)).unwrap_err(),
            Rejection::TooLong
        );
    }

    #[test]
    fn ambiguity_is_between_ids_and_never_resolved_by_template_order() {
        let right = starter_templates().remove(1);
        let trace = samples(&right.points);
        let mut other = right.clone();
        other.id = GestureId(99);
        for templates in [
            vec![right.clone(), other.clone()],
            vec![other, right.clone()],
        ] {
            let r = Recognizer::new(Config::default(), templates).unwrap();
            assert_eq!(r.recognize(&trace).unwrap_err(), Rejection::Ambiguous);
        }
        let r = Recognizer::new(Config::default(), vec![right.clone(), right]).unwrap();
        let matched = r.recognize(&trace).unwrap();
        assert_eq!(matched.id, GestureId(2));
        assert_eq!(matched.runner_up_distance, None);
        // Distinct IDs with small but nonzero score separation still reject.
        let almost = Template {
            id: GestureId(99),
            points: vec![Point::new(0., 0.), Point::new(100., 1.)],
        };
        let r = Recognizer::new(
            Config::default(),
            vec![starter_templates().remove(1), almost],
        )
        .unwrap();
        assert_eq!(r.recognize(&trace).unwrap_err(), Rejection::Ambiguous);
    }

    #[test]
    fn unknown_shapes_and_unsafe_settings_are_rejected() {
        let diagonal = samples(&[Point::new(0., 0.), Point::new(100., 100.)]);
        assert_eq!(
            recognizer().recognize(&diagonal).unwrap_err(),
            Rejection::NoMatch
        );
        for config in [
            Config {
                jitter: f64::NAN,
                ..Config::default()
            },
            Config {
                min_margin: 0.,
                ..Config::default()
            },
            Config {
                max_distance: 1.,
                ..Config::default()
            },
            Config {
                max_duration: Duration::ZERO,
                ..Config::default()
            },
        ] {
            assert!(matches!(
                Recognizer::new(config, starter_templates()),
                Err(BuildError::InvalidConfig)
            ));
        }
        assert!(matches!(
            Recognizer::new(Config::default(), vec![]),
            Err(BuildError::TemplateCount)
        ));
        let template = starter_templates().remove(0);
        assert!(matches!(
            Recognizer::new(Config::default(), vec![template; MAX_TEMPLATES + 1]),
            Err(BuildError::TemplateCount)
        ));
        for points in [
            vec![],
            vec![Point::new(0., 0.); 4],
            vec![Point::new(0., 0.), Point::new(f64::NAN, 1.)],
        ] {
            assert!(matches!(
                Recognizer::new(
                    Config::default(),
                    vec![Template {
                        id: GestureId(1),
                        points
                    }]
                ),
                Err(BuildError::InvalidTemplate)
            ));
        }
    }
}
