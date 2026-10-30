//! Nesting of pattern pieces on fabric (2D strip packing) on top of sparrow / jagua-rs.
//!
//! All logic lives here; the CLI (and a future GUI) only collect parameters and call [`run`].

pub mod dxf_in;
pub mod dxf_out;
pub mod extract;
pub mod geom;
pub mod manifest;
pub mod report;
pub mod solver;
pub mod validate;

use anyhow::{Context, Result};
use geom::Demand;
use solver::{NestOutcome, NestParams};
use std::fs;
use std::path::{Path, PathBuf};
use validate::Validation;

/// Result of a full run.
pub struct JobOutput {
    pub demands: Vec<Demand>,
    pub outcome: NestOutcome,
    pub validations: Vec<Validation>,
    pub report: String,
    pub sheet_files: Vec<PathBuf>,
}

impl JobOutput {
    pub fn valid(&self) -> bool {
        self.validations.iter().all(Validation::ok)
    }
}

/// Reads a folder of parts, nests, validates and writes `sheet_N.dxf` and `report.txt` to `out_dir`.
pub fn run(input: &Path, out_dir: &Path, import: &dxf_in::ImportOptions, params: &NestParams) -> Result<JobOutput> {
    let demands = manifest::load_job(input, import)?;
    // Intermediate layouts go to `steps/` so a run can be looked through afterwards.
    let steps_dir = out_dir.join("steps");
    fs::create_dir_all(&steps_dir).with_context(|| format!("{}: cannot create directory", steps_dir.display()))?;
    let mut n_step = 0;
    let mut step_error = None;
    let outcome = solver::nest_observed(&demands, params, &mut |st| {
        n_step += 1;
        let kind: String = st.kind.chars().map(|c| if c.is_alphanumeric() || c == '+' { c } else { '_' }).collect();
        let name = format!("{n_step:03}_sheet{}_{kind}_{}.svg", st.sheet, if st.fits { "fits" } else { "long" });
        let view = geom::Sheet {
            width: params.width,
            length: st.used_length.max(params.length),
            used_length: st.used_length,
            placements: st.placements.clone(),
        };
        if let Err(e) = dxf_out::write_svg_with_limit(&steps_dir.join(name), &demands, &view, Some(params.length)) {
            step_error.get_or_insert(e);
        }
    })?;
    if let Some(e) = step_error {
        return Err(e);
    }
    let validations: Vec<Validation> = outcome
        .result
        .sheets
        .iter()
        .map(|s| validate::validate_sheet(&demands, s, params.gap))
        .collect();

    fs::create_dir_all(out_dir).with_context(|| format!("{}: cannot create directory", out_dir.display()))?;
    let mut sheet_files = Vec::new();
    for (i, sheet) in outcome.result.sheets.iter().enumerate() {
        let path = out_dir.join(format!("sheet_{}.dxf", i + 1));
        dxf_out::write_sheet(&path, &demands, sheet)?;
        dxf_out::write_svg(&path.with_extension("svg"), &demands, sheet)?;
        sheet_files.push(path);
    }
    write_overview(&out_dir.join("index.html"), &outcome, &validations)?;
    let report = report::render(&demands, params, &outcome, &validations);
    let report_path = out_dir.join("report.txt");
    fs::write(&report_path, &report).with_context(|| format!("{}: cannot write", report_path.display()))?;

    Ok(JobOutput { demands, outcome, validations, report, sheet_files })
}

/// A page showing all sheet previews one under another, to look through a whole job at once.
fn write_overview(path: &Path, outcome: &NestOutcome, validations: &[Validation]) -> Result<()> {
    use std::fmt::Write as _;
    let mut h = String::from(
        "<!doctype html><meta charset='utf-8'><title>Nesting</title>\
         <style>body{font-family:sans-serif;margin:16px;background:#f4f4f4}.sheet{background:#fff;border:1px solid #999;line-height:0}\
         h2{font-size:15px;margin:18px 0 6px}</style>",
    );
    for (i, s) in outcome.result.sheets.iter().enumerate() {
        let ok = validations.get(i).is_some_and(Validation::ok);
        let _ = write!(
            h,
            "<h2>Sheet {} — {} parts, used {:.0} of {:.0} mm{}</h2><div class='sheet'>{}</div>",
            i + 1,
            s.placements.len(),
            s.used_length,
            s.length,
            if ok { "" } else { " — VALIDATION FAILED" },
            // Inline the preview so the page is a single self-contained file.
            fs::read_to_string(path.with_file_name(format!("sheet_{}.svg", i + 1)))
                .unwrap_or_default()
                .replacen("<svg ", "<svg style='width:100%;height:auto' ", 1)
        );
    }
    fs::write(path, h).with_context(|| format!("{}: cannot write", path.display()))
}
