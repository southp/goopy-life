use clap::{Parser, Subcommand};
use gl_core::goopy_provisioner::hello_provisioner::HelloProvisioner;
use gl_core::goopy_registry::sqlite_registry::SqliteRegistry;
use gl_core::sys_utils::{DryRunSysRunner, RealSysRunner};
use gl_core::*;
use indicatif::{MultiProgress, ProgressBar};
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "Goopy-Life CLI")]
#[command(version = "0.1")]
#[command(about = "Mainly a quick playground for now. Would it become a real CLI tool? Who knows.", long_about = None)]
struct Cli {
    /// Path to the config file
    #[arg(long, default_value = "./config.toml")]
    config: std::path::PathBuf,

    /// Use production mode (default: dev mode)
    ///
    /// Without this flag the CLI always operates in dev mode regardless of
    /// what `dev_mode` is set to in config.toml.
    #[arg(long)]
    prod: bool,

    /// Print privileged commands without executing them
    #[arg(long)]
    dry_run: bool,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Spawn one or more goopies
    Spawn {
        /// Number of instances to spawn
        #[arg(default_value = "1")]
        count: u32,
    },
    /// Despawn one or more goopies
    Despawn {
        #[arg(num_args = 1.., required = true)]
        slugs: Vec<String>,
    },
    /// List all the available goopies
    List {},
    /// Allocate storage at the given path using the configured allocator
    Alloc {
        #[arg(long)]
        path: std::path::PathBuf,
    },
    /// Release storage at the given path using the configured allocator
    Dealloc {
        #[arg(long)]
        path: std::path::PathBuf,
    },
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    // Config file is required — no silent fallback.
    if !cli.config.exists() {
        eprintln!(
            "error: config file not found: {}\n\
             Copy config.toml.example and adjust it to get started.",
            cli.config.display()
        );
        std::process::exit(1);
    }

    let cfg = match gl_core::Config::from_file(&cli.config) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("error: failed to load config from {}: {e}", cli.config.display());
            std::process::exit(1);
        }
    };

    // Only HelloProvisioner is implemented in this CLI; Ghost is still a stub.
    if cfg.provisioner_kind != ProvisionerKind::Hello {
        eprintln!(
            "error: unsupported provisioner_kind '{}'. Only 'Hello' is supported by this CLI.",
            cfg.provisioner_kind
        );
        std::process::exit(1);
    }

    // --prod overrides config; absent → dev mode (safe default).
    let dev_mode = !cli.prod;

    tracing::info!(
        config = %cli.config.display(),
        db = %cfg.registry.path.display(),
        base_dir = %cfg.base_dir.display(),
        domain = %cfg.domain,
        life_in_days = cfg.life_in_days,
        port_range_start = cfg.port_range_start,
        port_range_end = cfg.port_range_end,
        dev_mode,
        dry_run = cli.dry_run,
        "configuration loaded"
    );

    let storage: Arc<dyn StorageAllocator> = if dev_mode {
        Arc::new(PlainDirAllocator)
    } else {
        Arc::new(ZfsAllocator::new(cfg.allocator.pool, cfg.allocator.quota_mb))
    };

    let sys: Arc<dyn SysRunner> = if cli.dry_run {
        Arc::new(DryRunSysRunner)
    } else {
        Arc::new(RealSysRunner)
    };

    let provisioner = HelloProvisioner::new(
        cfg.domain.clone(),
        dev_mode,
        Arc::clone(&storage),
        sys,
    );

    let registry = SqliteRegistry::new(&cfg.registry.path)
        .expect("failed to open SQLite registry");

    let mut gm = GoopyManager::new(
        cfg.base_dir,
        cfg.domain,
        cfg.ssl_email,
        cfg.life_in_days,
        cfg.port_range_start,
        cfg.port_range_end,
        registry,
        provisioner,
    );
    let mp = MultiProgress::new();
    let mut spinners = vec![];
    let mut jobs = vec![];

    match cli.command {
        Cmd::Spawn { count } => {
            for _ in 0..count {
                let spinner = mp.add(ProgressBar::new_spinner());
                spinner.set_message("Spawning ...".to_string());
                spinner.enable_steady_tick(Duration::from_millis(100));

                match gm.spawn() {
                    Ok((slug, port, job_id)) => {
                        spinner.set_message(format!("Spawning {slug} (port {port}) ..."));
                        jobs.push(job_id);
                    }
                    Err(e) => {
                        println!("Spawn failed: {:?}", e);
                        spinner.finish_with_message(format!("Failed due to: {:?}", e));
                    }
                }
                spinners.push(spinner);
            }
        }
        Cmd::Despawn { slugs } => {
            for s in slugs.iter() {
                let spinner = mp.add(ProgressBar::new_spinner());
                spinner.set_message(format!("Despawning {} ...", s));
                spinner.enable_steady_tick(Duration::from_millis(100));

                match gm.despawn(s.to_string()) {
                    Ok(job_id) => jobs.push(job_id),
                    Err(e) => {
                        println!("Despawn failed: {:?}", e);
                        std::process::exit(1);
                    }
                }
                spinners.push(spinner);
            }
        }
        Cmd::Alloc { path } => {
            match storage.allocate(&path) {
                Ok(()) => println!("allocated: {}", path.display()),
                Err(e) => {
                    eprintln!("alloc failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        Cmd::Dealloc { path } => {
            match storage.release(&path) {
                Ok(()) => println!("released: {}", path.display()),
                Err(e) => {
                    eprintln!("dealloc failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        Cmd::List {} => {
            match gm.list() {
                Ok(goopies) => {
                    for gp in goopies {
                        println!("{:?}", gp);
                    }
                }
                Err(e) => {
                    println!("List failed: {:?}", e);
                    std::process::exit(1);
                }
            }
        }
    }

    while jobs.iter().map(|job_id| gm.is_job_finished(job_id).unwrap()).any(|s| !s) {
        std::thread::sleep(Duration::from_secs(1));
    }

    spinners.iter().for_each(|s| s.finish_with_message(format!("{} done!", s.message())));
}
