//! Quantity manifest and loading of a parts folder.
//!
//! `parts.toml`:
//! ```toml
//! [parts]
//! front = 2
//! sleeve = { qty = 4 }
//! ```
//! `parts.csv`: rows `name,qty`; the first row may be a header.

use crate::dxf_in::{read_part, ImportOptions};
use crate::geom::Demand;
use anyhow::{bail, Context, Result};
use log::warn;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

pub const MANIFEST_NAMES: [&str; 2] = ["parts.toml", "parts.csv"];

#[derive(Deserialize)]
struct TomlManifest {
    parts: BTreeMap<String, TomlQty>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TomlQty {
    Plain(usize),
    Table { qty: usize },
}

/// Quantity by part name.
pub type Quantities = BTreeMap<String, usize>;

pub fn read_manifest(path: &Path) -> Result<Quantities> {
    let text = fs::read_to_string(path).with_context(|| format!("{}: cannot read", path.display()))?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "toml" => parse_toml(&text),
        "csv" => parse_csv(&text),
        _ => bail!("unknown manifest format (expected .toml or .csv)"),
    }
    .with_context(|| format!("{}", path.display()))
}

fn parse_toml(text: &str) -> Result<Quantities> {
    let m: TomlManifest = toml::from_str(text)?;
    Ok(m.parts
        .into_iter()
        .map(|(k, v)| {
            let q = match v {
                TomlQty::Plain(q) | TomlQty::Table { qty: q } => q,
            };
            (k, q)
        })
        .collect())
}

fn parse_csv(text: &str) -> Result<Quantities> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(false)
        .trim(csv::Trim::All)
        .flexible(true)
        .comment(Some(b'#'))
        .from_reader(text.as_bytes());
    let mut out = Quantities::new();
    for (i, rec) in rdr.records().enumerate() {
        let rec = rec?;
        if rec.iter().all(str::is_empty) {
            continue;
        }
        let (name, qty) = (rec.get(0).unwrap_or(""), rec.get(1).unwrap_or(""));
        match qty.parse::<usize>() {
            Ok(q) => {
                if out.insert(name.to_string(), q).is_some() {
                    bail!("line {}: part \"{name}\" is listed twice", i + 1);
                }
            }
            Err(_) if i == 0 => {} // header
            Err(_) => bail!("line {}: quantity \"{qty}\" is not a number", i + 1),
        }
    }
    Ok(out)
}

/// All DXF files of a folder, sorted by name.
pub fn dxf_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("{}: cannot open directory", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dxf")))
        .collect();
    files.sort();
    Ok(files)
}

/// Reads a folder: parts plus quantities from the manifest, if any. Errors of all files are reported together.
pub fn load_job(dir: &Path, opts: &ImportOptions) -> Result<Vec<Demand>> {
    let manifest_path = MANIFEST_NAMES.iter().map(|n| dir.join(n)).find(|p| p.is_file());
    let mut qtys = match &manifest_path {
        Some(p) => Some(read_manifest(p)?),
        None => None,
    };
    let files = dxf_files(dir)?;
    if files.is_empty() {
        bail!("{}: no DXF files", dir.display());
    }
    let mut demands = Vec::new();
    let mut errors = Vec::new();
    for f in files {
        match read_part(&f, opts) {
            Ok(part) => {
                let qty = match qtys.as_mut() {
                    None => 1,
                    Some(q) => q.remove(&part.name).unwrap_or_else(|| {
                        warn!("part \"{}\" is not in the manifest, quantity = 1", part.name);
                        1
                    }),
                };
                if qty > 0 {
                    demands.push(Demand { part, qty });
                }
            }
            Err(e) => {
                // The file exists but failed to read: do not also report it as missing from the manifest.
                if let (Some(q), Some(stem)) = (qtys.as_mut(), f.file_stem()) {
                    q.remove(stem.to_string_lossy().as_ref());
                }
                errors.push(format!("{e:#}"));
            }
        }
    }
    if let Some(q) = qtys
        && !q.is_empty()
    {
        errors.push(format!(
            "manifest lists parts without a DXF file: {}",
            q.keys().cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    if !errors.is_empty() {
        bail!("input errors:\n  {}", errors.join("\n  "));
    }
    Ok(demands)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_both_forms() {
        let q = parse_toml("[parts]\nfront = 2\nsleeve = { qty = 4 }\n").unwrap();
        assert_eq!(q["front"], 2);
        assert_eq!(q["sleeve"], 4);
    }

    #[test]
    fn csv_with_header_and_comments() {
        let q = parse_csv("name,qty\n# comment\nfront, 2\nback,1\n").unwrap();
        assert_eq!(q.len(), 2);
        assert_eq!(q["front"], 2);
        assert!(parse_csv("a,1\nb,x\n").is_err());
    }
}
