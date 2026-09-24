use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use gl_core::goopy_registry::sqlite_registry::SqliteRegistry;
use gl_core::sys_utils::RealSysRunner;
use gl_core::*;
use indicatif::{MultiProgress, ProgressBar};
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "gl-cli")]
#[command(about = "Maintenance CLI for a Goopy.Life host.")]
#[command(long_about = "\
Maintenance CLI for a Goopy.Life host.

gl-serv has no despawn route, so tearing down a single instance, listing what \
exists, reading why something failed, and allocating or releasing storage by \
hand all happen here. The deploy installs it at /opt/goopy-life/bin/gl-cli \
from the same build as gl-serv, so the two always share a gl-core -- and \
therefore a registry schema and a provisioner.

On a droplet, run it as the service account and name both the config and the \
mode:

    sudo -u goopy /opt/goopy-life/bin/gl-cli \\
        --config /opt/goopy-life/config.toml --prod list

--prod is not optional there. Without it the CLI runs in dev mode whatever the \
config says, and a dev-mode despawn kills a detached process instead of \
removing the systemd unit and the nginx sites, leaving the instance's real \
resources behind.

Running alongside a live gl-serv is safe by design: both open the same SQLite \
registry in WAL mode with a 5s busy_timeout, so a reader never blocks the \
writer and a contended write waits instead of failing. Racing it on one \
instance is refused rather than corrupting anything -- despawning a slug the \
sweeper already claimed fails as Invalid. `alloc` and `dealloc` are the \
exception: they take a raw path and consult no registry, so never aim them at \
a live instance's working directory.")]
struct Cli {
    /// Path to the config file (on a droplet: /opt/goopy-life/config.toml)
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
    /// Show recorded failures and reaps, newest first
    ///
    /// The instance event log (#118) outlives the instances it describes, so
    /// this answers questions `list` cannot: why a spawn failed, and when the
    /// sweep reaped the row. It is the only way to read it on a host —
    /// journald is unreadable by both the service and admin accounts (#116),
    /// and the `goopies` row is long gone.
    ///
    /// Each event carries a `code`, which is a coarse and stable discriminant,
    /// and a `detail`, which is the full rendering of the error and is
    /// OPERATOR-ONLY: it can contain raw command output. Do not paste it
    /// anywhere a visitor can see.
    Events {
        /// Only events for this instance
        #[arg(long)]
        slug: Option<String>,

        /// Maximum number of events to show
        #[arg(long, default_value = "50")]
        limit: u32,
    },
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

    // The installed binary is how an operator tells which build a host is
    // running, so `--version` names the commit, in the same form as
    // `gl-serv --version`. Set at runtime rather than via `#[command(version)]`,
    // which takes only a `&'static str`: the commit stamp lives in gl-core's
    // build environment, not this crate's.
    let cli = Cli::from_arg_matches(
        &Cli::command()
            .version(gl_core::build_info::describe(env!("CARGO_PKG_VERSION")))
            .get_matches(),
    )
    .unwrap_or_else(|e| e.exit());

    // Config file is required — no silent fallback.
    if !cli.config.exists() {
        tracing::error!(
            path = %cli.config.display(),
            "config file not found; try --config config.local.toml from backend/ \
             (the committed local default), or --config /opt/goopy-life/config.toml \
             on a droplet"
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
        "Config: {path}\n  db:                {db}\n  base_dir:          {base_dir}\n  domain:            {domain}\n  life_in_days:      {life_in_days}\n  provisioner:       {provisioner}\n  port range:        {port_start}–{port_end}\n  allocator:         {alloc_kind}\n  allocator pool:    {alloc_pool}\n  allocator quota:   {alloc_quota} MB\n  cors_origin:       {cors_origin}\n  bind_address:      {bind_address}\n  api_address:       {api_address}\n  sweep_interval:    {sweep}s\n  event_retention:   {retention}d\n  mode:              {mode}",
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
        api_address = cfg.resolved_api_address(),
        sweep = cfg.sweep_interval_secs,
        retention = cfg.event_retention_days,
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
                                "{slug}\n  status:           {status}\n  life_in_days:     {life_in_days}\n  created_at:       {created_at}\n  port:             {port}\n  provisioner_kind: {provisioner_kind}\n  service_version:  {service_version}\n  build_sha:        {build_sha}\n  working_dir:      {working_dir}\n",
                                slug = gp.slug,
                                status = gp.status,
                                life_in_days = gp.life_in_days,
                                created_at = gp.created_at.format("%Y-%m-%dT%H:%M:%SZ"),
                                port = gp.port,
                                provisioner_kind = gp.provisioner_kind,
                                service_version = gp.service_version,
                                build_sha = gp.build_sha.as_deref().unwrap_or("(not recorded)"),
                                working_dir = gp.working_dir.display(),
                            );
                        }
                    }
                    Err(e) => {
                        println!("List failed: {:?}", e);
                        std::process::exit(1);
                    }
                },
                Cmd::Events { slug, limit } => match gm.events(slug.as_deref(), limit) {
                    Ok(events) if events.is_empty() => {
                        println!("No instance events recorded.");
                    }
                    Ok(events) => {
                        for ev in events {
                            println!(
                                "{at}  {slug}  {phase}/{outcome}  {code}",
                                at = ev.occurred_at.format("%Y-%m-%dT%H:%M:%SZ"),
                                slug = ev.slug,
                                phase = ev.phase,
                                outcome = ev.outcome,
                                code = ev.code,
                            );
                            // Indented rather than inline: a rendered
                            // subprocess error runs to several hundred
                            // characters and would push the scannable columns
                            // off the screen. Every line of it, because a
                            // captured stderr is usually a whole traceback and
                            // an unindented continuation is indistinguishable
                            // from the next event.
                            if let Some(detail) = ev.detail {
                                for line in detail.lines() {
                                    println!("    {line}");
                                }
                            }
                        }
                    }
                    Err(e) => {
                        println!("Events failed: {:?}", e);
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// clap can only report a malformed command definition (a duplicate long,
    /// a default that does not parse) at runtime, which for a binary means on
    /// the droplet. `debug_assert` surfaces it here instead.
    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    /// The two things an operator has to know before running this on a host:
    /// where the config lives, and that omitting `--prod` there silently gets
    /// a dev-mode teardown that leaves the systemd unit and nginx sites
    /// behind. Both live only in the help text, so pin them.
    #[test]
    fn long_help_warns_about_running_on_a_droplet() {
        let help = Cli::command().render_long_help().to_string();

        assert!(
            help.contains("/opt/goopy-life/config.toml"),
            "long help should name the config path the deploy installs:\n{help}"
        );
        assert!(
            help.contains("--prod is not optional"),
            "long help should say --prod is required on a droplet:\n{help}"
        );
    }

    /// `detail` can carry raw command output, so the one place an operator
    /// meets it has to say so.
    #[test]
    fn events_help_marks_the_detail_as_operator_only() {
        let help = Cli::command()
            .get_subcommands()
            .find(|c| c.get_name() == "events")
            .expect("the events subcommand should exist")
            .clone()
            .render_long_help()
            .to_string();

        assert!(
            help.contains("OPERATOR-ONLY"),
            "events help should warn that detail is not for visitors:\n{help}"
        );
    }
}
