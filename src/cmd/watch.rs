use super::fleet::prepare_connections;
use super::remote::run_remote;
use super::runtime::{ToolCandidate, toolbox_candidates, write_prefixed};
use binport::catalog::Platform;
use binport::ssh::{Destination, NativeSsh, SharedJump, select_hosts};
use clap::Args;
use std::ffi::OsString;
use std::io;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::{Duration, MissedTickBehavior};

#[derive(Debug, Args)]
pub struct WatchArgs {
    /// Seconds between snapshots
    #[arg(long, default_value_t = 5)]
    interval: u64,
    /// Stop after this many snapshots
    #[arg(long)]
    count: Option<u64>,
    /// Stop when the command succeeds on every host
    #[arg(long)]
    until_success: bool,
    /// Print unchanged snapshots too
    #[arg(long)]
    all: bool,
    /// Emit one JSON event per line
    #[arg(long)]
    jsonl: bool,
    /// SSH host, @group, or @all
    target: String,
    /// Toolbox tool to execute
    tool: OsString,
    /// Arguments passed to the tool
    #[arg(trailing_var_arg = true)]
    arguments: Vec<OsString>,
}

pub fn run(
    args: WatchArgs,
    use_password: bool,
    concurrency: usize,
    global_json: bool,
) -> io::Result<u8> {
    if global_json {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "watch is an event stream; use `--jsonl` instead of `--json`",
        ));
    }
    if concurrency == 0 || args.interval == 0 || args.count == Some(0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "concurrency, interval, and count must be greater than zero",
        ));
    }
    let hosts = if let Some(group) = args.target.strip_prefix('@') {
        select_hosts(group)?
    } else {
        vec![args.target.clone()]
    };
    if hosts.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no concrete SSH hosts matched",
        ));
    }
    let candidates = toolbox_candidates(&args.tool)?;
    if candidates.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "tool {:?} is not built; run `binport build` first",
                args.tool
            ),
        ));
    }
    let password = use_password
        .then(|| rpassword::prompt_password("SSH password: "))
        .transpose()?;
    tokio::runtime::Runtime::new()
        .map_err(io::Error::other)?
        .block_on(watch_async(hosts, args, candidates, password, concurrency))
}

#[derive(Clone, Debug, PartialEq)]
struct WatchSnapshot {
    status: u32,
    stdout: String,
    stderr: String,
}

struct WatchTarget {
    host: String,
    destination: Destination,
    jump: Option<SharedJump>,
    ssh: Option<NativeSsh>,
    last: Option<WatchSnapshot>,
    online: bool,
    attempted: bool,
}

