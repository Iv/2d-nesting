//! Раскладка лекал на ткань (2D strip packing) поверх sparrow / jagua-rs.
//!
//! Вся логика здесь; CLI (и будущий GUI) только собирают параметры и вызывают [`run`].

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

/// Итог полного прогона.
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

/// Читает папку с деталями, раскладывает, проверяет и пишет `sheet_N.dxf` и `report.txt` в `out_dir`.
pub fn run(input: &Path, out_dir: &Path, import: &dxf_in::ImportOptions, params: &NestParams) -> Result<JobOutput> {
    let demands = manifest::load_job(input, import)?;
    let outcome = solver::nest(&demands, params)?;
    let validations: Vec<Validation> = outcome
        .result
        .sheets
        .iter()
        .map(|s| validate::validate_sheet(&demands, s, params.gap))
        .collect();

    fs::create_dir_all(out_dir).with_context(|| format!("{}: не удалось создать папку", out_dir.display()))?;
    let mut sheet_files = Vec::new();
    for (i, sheet) in outcome.result.sheets.iter().enumerate() {
        let path = out_dir.join(format!("sheet_{}.dxf", i + 1));
        dxf_out::write_sheet(&path, &demands, sheet)?;
        sheet_files.push(path);
    }
    let report = report::render(&demands, params, &outcome, &validations);
    let report_path = out_dir.join("report.txt");
    fs::write(&report_path, &report).with_context(|| format!("{}: не удалось записать", report_path.display()))?;

    Ok(JobOutput { demands, outcome, validations, report, sheet_files })
}
