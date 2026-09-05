use clap::{Parser, Subcommand};
use gl_core::goopy_registry::sqlite_registry::SqliteRegistry;
use gl_core::sys_utils::RealSysRunner;
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
    let span_events = match std::env::var("RUST_LOG_SPANS").as_deref() {
        Ok("0") | Ok("false") | Ok("") | Err(_) => tracing_subscriber::fmt::format::FmtSpan::NONE,
        Ok(_) => tracing_subscriber::fmt::format::FmtSpan::FULL,
    };

    tracing_subscriber::fmt()
        .with_span_events(span_events)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();

    // Config file is required — no silent fallback.
    if !cli.config.exists() {
        tracing::error!(
            path = %cli.config.display(),
            "config file not found; try --config config.local.toml from backend/, \
             the committed local default"
        );
        std::process::exit(1);
    }

    let cfg = match gl_core::Config::from_file(&cli.config) {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::error!(error = %e, path = %cli.config.display(), "failed to load config");
            std::process::exit(1);
        }
    };

    // --prod overrides config; absent → dev mode (safe default).
    let dev_mode = !cli.prod;
    if cfg.dev_mode != dev_mode {
        tracing::warn!(
            config_dev_mode = cfg.dev_mode,
            effective_dev_mode = dev_mode,
            "config.toml dev_mode differs from effective mode; pass --prod to enable production mode"
        );
    }

    println!(
        "Config: {path}\n  db:                {db}\n  base_dir:          {base_dir}\n  domain:            {domain}\n  life_in_days:      {life_in_days}\n  provisioner:       {provisioner}\n  port range:        {port_start}–{port_end}\n  allocator:         {alloc_kind}\n  allocator pool:    {alloc_pool}\n  allocator quota:   {alloc_quota} MB\n  cors_origin:       {cors_origin}\n  bind_address:      {bind_address}\n  sweep_interval:    {sweep}s\n  mode:              {mode}",
        path = cli.config.display(),
        db = cfg.registry.path.display(),
        base_dir = cfg.base_dir.display(),
        domain = cfg.domain,
        life_in_days = cfg.life_in_days,
        provisioner = cfg.provisioner.kind(),
        port_start = cfg.port_range_start,
        port_end = cfg.port_range_end,
        alloc_kind = cfg.allocator.kind,
        alloc_pool = cfg.allocator.pool,
        alloc_quota = cfg.allocator.quota_mb,
        cors_origin = cfg.cors_origin,
        bind_address = cfg.bind_address,
        sweep = cfg.sweep_interval_secs,
        mode = if dev_mode { "dev" } else { "production" },
    );

    let sys: Arc<dyn SysRunner> = Arc::new(RealSysRunner);

    match cli.command {
        Cmd::Alloc { path } => {
            let storage = cfg.allocator.build();
            match storage.allocate(&path) {
                Ok(()) => println!("allocated: {}", path.display()),
                Err(e) => {
                    tracing::error!(error = %e, "alloc failed");
                    std::process::exit(1);
                }
            }
        }
        Cmd::Dealloc { path } => {
            let storage = cfg.allocator.build();
            match storage.release(&path) {
                Ok(()) => println!("released: {}", path.display()),
                Err(e) => {
                    tracing::error!(error = %e, "dealloc failed");
                    std::process::exit(1);
                }
            }
        }
        cmd => {
            // Spawn, Despawn, List — all require a provisioner/GoopyManager.
            let provisioner = cfg.build_provisioner(dev_mode, sys);

            let registry = SqliteRegistry::new(&cfg.registry.path).unwrap_or_else(|e| {
                tracing::error!(error = %e, "failed to open SQLite registry");
                std::process::exit(1);
            });

            let gm = GoopyManager::new(cfg.build_manager_config(), registry, provisioner);
            let mp = MultiProgress::new();
            let mut spinners = vec![];
            // Slugs whose background jobs we must wait for before exiting.
            // For spawn: wait until status leaves Spawning (→ Done or Failed).
            // For despawn: wait until the row is gone (→ deleted) or status is Failed.
            let mut pending_slugs: Vec<String> = vec![];

            match cmd {
                Cmd::Spawn { count } => {
                    for _ in 0..count {
                        let spinner = mp.add(ProgressBar::new_spinner());
                        spinner.set_message("Spawning ...".to_string());
                        spinner.enable_steady_tick(Duration::from_millis(100));

                        match gm.spawn() {
                            Ok((slug, port)) => {
                                spinner.set_message(format!("Spawning {slug} (port {port}) ..."));
                                pending_slugs.push(slug);
                            }
                            Err(e) => {
                                tracing::error!(error = ?e, "spawn failed");
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
                            Ok(slug) => pending_slugs.push(slug),
                            Err(e) => {
                                tracing::error!(error = ?e, "despawn failed");
                                std::process::exit(1);
                            }
                        }
                        spinners.push(spinner);
                    }
                }
                Cmd::List {} => match gm.list() {
                    Ok(goopies) => {
                        for gp in goopies {
                            println!(
                                "{slug}\n  status:           {status}\n  life_in_days:     {life_in_days}\n  created_at:       {created_at}\n  port:             {port}\n  provisioner_kind: {provisioner_kind}\n  service_version:  {service_version}\n  working_dir:      {working_dir}\n",
                                slug = gp.slug,
                                status = gp.status,
                                life_in_days = gp.life_in_days,
                                created_at = gp.created_at.format("%Y-%m-%dT%H:%M:%SZ"),
                                port = gp.port,
                                provisioner_kind = gp.provisioner_kind,
                                service_version = gp.service_version,
                                working_dir = gp.working_dir.display(),
                            );
                        }
                    }
                    Err(e) => {
                        println!("List failed: {:?}", e);
                        std::process::exit(1);
                    }
                },
                Cmd::Alloc { .. } | Cmd::Dealloc { .. } => {
                    unreachable!("Alloc/Dealloc must not reach the provisioner branch")
                }
            }

            // Poll the registry until every background job has reached a terminal
            // state.  A slug is considered finished when its status is no longer
            // Spawning or Despawning (i.e. it transitioned to Done/Failed, or the
            // row was deleted by a completed despawn).
            while pending_slugs.iter().any(|slug| {
                match gm.get(slug) {
                    Ok(Some(g)) => g.status == Status::Spawning || g.status == Status::Despawning,
                    // Row gone (successful despawn) or error reading — either way
                    // no longer in-progress.
                    Ok(None) | Err(_) => false,
                }
            }) {
                std::thread::sleep(Duration::from_secs(1));
            }

            spinners
                .iter()
                .for_each(|s| s.finish_with_message(format!("{} done!", s.message())));
        }
    }
}
