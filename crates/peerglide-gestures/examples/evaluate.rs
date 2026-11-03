//! Offline evaluator for explicit GUI exports. Never treats canvas paths as
//! physical polling-rate or independent-human acceptance evidence.
use peerglide_gestures::{starter_templates, Config, Point, Recognizer, Sample, MAX_SAMPLES};
use serde::Deserialize;
use std::{fs::File, io::Read, path::Path, time::Duration};
const MAX_BYTES: u64 = 256 * 1024;
const MAX_FILES: usize = 10_000;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Export {
    schema_version: u16,
    source: String,
    units: String,
    expected_id: Option<u32>,
    samples: Vec<Recorded>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Recorded {
    x: f64,
    y: f64,
    elapsed_us: u64,
}
fn classify(bytes: &[u8], r: &Recognizer) -> Result<(usize, usize), String> {
    let e: Export = serde_json::from_slice(bytes).map_err(|_| "invalid sample schema")?;
    if e.schema_version != 1 || e.source != "egui_canvas" || e.units != "logical_pixels" {
        return Err("unsupported sample format/source/units".into());
    }
    let expected = e.expected_id.unwrap_or(0) as usize;
    if expected > 8
        || e.expected_id == Some(0)
        || e.samples.len() > MAX_SAMPLES
        || e.samples.len() < 2
    {
        return Err("invalid label or sample count".into());
    }
    let samples: Vec<_> = e
        .samples
        .into_iter()
        .map(|s| Sample {
            point: Point::new(s.x, s.y),
            elapsed: Duration::from_micros(s.elapsed_us),
        })
        .collect();
    // Invalid recordings must not improve rejection metrics by becoming negatives.
    if samples.iter().any(|s| {
        !s.point.x.is_finite()
            || !s.point.y.is_finite()
            || s.point.x.abs() > 1_000_000.
            || s.point.y.abs() > 1_000_000.
    }) || samples.windows(2).any(|p| p[1].elapsed < p[0].elapsed)
        || samples.last().unwrap().elapsed >= Config::default().max_duration
    {
        return Err("invalid coordinates/timing".into());
    }
    let predicted = match r.recognize(&samples) {
        Ok(m) => m.id.0 as usize,
        Err(peerglide_gestures::Rejection::TooLong) => {
            return Err("recording exceeded path budget".into())
        }
        Err(_) => 0,
    };
    Ok((expected, predicted))
}
fn evaluate(directory: &Path) -> Result<serde_json::Value, String> {
    let r = Recognizer::new(Config::default(), starter_templates()).map_err(|_| "templates")?;
    let mut files = Vec::new();
    for (i, entry) in std::fs::read_dir(directory)
        .map_err(|_| "cannot read sample folder")?
        .enumerate()
    {
        if i >= MAX_FILES {
            return Err("too many directory entries".into());
        }
        let entry = entry.map_err(|_| "cannot read sample entry")?;
        if entry.path().extension().is_some_and(|x| x == "json") {
            files.push(entry.path());
        }
    }
    files.sort();
    if files.is_empty() {
        return Err("no JSON samples".into());
    }
    let mut confusion = [[0u32; 9]; 9];
    for (index, path) in files.iter().enumerate() {
        let invalid = || {
            format!(
                "invalid sample {} (file contents/path not logged)",
                index + 1
            )
        };
        let metadata = std::fs::symlink_metadata(path).map_err(|_| invalid())?;
        if !metadata.is_file() || metadata.len() > MAX_BYTES {
            return Err(invalid());
        }
        let mut bytes = Vec::new();
        File::open(path)
            .map_err(|_| invalid())?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid());
        }
        let (expected, predicted) = classify(&bytes, &r).map_err(|_| invalid())?;
        confusion[expected][predicted] += 1;
    }
    let negative: u32 = confusion[0].iter().sum();
    let positive: u32 = confusion.iter().skip(1).flatten().sum();
    let correct: u32 = (1..9).map(|i| confusion[i][i]).sum();
    let rejected_positive: u32 = (1..9).map(|i| confusion[i][0]).sum();
    let false_matches = negative - confusion[0][0];
    Ok(
        serde_json::json!({"format":"peerglide-canvas-evaluation/1","samples":files.len(),"positive":positive,"correct":correct,
        "wrong_ids":positive-correct-rejected_positive,"rejected_positive":rejected_positive,"negative":negative,"false_matches":false_matches,
        "recognition_rate":if positive>0 {Some(correct as f64/positive as f64)}else{None},
        "false_match_rate":if negative>0 {Some(false_matches as f64/negative as f64)}else{None},
        "confusion_expected_rows_predicted_columns":confusion,"human_acceptance":false,"native_polling_acceptance":false}),
    )
}
fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let result = if args.len() == 1 {
        evaluate(Path::new(&args[0]))
    } else {
        Err("usage: cargo run -p peerglide-gestures --example evaluate -- SAMPLE_FOLDER".into())
    };
    match result {
        Ok(report) => println!("{report}"),
        Err(e) => {
            eprintln!("Evaluation failed: {e}");
            std::process::exit(1);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn record(expected: Option<u32>) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({"schema_version":1,"source":"egui_canvas","units":"logical_pixels","expected_id":expected,
        "samples":[{"x":0.,"y":0.,"elapsed_us":0},{"x":100.,"y":0.,"elapsed_us":100000}]})).unwrap()
    }
    #[test]
    fn labels_are_independent_and_invalid_data_is_not_counted_as_good_rejection() {
        let r = Recognizer::new(Config::default(), starter_templates()).unwrap();
        assert_eq!(classify(&record(Some(2)), &r).unwrap(), (2, 2));
        assert_eq!(classify(&record(None), &r).unwrap(), (0, 2));
        assert!(classify(&record(Some(0)), &r).is_err());
        assert!(classify(&record(Some(99)), &r).is_err());
        let mut value: serde_json::Value = serde_json::from_slice(&record(None)).unwrap();
        value["samples"][0]["elapsed_us"] = 100001.into();
        assert!(classify(&serde_json::to_vec(&value).unwrap(), &r).is_err());
    }
    #[test]
    fn directory_metrics_fail_on_invalid_files_and_keep_false_matches() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let mut reserved = None;
        for _ in 0..128 {
            let p = std::env::temp_dir().join(format!(
                "peerglide-eval-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&p) {
                Ok(()) => {
                    reserved = Some(p);
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("reserve evaluation fixture: {e}"),
            }
        }
        let root = reserved.expect("exclusive fixture");
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        std::fs::write(root.join("positive.json"), record(Some(2))).unwrap();
        std::fs::write(root.join("negative.json"), record(None)).unwrap();
        let result = evaluate(&root).unwrap();
        assert_eq!(result["correct"], 1);
        assert_eq!(result["false_matches"], 1);
        assert_eq!(result["confusion_expected_rows_predicted_columns"][0][2], 1);
        assert_eq!(result["human_acceptance"], false);
        std::fs::write(root.join("invalid.json"), b"broken").unwrap();
        assert!(evaluate(&root).is_err());
    }
}
