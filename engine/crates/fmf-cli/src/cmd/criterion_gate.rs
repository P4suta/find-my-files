//! `criterion-gate` — turn criterion change reports into an exit code
//! (criterion itself never sets one on regressions; ADR-0013).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::term;

/// Single machine-readable report contract shared with xtask's transactional
/// baseline validation. A renamed/added benchmark changes this manifest first;
/// the small exclusion list below is the only policy layered on top.
const BENCHMARK_MANIFEST: &str = include_str!("../../../../benches/criterion-benchmarks.txt");

/// Measured and required to be present, but deliberately excluded from the
/// relative gate. Their synthetic layout noise is documented at the benchmark
/// definitions; the real-volume absolute gate is their arbiter.
const INFORMATIONAL_BENCHES: &[&str] = &["build/finish_1m", "query/regex_scan"];

#[derive(Clone, Copy, Debug, PartialEq)]
struct MedianEstimate {
    point: f64,
    lower: f64,
    upper: f64,
}

fn expected_report_ids() -> BTreeSet<&'static str> {
    BENCHMARK_MANIFEST
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

fn gated_report_ids() -> BTreeSet<&'static str> {
    let informational: BTreeSet<_> = INFORMATIONAL_BENCHES.iter().copied().collect();
    expected_report_ids()
        .difference(&informational)
        .copied()
        .collect()
}

fn validate_report_ids(actual: &BTreeSet<String>) -> Result<(), String> {
    let expected = expected_report_ids();
    let missing: Vec<_> = expected
        .iter()
        .filter(|id| !actual.contains(**id))
        .copied()
        .collect();
    let unexpected: Vec<_> = actual
        .iter()
        .filter(|id| !expected.contains(id.as_str()))
        .map(String::as_str)
        .collect();

    if missing.is_empty() && unexpected.is_empty() {
        return Ok(());
    }

    Err(format!(
        "criterion report set does not match the benchmark contract; \
         missing=[{}]; unexpected/stale=[{}]. Run the complete \
         `just bench-micro-check` suite and remove stale Criterion output",
        missing.join(", "),
        unexpected.join(", ")
    ))
}

fn collect_change_reports(
    root: &Path,
    dir: &Path,
    out: &mut BTreeMap<String, PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("failed to read Criterion directory {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| {
            format!(
                "failed to enumerate Criterion directory {}: {e}",
                dir.display()
            )
        })?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| format!("failed to inspect Criterion path {}: {e}", path.display()))?;
        if file_type.is_dir() {
            collect_change_reports(root, &path, out)?;
            continue;
        }
        if !file_type.is_file()
            || path.file_name().is_none_or(|name| name != "estimates.json")
            || path
                .parent()
                .and_then(Path::file_name)
                .is_none_or(|name| name != "change")
        {
            continue;
        }

        let bench_dir = path
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| format!("malformed Criterion report path {}", path.display()))?;
        let id = bench_dir
            .strip_prefix(root)
            .map_err(|_| {
                format!(
                    "Criterion report {} escaped root {}",
                    path.display(),
                    root.display()
                )
            })?
            .display()
            .to_string()
            .replace('\\', "/");
        if out.insert(id.clone(), path.clone()).is_some() {
            return Err(format!("duplicate Criterion change report for benchmark `{id}`").into());
        }
    }
    Ok(())
}

fn parse_median_estimate(value: &serde_json::Value) -> Result<MedianEstimate, String> {
    let median = value
        .get("median")
        .ok_or_else(|| "missing `median` estimate".to_owned())?;
    let point = median
        .get("point_estimate")
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| "missing numeric `median.point_estimate`".to_owned())?;
    let interval = median
        .get("confidence_interval")
        .ok_or_else(|| "missing `median.confidence_interval`".to_owned())?;
    let confidence = interval
        .get("confidence_level")
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| {
            "missing numeric `median.confidence_interval.confidence_level`".to_owned()
        })?;
    let lower = interval
        .get("lower_bound")
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| "missing numeric `median.confidence_interval.lower_bound`".to_owned())?;
    let upper = interval
        .get("upper_bound")
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| "missing numeric `median.confidence_interval.upper_bound`".to_owned())?;

    if (confidence - 0.95).abs() > f64::EPSILON {
        return Err(format!(
            "median confidence level is {confidence}, expected 0.95"
        ));
    }
    if !point.is_finite() || !lower.is_finite() || !upper.is_finite() {
        return Err("median estimate contains a non-finite value".to_owned());
    }
    if lower > point || point > upper {
        return Err(format!(
            "invalid median confidence interval: {lower} <= {point} <= {upper} is false"
        ));
    }

    Ok(MedianEstimate {
        point,
        lower,
        upper,
    })
}

fn is_regression(estimate: MedianEstimate, threshold: f64) -> bool {
    estimate.lower > threshold
}

