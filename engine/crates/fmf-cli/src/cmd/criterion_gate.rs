//! `criterion-gate` — turn criterion change reports into an exit code
//! (criterion itself never sets one on regressions; ADR-0013).

use super::term;

/// Collect `<bench>/change/estimates.json` paths under criterion's output dir.
fn collect_change_reports(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_change_reports(&p, out);
        } else if p.file_name().is_some_and(|f| f == "estimates.json")
            && p.parent()
                .and_then(|d| d.file_name())
                .is_some_and(|d| d == "change")
        {
            out.push(p);
        }
    }
}

pub fn criterion_gate(
    dir: &std::path::Path,
    threshold: f64,
    ctx: super::ctx::Ctx,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut reports = Vec::new();
    collect_change_reports(dir, &mut reports);
    if reports.is_empty() {
        return Err(format!(
            "no criterion change reports under {} — run `just bench-micro-baseline` first, \
             then `cargo bench -p fmf-core -- --baseline committed`",
            dir.display()
        )
        .into());
    }

    let mut checked = 0u32;
    let mut regressions = Vec::new();
    for path in &reports {
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        let Some(median) = v["median"]["point_estimate"].as_f64() else {
            continue;
        };
        checked += 1;
        // Bench id = the path between the criterion dir and /change/.
        let name = path
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.strip_prefix(dir).ok())
            .map_or_else(
                || path.display().to_string(),
                |p| p.display().to_string().replace('\\', "/"),
            );
        if median > threshold {
            if ctx.human_chrome() {
                anstream::eprintln!(
                    "{} {name} median {:+.1}%",
                    term::paint(term::ERROR, "REGRESSION"),
                    median * 100.0
                );
            }
            regressions.push((name, median));
        }
    }

    if ctx.is_json() {
        let regressions_json: Vec<_> = regressions
            .iter()
            .map(|(name, median)| serde_json::json!({ "bench": name, "median": median }))
            .collect();
        super::json::emit(&serde_json::json!({
            "checked": checked,
            "threshold": threshold,
            "regressions": regressions_json,
            "passed": regressions.is_empty(),
        }))?;
    } else {
        println!(
            "criterion-gate: {checked} benches compared, threshold {:+.0}%",
            threshold * 100.0
        );
    }
    if !regressions.is_empty() {
        return Err("micro-benchmark regression vs criterion baseline".into());
    }
    Ok(())
}