async fn watch_async(
    hosts: Vec<String>,
    args: WatchArgs,
    candidates: Vec<ToolCandidate>,
    password: Option<String>,
    concurrency: usize,
) -> io::Result<u8> {
    let width = hosts.iter().map(String::len).max().unwrap_or(4);
    let total = hosts.len();
    let (destinations, jumps, _jump_errors) =
        prepare_connections(hosts, password.as_deref()).await?;
    let mut targets = destinations
        .into_iter()
        .map(|(host, destination)| WatchTarget {
            jump: destination
                .proxy_jump
                .as_ref()
                .and_then(|alias| jumps.get(alias))
                .cloned(),
            host,
            destination,
            ssh: None,
            last: None,
            online: false,
            attempted: false,
        })
        .collect::<Vec<_>>();
    let candidates = Arc::new(candidates);
    let started = Instant::now();
    let mut ticker = tokio::time::interval(Duration::from_secs(args.interval));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    let mut iteration = 0_u64;
    let mut final_code = 0;

    if !args.jsonl {
        eprintln!(
            "Watching {total} host{} every {}s · Ctrl-C to stop",
            if total == 1 { "" } else { "s" },
            args.interval
        );
    }

    loop {
        tokio::select! {
            signal = &mut shutdown => {
                signal.map_err(io::Error::other)?;
                if !args.jsonl { eprintln!("Watch stopped"); }
                break;
            }
            _ = ticker.tick() => {}
        }
        iteration += 1;
        let mut iteration_failed = false;
        let semaphore = Arc::new(Semaphore::new(concurrency));
        let mut tasks = JoinSet::new();
        for mut target in targets {
            let permit_pool = Arc::clone(&semaphore);
            let candidates = Arc::clone(&candidates);
            let arguments = args.arguments.clone();
            let password = password.clone();
            tasks.spawn(async move {
                let _permit = permit_pool
                    .acquire_owned()
                    .await
                    .map_err(io::Error::other)?;
                let result =
                    watch_target_once(&mut target, &candidates, &arguments, password.as_deref())
                        .await;
                Ok::<_, io::Error>((target, result))
            });
        }
        targets = Vec::with_capacity(total);
        let mut successful = 0_usize;
        while let Some(task) = tasks.join_next().await {
            let (mut target, result) = task.map_err(io::Error::other)??;
            let elapsed = started.elapsed().as_secs_f64();
            match result {
                Ok((platform, cache_hit, snapshot)) => {
                    let kind = if target.attempted && !target.online {
                        "RECOVERED"
                    } else if target.last.is_none() {
                        "INITIAL"
                    } else if target.last.as_ref() == Some(&snapshot) {
                        "UNCHANGED"
                    } else if snapshot.stdout.is_empty()
                        && snapshot.stderr.is_empty()
                        && target
                            .last
                            .as_ref()
                            .is_some_and(|last| !last.stdout.is_empty() || !last.stderr.is_empty())
                    {
                        "CLEARED"
                    } else {
                        "CHANGED"
                    };
                    if snapshot.status == 0 {
                        successful += 1;
                    } else {
                        iteration_failed = true;
                    }
                    if kind != "UNCHANGED" || args.all {
                        emit_watch_event(
                            &target.host,
                            width,
                            kind,
                            elapsed,
                            Some(platform),
                            Some(cache_hit),
                            Some(&snapshot),
                            None,
                            args.jsonl,
                        )?;
                    }
                    target.last = Some(snapshot);
                    target.online = true;
                    target.attempted = true;
                }
                Err(error) => {
                    iteration_failed = true;
                    if target.online || !target.attempted || args.all {
                        emit_watch_event(
                            &target.host,
                            width,
                            "OFFLINE",
                            elapsed,
                            None,
                            None,
                            None,
                            Some(&error.to_string()),
                            args.jsonl,
                        )?;
                    }
                    target.online = false;
                    target.attempted = true;
                    target.ssh = None;
                }
            }
            targets.push(target);
        }
        targets.sort_by(|left, right| left.host.cmp(&right.host));
        final_code = if iteration_failed { 1 } else { 0 };
        if args.until_success && successful == total {
            if !args.jsonl {
                eprintln!("Condition satisfied on all {total} hosts");
            }
            final_code = 0;
            break;
        }
        if args.count.is_some_and(|count| iteration >= count) {
            break;
        }
    }
    Ok(final_code)
}

async fn watch_target_once(
    target: &mut WatchTarget,
    candidates: &[ToolCandidate],
    arguments: &[OsString],
    password: Option<&str>,
) -> io::Result<(Platform, bool, WatchSnapshot)> {
    // Reconnect for bastion hosts on every iteration since they only support
    // one exec channel per connection, and run_remote will use multiple channels.
    if target.ssh.is_none() || target.ssh.as_ref().is_some_and(|s| s.is_bastion()) {
        target.ssh = Some(if let Some(jump) = &target.jump {
            NativeSsh::connect_with_jump(&target.destination, password, jump).await?
        } else {
            NativeSsh::connect(&target.destination, password).await?
        });
    }
    let (status, stdout, stderr, platform, cache_hit) = run_remote(
        target.ssh.as_ref().expect("SSH was initialized"),
        candidates,
        arguments,
    )
    .await?;
    Ok((
        platform,
        cache_hit,
        WatchSnapshot {
            status,
            stdout,
            stderr,
        },
    ))
}

#[allow(clippy::too_many_arguments)]
fn emit_watch_event(
    host: &str,
    width: usize,
    kind: &str,
    elapsed: f64,
    platform: Option<Platform>,
    cache_hit: Option<bool>,
    snapshot: Option<&WatchSnapshot>,
    error: Option<&str>,
    jsonl: bool,
) -> io::Result<()> {
    if jsonl {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "elapsed_seconds": elapsed,
                "host": host,
                "event": kind.to_ascii_lowercase(),
                "platform": platform.map(Platform::name),
                "cache_hit": cache_hit,
                "status": snapshot.map(|value| value.status),
                "stdout": snapshot.map(|value| value.stdout.as_str()),
                "stderr": snapshot.map(|value| value.stderr.as_str()),
                "error": error,
            }))
            .map_err(io::Error::other)?
        );
    } else {
        println!("+{elapsed:>7.1}s  {host:width$}  {kind}");
        if let Some(snapshot) = snapshot {
            write_prefixed(host, width, &snapshot.stdout, false);
            write_prefixed(host, width, &snapshot.stderr, true);
            if snapshot.status != 0 {
                eprintln!("{host:width$}  exited with status {}", snapshot.status);
            }
        }
        if let Some(error) = error {
            eprintln!("{host:width$}  {error}");
        }
    }
    Ok(())
}
