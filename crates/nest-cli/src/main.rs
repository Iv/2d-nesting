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
struct RunArgs {
    /// Folder with part DXF files and an optional parts.toml / parts.csv manifest
    input: PathBuf,

    /// Output folder (sheet_N.dxf, report.txt)
    #[arg(short, long, default_value = "out")]
    out: PathBuf,

    /// Fabric width, mm
    #[arg(long, default_value_t = 1560.0)]
    width: f64,

    /// Maximum sheet length, mm (exceeding it gives a warning)
    #[arg(long, default_value_t = 5800.0)]
    max_length: f64,

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

    /// Search time, seconds
    #[arg(short, long, default_value_t = 60.0, conflicts_with = "iterations")]
    time_limit: f64,

    /// Fixed iteration budget instead of time: the same seed gives the same layout
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
        max_length: cli.max_length,
        gap: cli.gap,
        rotations: cli.rotations,
        rotation_tolerance: cli.rotation_tolerance,
        rotation_step: cli.rotation_step,
        budget: match cli.iterations {
            Some(n) => Budget::Iterations(n),
            None => Budget::Time(Duration::from_secs_f64(cli.time_limit)),
        },
        seed: cli.seed,
        workers: cli.workers,
        cancel: Some(cancel),
    };

    let out = nest_core::run(&cli.input, &cli.out, &import, &params)?;
    println!("{}", out.report);
    for f in &out.sheet_files {
        println!("Wrote {}", f.display());
    }
    println!("Wrote {}", cli.out.join("report.txt").display());
    for (i, s) in out.outcome.result.sheets.iter().enumerate() {
        if s.length > params.max_length {
            eprintln!(
                "WARNING: sheet {} is {:.1} mm long, more than the maximum {:.0} mm",
                i + 1,
                s.length,
                params.max_length
            );
        }
    }
    if !out.valid() {
        eprintln!("VALIDATION ERROR: the layout has violations, see the report");
    }
    Ok(out.valid())
}