pub fn criterion_gate(
    dir: &Path,
    threshold: f64,
    ctx: super::ctx::Ctx,
) -> Result<(), Box<dyn std::error::Error>> {
    if !threshold.is_finite() || threshold < 0.0 {
        return Err(format!(
            "criterion regression threshold must be a finite non-negative ratio, got {threshold}"
        )
        .into());
    }

    let mut reports = BTreeMap::new();
    collect_change_reports(dir, dir, &mut reports)?;
    validate_report_ids(&reports.keys().cloned().collect())?;

    let gated = gated_report_ids();
    let mut checked = 0u32;
    let mut regressions = Vec::new();
    for (name, path) in &reports {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read Criterion report {}: {e}", path.display()))?;
        let value: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!("invalid Criterion JSON in {}: {e}", path.display()))?;
        let median = parse_median_estimate(&value)
            .map_err(|e| format!("invalid Criterion report for `{name}`: {e}"))?;

        if !gated.contains(name.as_str()) {
            continue;
        }
        checked += 1;
        // A noisy point estimate is not a regression. The whole 95% median
        // interval must clear the effect-size threshold.
        if is_regression(median, threshold) {
            if ctx.human_chrome() {
                anstream::eprintln!(
                    "{} {name} median {:+.1}% (95% CI {:+.1}%..{:+.1}%)",
                    term::paint(term::ERROR, "REGRESSION"),
                    median.point * 100.0,
                    median.lower * 100.0,
                    median.upper * 100.0
                );
            }
            regressions.push((name, median));
        }
    }

    if ctx.is_json() {
        let regressions_json: Vec<_> = regressions
            .iter()
            .map(|(name, median)| {
                serde_json::json!({
                    "bench": name,
                    "median": median.point,
                    "median_ci_lower": median.lower,
                    "median_ci_upper": median.upper,
                })
            })
            .collect();
        super::json::emit(&serde_json::json!({
            "expected_reports": reports.len(),
            "checked": checked,
            "informational": INFORMATIONAL_BENCHES.len(),
            "threshold": threshold,
            "confidence_level": 0.95,
            "regressions": regressions_json,
            "passed": regressions.is_empty(),
        }))?;
    } else {
        println!(
            "criterion-gate: {checked} gated benches compared, {} informational reports \
             verified, threshold {:+.0}% (95% CI lower bound)",
            INFORMATIONAL_BENCHES.len(),
            threshold * 100.0
        );
    }
    if !regressions.is_empty() {
        return Err("micro-benchmark regression vs criterion baseline".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn estimate(point: f64, lower: f64, upper: f64) -> serde_json::Value {
        serde_json::json!({
            "median": {
                "confidence_interval": {
                    "confidence_level": 0.95,
                    "lower_bound": lower,
                    "upper_bound": upper,
                },
                "point_estimate": point,
            }
        })
    }

    #[test]
    fn benchmark_contract_is_unique_and_classifies_informational_cases() {
        let manifest_line_count = BENCHMARK_MANIFEST
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .count();
        let expected = expected_report_ids();
        let gated = gated_report_ids();
        let informational: BTreeSet<_> = INFORMATIONAL_BENCHES.iter().copied().collect();
        assert_eq!(expected.len(), manifest_line_count, "duplicate manifest id");
        assert_eq!(
            informational.len(),
            INFORMATIONAL_BENCHES.len(),
            "duplicate informational id"
        );
        assert!(informational.is_subset(&expected));
        assert!(gated.is_disjoint(&informational));
        assert_eq!(gated.len(), 26);
        assert_eq!(expected.len(), 28);
        assert!(!gated.contains("query/regex_scan"));
        assert!(!gated.contains("build/finish_1m"));
    }

    #[test]
    fn report_set_must_match_exactly() {
        let exact: BTreeSet<String> = expected_report_ids()
            .into_iter()
            .map(str::to_owned)
            .collect();
        assert!(validate_report_ids(&exact).is_ok());

        let mut missing = exact.clone();
        missing.remove("query/common");
        let error = validate_report_ids(&missing).unwrap_err();
        assert!(error.contains("missing=[query/common]"), "{error}");

        let mut stale = exact;
        stale.insert("query/removed_benchmark".to_owned());
        let error = validate_report_ids(&stale).unwrap_err();
        assert!(
            error.contains("unexpected/stale=[query/removed_benchmark]"),
            "{error}"
        );
    }

    #[test]
    fn median_ci_lower_bound_drives_the_regression_verdict() {
        let noisy = parse_median_estimate(&estimate(0.50, 0.09, 0.80)).unwrap();
        assert!(noisy.point > 0.10);
        assert!(
            !is_regression(noisy, 0.10),
            "point alone must not fail the gate"
        );

        let boundary = parse_median_estimate(&estimate(0.20, 0.10, 0.30)).unwrap();
        assert!(
            !is_regression(boundary, 0.10),
            "the threshold comparison is strictly greater"
        );

        let regression = parse_median_estimate(&estimate(0.20, 0.100_001, 0.30)).unwrap();
        assert!(is_regression(regression, 0.10));
    }

    #[test]
    fn median_estimate_requires_a_well_formed_95_percent_interval() {
        let mut wrong_confidence = estimate(0.20, 0.10, 0.30);
        wrong_confidence["median"]["confidence_interval"]["confidence_level"] =
            serde_json::json!(0.90);
        assert!(
            parse_median_estimate(&wrong_confidence)
                .unwrap_err()
                .contains("expected 0.95")
        );

        assert!(parse_median_estimate(&estimate(0.20, 0.30, 0.40)).is_err());
        assert!(parse_median_estimate(&serde_json::json!({})).is_err());
    }
}
