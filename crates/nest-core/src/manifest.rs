//! Quantity manifest and loading of a parts folder.
//!
//! `parts.toml`:
//! ```toml
//! [parts]
//! front = 2
//! sleeve = { qty = 4 }
//! top5 = { qty = 1, left = 1 }   # one as drawn (right) and one mirrored (left)
//! ```
//! `parts.csv`: rows `name,qty[,left]`; the first row may be a header.
//!
//! Parts are drawn as right-hand ones; `left` copies are mirrored and named `<name>_L`.

use crate::dxf_in::{read_part, ImportOptions};
use crate::geom::{Demand, Part};
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
    Table(TomlTable),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TomlTable {
    #[serde(default, alias = "right")]
    qty: usize,
    #[serde(default)]
    left: usize,
}

/// How many copies of a part are needed: as drawn (right-hand) and mirrored (left-hand).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Qty {
    pub right: usize,
    pub left: usize,
}

/// Quantity by part name.
pub type Quantities = BTreeMap<String, Qty>;

/// Suffix of the mirrored (left-hand) copy of a part.
pub const LEFT_SUFFIX: &str = "_L";

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
                TomlQty::Plain(q) => Qty { right: q, left: 0 },
                TomlQty::Table(t) => Qty { right: t.qty, left: t.left },
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
        let (name, qty, left) = (rec.get(0).unwrap_or(""), rec.get(1).unwrap_or(""), rec.get(2).unwrap_or(""));
        let left = match left {
            "" => Some(0),
            l => l.parse::<usize>().ok(),
        };
        match (qty.parse::<usize>(), left) {
            (Ok(right), Some(left)) => {
                if out.insert(name.to_string(), Qty { right, left }).is_some() {
                    bail!("line {}: part \"{name}\" is listed twice", i + 1);
                }
            }
            _ if i == 0 => {} // header
            _ => bail!("line {}: quantity \"{qty}\" or \"{}\" is not a number", i + 1, rec.get(2).unwrap_or("")),
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
                    None => Qty { right: 1, left: 0 },
                    Some(q) => q.remove(&part.name).unwrap_or_else(|| {
                        warn!("part \"{}\" is not in the manifest, quantity = 1", part.name);
                        Qty { right: 1, left: 0 }
                    }),
                };
                if qty.left > 0 {
                    let left = Part { name: format!("{}{LEFT_SUFFIX}", part.name), source: part.source.clone(), shape: part.shape.mirrored() };
                    if qty.right > 0 {
                        demands.push(Demand { part, qty: qty.right, filler: false });
                    }
                    demands.push(Demand { part: left, qty: qty.left, filler: false });
                } else if qty.right > 0 {
                    demands.push(Demand { part, qty: qty.right, filler: false });
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
    for (i, d) in demands.iter().enumerate() {
        if demands[..i].iter().any(|o| o.part.name == d.part.name) {
            errors.push(format!("part name \"{}\" is used twice (a mirrored copy is named <name>{LEFT_SUFFIX})", d.part.name));
        }
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
        assert_eq!(q["front"], Qty { right: 2, left: 0 });
        assert_eq!(q["sleeve"], Qty { right: 4, left: 0 });
    }

    #[test]
    fn toml_left_copies() {
        let q = parse_toml("[parts]\ntop5 = { qty = 1, left = 1 }\nbot5 = { right = 2, left = 3 }\nbot6 = { left = 1 }\n").unwrap();
        assert_eq!(q["top5"], Qty { right: 1, left: 1 });
        assert_eq!(q["bot5"], Qty { right: 2, left: 3 });
        assert_eq!(q["bot6"], Qty { right: 0, left: 1 });
        assert!(parse_toml("[parts]\ntop5 = { qty = 1, lefts = 1 }\n").is_err());
    }

    #[test]
    fn csv_with_header_and_comments() {
        let q = parse_csv("name,qty\n# comment\nfront, 2\nback,1\n").unwrap();
        assert_eq!(q.len(), 2);
        assert_eq!(q["front"], Qty { right: 2, left: 0 });
        let q = parse_csv("name,qty,left\ntop5,1,1\ntop6,2\n").unwrap();
        assert_eq!(q["top5"], Qty { right: 1, left: 1 });
        assert_eq!(q["top6"], Qty { right: 2, left: 0 });
        assert!(parse_csv("a,1\nb,x\n").is_err());
        assert!(parse_csv("a,1\nb,1,x\n").is_err());
    }
}
