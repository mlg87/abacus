mod app;
mod bd;
mod dispatch;
mod git;
mod model;
mod prompt;
mod runs;
mod session;
mod ui;
mod view_model;
mod watch;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Parser;
use crossterm::event::{self, Event, KeyEventKind};

use app::{App, AppEvent, Config};

/// Terminal UI for a beads graph that dispatches headless omp agents onto beads.
#[derive(Parser, Debug)]
#[command(version)]
struct Cli {
    /// Any directory inside a repo that has `.beads/` (worktrees resolve to the primary checkout).
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Agents running at once; further dispatches wait in a queue.
    #[arg(long, default_value_t = 3)]
    max_concurrent: usize,
    /// Model passed to the agent as `--model`; omitted uses the agent's default.
    #[arg(long)]
    model: Option<String>,
    /// Agent executable (must accept omp's headless flags).
    #[arg(long, default_value = "omp")]
    agent_cmd: String,
    /// Where run logs, sessions, and scratch worktrees live.
    #[arg(long, env = "ABACUS_STATE_DIR")]
    state_dir: Option<PathBuf>,
}

fn require(cmd: &str, args: &[&str]) -> Result<()> {
    let ok = Command::new(cmd)
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        bail!(
            "`{cmd} {}` failed — is {cmd} installed and on PATH?",
            args.join(" ")
        );
    }
    Ok(())
}

fn resolve_agent_cmd(cmd: &str) -> Result<String> {
    if cmd.contains('/') {
        let p = Path::new(cmd);
        if !p.is_file() {
            bail!("--agent-cmd {cmd} does not exist");
        }
        return Ok(p
            .canonicalize()
            .unwrap_or_else(|_| p.to_path_buf())
            .to_string_lossy()
            .into_owned());
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    if std::env::split_paths(&path).any(|d| d.join(cmd).is_file()) {
        return Ok(cmd.to_string());
    }
    bail!("--agent-cmd {cmd} not found on PATH")
}

fn startup(cli: Cli) -> Result<(Config, PathBuf)> {
    require("bd", &["--version"])?;
    require("git", &["--version"])?;
    let agent_cmd = resolve_agent_cmd(&cli.agent_cmd)?;
    if !cli.repo.is_dir() {
        bail!("--repo {} is not a directory", cli.repo.display());
    }
    let beads_dir = bd::beads_dir(&cli.repo).context("no beads workspace found")?;
    let repo_root = beads_dir
        .parent()
        .context("beads dir has no parent")?
        .to_path_buf();
    let state_dir = match cli.state_dir {
        Some(p) => p,
        None => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?)
            .join(".local/state/abacus"),
    };
    std::fs::create_dir_all(state_dir.join("runs"))?;
    std::fs::create_dir_all(state_dir.join("scratch"))?;
    let repo_name = repo_root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let origin = git::origin_slug(&repo_root);
    Ok((
        Config {
            repo_root,
            state_dir,
            max_concurrent: cli.max_concurrent.max(1),
            model: cli.model,
            agent_cmd,
            repo_name,
            origin,
        },
        beads_dir,
    ))
}

fn main() {
    if let Err(e) = run(Cli::parse()) {
        eprintln!("abacus: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let (cfg, beads_dir) = startup(cli)?;
    let registry = runs::Registry::load(&cfg.state_dir, &cfg.repo_root)?;
    let (tx, rx) = mpsc::channel();

    let watch_error = match watch::spawn(&beads_dir, tx.clone()) {
        Ok(w) => {
            // Lives for the whole process; dropping it stops the watch.
            std::mem::forget(w);
            None
        }
        Err(e) => Some(format!("watcher unavailable ({e}); polling every 30s")),
    };

    let input_tx = tx.clone();
    std::thread::spawn(move || {
        loop {
            match event::read() {
                Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => {
                    if input_tx.send(AppEvent::Key(k)).is_err() {
                        return;
                    }
                }
                Ok(Event::Resize(..)) => {
                    let _ = input_tx.send(AppEvent::Resize);
                }
                Ok(_) => {}
                Err(_) => return,
            }
        }
    });

    let mut app = App::new(cfg, registry, tx);
    if let Some(e) = watch_error {
        app.flash(e);
    }
    app.request_reload();

    let mut terminal = ratatui::init();
    let result = (|| -> Result<()> {
        loop {
            app.tick();
            terminal.draw(|f| ui::draw(f, &mut app))?;
            if app.quit {
                return Ok(());
            }
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(ev) => {
                    app.handle(ev);
                    // Drain bursts (key repeat, paste) before redrawing.
                    while let Ok(ev) = rx.try_recv() {
                        app.handle(ev);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            }
        }
    })();
    ratatui::restore();
    result
}
