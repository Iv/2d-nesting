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

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum StrategyArg {
    /// Deal parts over all sheets up front, then repair and fill
    Balanced,
    /// Fill one sheet at a time from a strip of all remaining parts
    Greedy,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum PanelAxis {
    /// Along the panel's long axis
    Long,
    /// Along the drawing's X axis
    Drawn,
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

    /// Folder of filler parts (e.g. small parts of another job; quantity = at most this many) that
    /// go into free space of the sheets without making them longer; may be repeated
    #[arg(long)]
    fillers: Vec<PathBuf>,

    /// Write parts to the sheet DXF as traced contours with large name labels (for checking
    /// layouts) instead of their source drawings inserted unchanged as blocks
    #[arg(long)]
    outlines: bool,

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

    /// Parts whose name contains one of these (comma-separated, case-insensitive) are panels:
    /// their deviation from a base angle is penalised
    #[arg(long, default_value = "top,bot", value_delimiter = ',')]
    panels: Vec<String>,

    /// Panel tilt penalty: a panel turned d degrees off a base angle costs this · d² mm of
    /// fabric length (0 = no penalty)
    #[arg(long, default_value_t = 10.0)]
    tilt_penalty: f64,

    /// Largest tilt of a panel off a base angle, degrees (within --rotation-tolerance)
    #[arg(long, default_value_t = 3.0)]
    panel_tolerance: f64,

    /// Straight direction of a panel: along its long axis (elongated panels are turned before
    /// nesting) or the drawing's X axis
    #[arg(long, value_enum, default_value_t = PanelAxis::Long)]
    panel_axis: PanelAxis,

    /// A free strip along the top of a sheet at least this wide (mm) is reusable and not counted
    /// as used fabric; sheets with spare length are re-nested to leave one (0 = off)
    #[arg(long, default_value_t = 150.0)]
    min_strip: f64,

    /// Fabric saved at the end of a sheet is worth this many times the same area left as a strip
    /// on top (1 = equally valuable)
    #[arg(long, default_value_t = 2.0)]
    strip_ratio: f64,

    /// How parts are distributed over sheets
    #[arg(long, value_enum, default_value_t = StrategyArg::Balanced)]
    strategy: StrategyArg,

    /// Quick drafts (each 1/10 of the time limit) that decide which parts go on which sheet; the
    /// sheets of the best one are then nested again with the full time limit. 0 = off
    #[arg(long, default_value_t = 0)]
    drafts: usize,

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

    /// How far inner geometry (marks, open lines) may stick out of the outer contour, mm
    #[arg(long, default_value_t = 2.0)]
    edge_tolerance: f64,

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
    if !(cli.strip_ratio >= 1.0) {
        bail!("--strip-ratio must be at least 1");
    }
    if !(cli.panel_tolerance >= 0.0) {
        bail!("--panel-tolerance must not be negative");
    }
    if !(cli.tilt_penalty >= 0.0) {
        bail!("--tilt-penalty must not be negative");
    }
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
        edge_tolerance: cli.edge_tolerance,
        ..Default::default()
    };
    let params = NestParams {
        width: cli.width,
        length: cli.length,
        gap: cli.gap,
        rotations: cli.rotations,
        rotation_tolerance: cli.rotation_tolerance,
        rotation_step: cli.rotation_step,
        panels: cli.panels,
        tilt_penalty: cli.tilt_penalty,
        panel_tolerance: cli.panel_tolerance,
        align_panels: cli.panel_axis == PanelAxis::Long,
        min_strip: cli.min_strip,
        strip_ratio: cli.strip_ratio,
        budget: match cli.iterations {
            Some(n) => Budget::Iterations(n),
            None => Budget::Time(Duration::from_secs_f64(cli.time_limit)),
        },
        strategy: match cli.strategy {
            StrategyArg::Balanced => nest_core::solver::Strategy::Balanced,
            StrategyArg::Greedy => nest_core::solver::Strategy::Greedy,
        },
        drafts: cli.drafts,
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

    let out = nest_core::run_with_fillers(
        &cli.input,
        &cli.fillers,
        &out_dir,
        &import,
        &params,
        &nest_core::dxf_out::OutputOptions { outlines: cli.outlines },
    )?;
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
