use anyhow::{bail, Result};
use clap::{Args, Parser, Subcommand};
use nest_core::dxf_in::ImportOptions;
use nest_core::solver::{Budget, NestParams};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Раскладка лекал на ткань (2D strip packing), вход и выход DXF.
#[derive(Parser, Debug)]
#[command(name = "nest", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,

    /// Подробный вывод (повторите для отладки солвера)
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Разложить детали: папка DXF (одна деталь = один файл) → DXF отреза и отчёт
    Run(RunArgs),
    /// Сохранить блоки сборочного DXF в отдельные файлы-детали
    Extract(ExtractArgs),
}

#[derive(Args, Debug)]
struct ExtractArgs {
    /// Сборочный DXF, где каждая деталь в своём блоке
    input: PathBuf,

    /// Шаблоны имён блоков через запятую (* и ?), например "rib*"
    #[arg(short, long, value_delimiter = ',', required = true)]
    blocks: Vec<String>,

    /// Папка для файлов деталей и parts.toml
    #[arg(short, long)]
    out: PathBuf,
}

#[derive(Args, Debug)]
struct RunArgs {
    /// Папка с DXF-деталями и необязательным манифестом parts.toml / parts.csv
    input: PathBuf,

    /// Папка для результатов (sheet_N.dxf, report.txt)
    #[arg(short, long, default_value = "out")]
    out: PathBuf,

    /// Ширина ткани, мм
    #[arg(long, default_value_t = 1560.0)]
    width: f64,

    /// Максимальная длина отреза, мм (при превышении — предупреждение)
    #[arg(long, default_value_t = 5800.0)]
    max_length: f64,

    /// Зазор между деталями, мм
    #[arg(short, long, default_value_t = 0.0)]
    gap: f64,

    /// Разрешённые углы поворота через запятую, градусы ("0" — без поворотов)
    #[arg(short, long, default_value = "0,180", value_delimiter = ',')]
    rotations: Vec<f64>,

    /// Время поиска, секунды
    #[arg(short, long, default_value_t = 60.0, conflicts_with = "iterations")]
    time_limit: f64,

    /// Вместо времени — фиксированное число итераций: с тем же seed результат повторяется
    #[arg(short, long)]
    iterations: Option<usize>,

    /// Seed генератора случайных чисел
    #[arg(short, long)]
    seed: Option<u64>,

    /// Число потоков солвера (для воспроизводимости держите одинаковым)
    #[arg(short, long, default_value_t = 3)]
    workers: usize,

    /// Допуск аппроксимации дуг и сплайнов, мм
    #[arg(long, default_value_t = 0.2)]
    tolerance: f64,

    /// Допуск сшивки концов отрезков в контур, мм
    #[arg(long, default_value_t = 0.05)]
    join_tolerance: f64,

    /// Читать только эти слои DXF (через запятую)
    #[arg(long, value_delimiter = ',')]
    layers: Option<Vec<String>>,

    /// Пропускать эти слои DXF (через запятую)
    #[arg(long, value_delimiter = ',')]
    ignore_layers: Vec<String>,

    /// Максимальный разрыв, который закрывается при сборке внешнего контура из кусков, мм
    #[arg(long, default_value_t = 1.0)]
    close_gaps: f64,

    /// Считать ошибкой любую незамкнутую линию, даже внутри детали
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
            eprintln!("Ошибка: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn extract(args: ExtractArgs) -> Result<()> {
    let blocks = nest_core::extract::extract_blocks(&args.input, &args.blocks, &args.out)?;
    for b in &blocks {
        println!("{:<24} вставок в чертеже: {:>2}  → {}", b.name, b.inserts, b.path.display());
    }
    println!("Сохранено деталей: {}. Количества задайте в {}", blocks.len(), args.out.join("parts.toml").display());
    Ok(())
}

fn run(cli: RunArgs) -> Result<bool> {
    if !(cli.time_limit > 0.0) {
        bail!("--time-limit должен быть больше нуля");
    }
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let c = cancel.clone();
        ctrlc::set_handler(move || {
            eprintln!("прерывание: перехожу к следующей фазе / завершаю");
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
        println!("Записан {}", f.display());
    }
    println!("Записан {}", cli.out.join("report.txt").display());
    for (i, s) in out.outcome.result.sheets.iter().enumerate() {
        if s.length > params.max_length {
            eprintln!(
                "ВНИМАНИЕ: отрез {} длиной {:.1} мм больше максимальной {:.0} мм",
                i + 1,
                s.length,
                params.max_length
            );
        }
    }
    if !out.valid() {
        eprintln!("ОШИБКА ПРОВЕРКИ: раскладка содержит нарушения, см. отчёт");
    }
    Ok(out.valid())
}
