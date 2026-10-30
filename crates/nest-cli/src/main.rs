use anyhow::{bail, Result};
use clap::{Args, Parser, Subcommand};
use nest_core::dxf_in::ImportOptions;
use nest_core::solver::{Budget, NestParams};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Nesting of pattern pieces on fabric (2D strip packing), DXF in and out.
#[derive(Parser, Debug)]
#[command(name = "nest", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,

    /// Verbose output (repeat for solver debug output)
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Nest parts: a folder of DXF files (one part per file) -> sheet DXF and report
    Run(RunArgs),
    /// Save blocks of an assembly DXF as separate part files
    Extract(ExtractArgs),
    /// Save loose labelled parts (e.g. panels "A16" under a "TOP" heading) as separate part files
    ExtractLabelled(ExtractLabelledArgs),
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum StrategyArg {
    /// Deal parts over all sheets up front, then repair and fill
    Balanced,
    /// Fill one sheet at a time from a strip of all remaining parts
    Greedy,
}

#[derive(Args, Debug)]
struct ExtractArgs {
    /// Assembly DXF with every part in its own block
    input: PathBuf,

    /// Comma-separated block name patterns (* and ?), e.g. "rib*"
    #[arg(short, long, value_delimiter = ',', required = true)]
    blocks: Vec<String>,

    /// Output folder for the part files and parts.toml
    #[arg(short, long)]
    out: PathBuf,
}

#[derive(Args, Debug)]
struct ExtractLabelledArgs {
    /// Assembly DXF with loose parts, each holding a label like "A16"
    input: PathBuf,

    /// Heading texts and the section names they start, e.g. "TOP=top,BOT=bottom"
    #[arg(long, value_delimiter = ',', required = true)]
    headings: Vec<String>,

    /// Only labels inside this box: x0,y0,x1,y1 in drawing units
    #[arg(long, value_delimiter = ',', allow_negative_numbers = true)]
    region: Option<Vec<f64>>,

    /// Output folder; parts go to <out>/<group>/
    #[arg(short, long)]
    out: PathBuf,

    /// Read only these DXF layers (comma-separated)
    #[arg(long, value_delimiter = ',')]
    layers: Option<Vec<String>>,

    /// Largest gap bridged when closing part outlines, mm
    #[arg(long, default_value_t = 1.0)]
    close_gaps: f64,
}

#[derive(Args, Debug)]
struct RunArgs {
    /// Folder with part DXF files and an optional parts.toml / parts.csv manifest
    input: PathBuf,

    /// Output folder (sheet_N.dxf, report.txt, steps/); default runs/<input name>_<date>_<time>
    #[arg(short, long)]
    out: Option<PathBuf>,

    /// Sheet width, mm (across the fabric)
    #[arg(long, default_value_t = 1560.0)]
    width: f64,

    /// Sheet length, mm (along the fabric); parts go to as many sheets as needed
    #[arg(long, default_value_t = 5800.0)]
    length: f64,

    /// Gap between parts, mm
    #[arg(short, long, default_value_t = 0.0)]
    gap: f64,

    /// Comma-separated allowed rotation angles, degrees ("0" disables rotation)
    #[arg(short, long, default_value = "0,90,180,270", value_delimiter = ',')]
    rotations: Vec<f64>,

    /// Rotation tolerance around each base angle, ± degrees (e.g. 10)
    #[arg(long, default_value_t = 0.0)]
    rotation_tolerance: f64,

    /// Rotation step within the tolerance, degrees
    #[arg(long, default_value_t = 1.0)]
    rotation_step: f64,

    /// How parts are distributed over sheets
    #[arg(long, value_enum, default_value_t = StrategyArg::Balanced)]
    strategy: StrategyArg,

    /// Search time per sheet, seconds
    #[arg(short, long, default_value_t = 60.0, conflicts_with = "iterations")]
    time_limit: f64,

    /// Fixed iteration budget per sheet instead of time: the same seed gives the same layout
    #[arg(short, long)]
    iterations: Option<usize>,

    /// Random seed
    #[arg(short, long)]
    seed: Option<u64>,

    /// Solver worker threads (keep it fixed for reproducibility)
    #[arg(short, long, default_value_t = 3)]
    workers: usize,

    /// Arc and spline approximation tolerance, mm
    #[arg(long, default_value_t = 0.2)]
    tolerance: f64,

    /// Tolerance for joining segment ends into contours, mm
    #[arg(long, default_value_t = 0.05)]
    join_tolerance: f64,

    /// Read only these DXF layers (comma-separated)
    #[arg(long, value_delimiter = ',')]
    layers: Option<Vec<String>>,

    /// Skip these DXF layers (comma-separated)
    #[arg(long, value_delimiter = ',')]
    ignore_layers: Vec<String>,

    /// Largest gap bridged when assembling the outer contour from pieces, mm
    #[arg(long, default_value_t = 1.0)]
    close_gaps: f64,

