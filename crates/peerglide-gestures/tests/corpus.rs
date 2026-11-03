//! Synthetic regression corpus only. This is NOT the 5-person G1 acceptance set.
use peerglide_gestures::{starter_templates, Config, GestureId, Point, Recognizer, Sample};
use serde::Deserialize;
use std::time::Duration;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Corpus {
    schema_version: u16,
    provenance: String,
    cases: Vec<Case>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    name: String,
    expected: Option<u32>,
    points: Vec<[f64; 2]>,
}

fn samples(points: impl IntoIterator<Item = Point>) -> Vec<Sample> {
    points
        .into_iter()
        .enumerate()
        .map(|(i, point)| Sample {
            point,
            elapsed: Duration::from_millis(i as u64 * 5),
        })
        .collect()
}

#[test]
fn labelled_synthetic_paths_keep_expected_direction_and_rejections() {
    let corpus: Corpus = serde_json::from_str(include_str!("fixtures/trajectories.json")).unwrap();
    assert_eq!(corpus.schema_version, 1);
    assert_eq!(
        corpus.provenance,
        "hand-authored synthetic regression paths; no human recordings"
    );
    let r = Recognizer::new(Config::default(), starter_templates()).unwrap();
    let mut positive = 0;
    let mut negative = 0;
    for case in &corpus.cases {
        let trace = samples(case.points.iter().map(|p| Point::new(p[0], p[1])));
        let observed = r.recognize(&trace).ok().map(|m| m.id.0);
        assert_eq!(observed, case.expected, "{}", case.name);
        if case.expected.is_some() {
            positive += 1;
        } else {
            negative += 1;
        }
    }
    println!("synthetic_fixture: positive={positive}, negative={negative}, wrong_ids=0, false_matches=0; human_acceptance=false");
}

// Fixed seed, wrapping integer operations: reproducible on every platform.
struct Random(u64);
impl Random {
    fn unit(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 32) as u32 as f64 / u32::MAX as f64
    }
    fn noise(&mut self) -> f64 {
        self.unit() * 2.0 - 1.0
    }
}

#[test]
fn fixed_seed_synthetic_regression_metrics() {
    let r = Recognizer::new(Config::default(), starter_templates()).unwrap();
    let mut random = Random(0x5047_2026_1103);
    let mut correct = 0;
    let mut wrong = 0;
    let mut rejected_positive = 0;
    for template in starter_templates() {
        for _ in 0..64 {
            let scale = 0.6 + random.unit() * 3.0;
            let offset = Point::new(random.noise() * 10_000.0, random.noise() * 10_000.0);
            let count = 12 + (random.unit() * 80.) as usize;
            let mut points = Vec::new();
            for i in 0..count {
                let t =
                    (i as f64 / (count - 1) as f64).powf(1.3) * (template.points.len() - 1) as f64;
                let segment = (t.floor() as usize).min(template.points.len() - 2);
                let f = t - segment as f64;
                let a = template.points[segment];
                let b = template.points[segment + 1];
                points.push(Point::new(
                    offset.x + scale * (a.x + (b.x - a.x) * f + random.noise() * 0.7),
                    offset.y + scale * (a.y + (b.y - a.y) * f + random.noise() * 0.7),
                ));
            }
            match r.recognize(&samples(points)) {
                Ok(m) if m.id == template.id => correct += 1,
                Ok(_) => wrong += 1,
                Err(_) => rejected_positive += 1,
            }
        }
    }
    let mut false_matches = 0;
    let mut negative_count = 0;
    for i in 0..512 {
        let points: Vec<_> = if i < 256 {
            (0..30)
                .map(|_| Point::new(50. + random.noise() * 3., 50. + random.noise() * 3.))
                .collect()
        } else if i < 384 {
            let radius = 50. + random.unit() * 100.;
            (0..65)
                .map(|j| {
                    let angle = j as f64 / 64. * std::f64::consts::TAU;
                    Point::new(radius * angle.cos(), radius * angle.sin())
                })
                .collect()
        } else {
            let slope = if i % 2 == 0 { 1. } else { -1. };
            (0..40)
                .map(|j| Point::new(j as f64 * 3., slope * j as f64 * 3.))
                .collect()
        };
        if r.recognize(&samples(points)).is_ok() {
            false_matches += 1;
        }
        negative_count += 1;
    }
    println!(
        "{}",
        serde_json::json!({"dataset":"synthetic_regression_v1", "seed":"0x504720261103",
        "positive":512,"correct":correct,"wrong_ids":wrong,"rejected_positive":rejected_positive,
        "negative":negative_count,"false_matches":false_matches,"human_acceptance":false})
    );
    assert_eq!((correct, wrong, rejected_positive), (512, 0, 0));
    assert_eq!((negative_count, false_matches), (512, 0));
}

#[test]
fn raw_right_drag_can_match_and_must_be_arbitrated_outside_the_recognizer() {
    let r = Recognizer::new(Config::default(), starter_templates()).unwrap();
    // No classifier can infer user intent from this geometry alone. G03 must
    // select the context and trigger before recognition; there is no action here.
    let ordinary_drag = samples([Point::new(0., 0.), Point::new(100., 0.)]);
    assert_eq!(r.recognize(&ordinary_drag).unwrap().id, GestureId(2));
}
