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
    let outcome = solver::nest(&demands, params)?;
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
        sheet_files.push(path);
    }
    let report = report::render(&demands, params, &outcome, &validations);
    let report_path = out_dir.join("report.txt");
    fs::write(&report_path, &report).with_context(|| format!("{}: cannot write", report_path.display()))?;

    Ok(JobOutput { demands, outcome, validations, report, sheet_files })
}