    /// Also write part names as text on layer LABELS in the DXF
    #[arg(long)]
    labels: bool,

    /// Treat any open line as an error, even inside a part
    #[arg(long)]
    strict_open: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let level = match cli.verbose {
        0 => "warn,nest_core=info",
        1 => "info",
        _ => "debug",
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level))
        .format_timestamp(None)
        .init();
    let result = match cli.cmd {
        Cmd::Run(args) => run(args),
        Cmd::Extract(args) => extract(args).map(|()| true),
        Cmd::ExtractLabelled(args) => extract_labelled(args).map(|()| true),
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(2),
        Err(e) => {
            eprintln!("Error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn extract(args: ExtractArgs) -> Result<()> {
    let blocks = nest_core::extract::extract_blocks(&args.input, &args.blocks, &args.out)?;
    for b in &blocks {
        println!("{:<24} inserts in drawing: {:>2}  -> {}", b.name, b.inserts, b.path.display());
    }
    println!("Saved {} parts. Set quantities in {}", blocks.len(), args.out.join("parts.toml").display());
    Ok(())
}

fn extract_labelled(args: ExtractLabelledArgs) -> Result<()> {
    let headings = args
        .headings
        .iter()
        .map(|h| match h.split_once('=') {
            Some((t, s)) => Ok((t.trim().to_string(), s.trim().to_string())),
            None => bail!("--headings expects TEXT=section, got {h:?}"),
        })
        .collect::<Result<Vec<_>>>()?;
    let opts = ImportOptions { layers: args.layers, close_gaps: args.close_gaps, ..Default::default() };
    if args.region.as_ref().is_some_and(|r| r.len() != 4) {
        bail!("--region expects x0,y0,x1,y1");
    }
    let region = args.region.as_ref().map(|r| nest_core::geom::BBox {
        min: nest_core::geom::Pt::new(r[0].min(r[2]), r[1].min(r[3])),
        max: nest_core::geom::Pt::new(r[0].max(r[2]), r[1].max(r[3])),
    });
    let parts = nest_core::extract::extract_labelled(&args.input, &headings, region, &opts, &args.out)?;
    let mut groups: std::collections::BTreeMap<&str, Vec<&str>> = std::collections::BTreeMap::new();
    for p in &parts {
        groups.entry(p.group.as_str()).or_default().push(p.name.as_str());
    }
    for (g, names) in &groups {
        println!("{g}: {} parts: {}", names.len(), names.join(" "));
    }
    println!("Saved {} parts under {}", parts.len(), args.out.display());
    Ok(())
}

fn run(cli: RunArgs) -> Result<bool> {
    if !(cli.time_limit > 0.0) {
        bail!("--time-limit must be positive");
    }
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let c = cancel.clone();
        ctrlc::set_handler(move || {
            eprintln!("interrupted: moving to the next phase / stopping");
            c.store(true, Ordering::SeqCst);
        })?;
    }
    let import = ImportOptions {
        tolerance: cli.tolerance,
        join_tolerance: cli.join_tolerance,
        layers: cli.layers,
        ignore_layers: cli.ignore_layers,
        allow_open_inside: !cli.strict_open,
        close_gaps: cli.close_gaps,
        ..Default::default()
    };
    let params = NestParams {
        width: cli.width,
        length: cli.length,
        gap: cli.gap,
        rotations: cli.rotations,
        rotation_tolerance: cli.rotation_tolerance,
        rotation_step: cli.rotation_step,
        budget: match cli.iterations {
            Some(n) => Budget::Iterations(n),
            None => Budget::Time(Duration::from_secs_f64(cli.time_limit)),
        },
        strategy: match cli.strategy {
            StrategyArg::Balanced => nest_core::solver::Strategy::Balanced,
            StrategyArg::Greedy => nest_core::solver::Strategy::Greedy,
        },
        seed: cli.seed,
        workers: cli.workers,
        cancel: Some(cancel),
    };

    let out_dir = cli.out.clone().unwrap_or_else(|| {
        let name = cli.input.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "job".into());
        PathBuf::from("runs").join(format!("{name}_{}", jiff::Zoned::now().strftime("%Y-%m-%d_%H%M%S")))
    });
    std::fs::create_dir_all(&out_dir)?;
    // Keep the exact command next to the results, to compare runs later.
    let command: Vec<String> = std::env::args().collect();
    std::fs::write(out_dir.join("command.txt"), command.join(" ") + "\n")?;

    let dxf_opts = nest_core::dxf_out::DxfOptions { labels: cli.labels };
    let out = nest_core::run(&cli.input, &out_dir, &import, &params, dxf_opts)?;
    println!("{}", out.report);
    for f in &out.sheet_files {
        println!("Wrote {}", f.display());
    }
    println!("Wrote {}", out_dir.join("report.txt").display());
    println!("Overview: {}", out_dir.join("index.html").display());
    if !out.valid() {
        eprintln!("VALIDATION ERROR: the layout has violations, see the report");
    }
    Ok(out.valid())
}
