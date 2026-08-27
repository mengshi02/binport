use super::runtime::{human_bytes, toolbox_all_candidates};
use binport::catalog::Platform;
use binport::ssh::{Destination, NativeSsh, SharedJump, select_hosts};
use binport::toolbox;
use binport::{cache_check_command, remote_paths, safe_tool_name, sha256_file, upload_command};
use clap::Args;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// SSH host, @group, or @all
    target: String,
}

pub fn doctor(
    args: DoctorArgs,
    use_password: bool,
    concurrency: usize,
    json: bool,
) -> io::Result<u8> {
    run(args, use_password, concurrency, json, doctor_async)
}

pub fn warm(
    args: DoctorArgs,
    use_password: bool,
    concurrency: usize,
    json: bool,
) -> io::Result<u8> {
    run(args, use_password, concurrency, json, warm_async)
}

fn run<F, Fut>(
    args: DoctorArgs,
    use_password: bool,
    concurrency: usize,
    json: bool,
    operation: F,
) -> io::Result<u8>
where
    F: FnOnce(Vec<String>, Option<String>, usize, bool) -> Fut,
    Fut: Future<Output = io::Result<u8>>,
{
    if concurrency == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--concurrency must be greater than zero",
        ));
    }
    let hosts = if let Some(group) = args.target.strip_prefix('@') {
        select_hosts(group)?
    } else {
        vec![args.target]
    };
    if hosts.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no concrete SSH hosts matched",
        ));
    }
    let password = use_password
        .then(|| rpassword::prompt_password("SSH password: "))
        .transpose()?;
    tokio::runtime::Runtime::new()
        .map_err(io::Error::other)?
        .block_on(operation(hosts, password, concurrency, json))
}

pub async fn prepare_connections(
    hosts: Vec<String>,
    password: Option<&str>,
) -> io::Result<(
    Vec<(String, Destination)>,
    HashMap<String, SharedJump>,
    HashMap<String, String>,
)> {
    let destinations = hosts
        .into_iter()
        .map(|host| Destination::resolve(&host).map(|destination| (host, destination)))
        .collect::<io::Result<Vec<_>>>()?;
    let jump_aliases = destinations
        .iter()
        .filter_map(|(_, destination)| destination.proxy_jump.clone())
        .collect::<HashSet<_>>();
    let mut jumps = HashMap::new();
    let mut jump_errors = HashMap::new();
    for alias in jump_aliases {
        match NativeSsh::connect_jump(&alias, password).await {
            Ok(jump) => {
                jumps.insert(alias, jump);
            }
            Err(error) => {
                jump_errors.insert(alias, error.to_string());
            }
        }
    }
    Ok((destinations, jumps, jump_errors))
}

#[derive(Debug)]
struct DoctorOutcome {
    platform: Platform,
    route: String,
    cached: usize,
    tools: usize,
    elapsed_ms: u128,
}

#[derive(Debug)]
struct WarmOutcome {
    platform: Platform,
    route: String,
    cached: usize,
    uploaded: usize,
    bytes: u64,
    elapsed_ms: u128,
}

async fn warm_async(
    hosts: Vec<String>,
    password: Option<String>,
    concurrency: usize,
    json: bool,
) -> io::Result<u8> {
    let width = hosts.iter().map(String::len).max().unwrap_or(4).max(4);
    let (destinations, jumps, jump_errors) =
        prepare_connections(hosts, password.as_deref()).await?;
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let mut tasks = JoinSet::new();
    for (host, destination) in destinations {
        let permit_pool = Arc::clone(&semaphore);
        let password = password.clone();
        let jump = destination
            .proxy_jump
            .as_ref()
            .and_then(|alias| jumps.get(alias))
            .cloned();
        let jump_error = destination
            .proxy_jump
            .as_ref()
            .and_then(|alias| jump_errors.get(alias))
            .cloned();
        tasks.spawn(async move {
            let _permit = permit_pool
                .acquire_owned()
                .await
                .map_err(io::Error::other)?;
            let result = if let Some(error) = jump_error {
                Err(io::Error::other(format!(
                    "ProxyJump connection failed: {error}"
                )))
            } else {
                warm_host(destination, password.as_deref(), jump.as_ref()).await
            };
            Ok::<_, io::Error>((host, result))
        });
    }
    if !json {
        println!(
            "{:<width$}  {:<13}  {:<12}  {:>6}  {:>8}  {:>8}",
            "HOST", "ROUTE", "PLATFORM", "CACHED", "UPLOADED", "TRANSFER"
        );
    }
    let mut failed = 0;
    let mut results = Vec::new();
    while let Some(task) = tasks.join_next().await {
        let (host, result) = task.map_err(io::Error::other)??;
        match result {
            Ok(outcome) => {
                if json {
                    results.push(serde_json::json!({
                        "host": host,
                        "ok": true,
                        "route": outcome.route,
                        "platform": outcome.platform.name(),
                        "cached_tools": outcome.cached,
                        "uploaded_tools": outcome.uploaded,
                        "transferred_bytes": outcome.bytes,
                        "elapsed_ms": outcome.elapsed_ms,
                    }));
                } else {
                    println!(
                        "{host:<width$}  {:<13}  {:<12}  {:>6}  {:>8}  {:>7}",
                        outcome.route,
                        outcome.platform.name(),
                        outcome.cached,
                        outcome.uploaded,
                        human_bytes(outcome.bytes)
                    );
                }
            }
            Err(error) => {
                failed += 1;
                if json {
                    results.push(serde_json::json!({
                        "host": host,
                        "ok": false,
                        "error": error.to_string(),
                    }));
                } else {
                    println!("{host:<width$}  ERROR {error}");
                }
            }
        }
    }
    if json {
        results.sort_by(|left, right| left["host"].as_str().cmp(&right["host"].as_str()));
        println!(
            "{}",
            serde_json::to_string_pretty(&results).map_err(io::Error::other)?
        );
    }
    Ok(if failed == 0 { 0 } else { 1 })
}

async fn warm_host(
    destination: Destination,
    password: Option<&str>,
    shared_jump: Option<&SharedJump>,
) -> io::Result<WarmOutcome> {
    let started = Instant::now();
    let ssh = if let Some(jump) = shared_jump {
        NativeSsh::connect_with_jump(&destination, password, jump).await?
    } else {
        NativeSsh::connect(&destination, password).await?
    };
    let (status, stdout, stderr) = ssh.execute_capture_fresh("uname -s; uname -m").await?;
    if status != 0 {
        return Err(io::Error::other(format!(
            "platform detection failed: {stderr}"
        )));
    }
    let mut lines = stdout.lines();
    let platform = Platform::from_uname(
        lines.next().unwrap_or_default(),
        lines.next().unwrap_or_default(),
    )
    .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "unsupported remote platform"))?;
    let candidates = toolbox_all_candidates()?
        .into_iter()
        .filter(|candidate| candidate.platform == platform)
        .collect::<Vec<_>>();
    let checks = candidates
        .iter()
        .map(|candidate| {
            format!(
                "{} && printf 1 || printf 0",
                cache_check_command(&candidate.remote_file)
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let flags = if checks.is_empty() {
        String::new()
    } else {
        let (status, stdout, stderr) = ssh.execute_capture_fresh(&checks).await?;
        if status != 0 {
            return Err(io::Error::other(format!(
                "remote cache check failed: {stderr}"
            )));
        }
        stdout
    };
    if flags.len() != candidates.len() {
        return Err(io::Error::other("remote cache check returned invalid data"));
    }
    let mut cached = 0;
    let mut uploaded = 0;
    let mut bytes = 0;
    for (candidate, hit) in candidates.iter().zip(flags.bytes()) {
        if hit == b'1' {
            cached += 1;
            continue;
        }
        let data = fs::read(&candidate.local_path)?;
        bytes += data.len() as u64;
        let (status, _, stderr) = ssh
            .execute_capture_with_input_fresh(
                &upload_command(&candidate.directory, &candidate.remote_file),
                data,
            )
            .await?;
        if status != 0 {
            return Err(io::Error::other(format!(
                "upload of {} failed: {}",
                candidate.name,
                String::from_utf8_lossy(&stderr)
            )));
        }
        uploaded += 1;
    }
    Ok(WarmOutcome {
        platform,
        route: destination_route(&destination),
        cached,
        uploaded,
        bytes,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

async fn doctor_async(
    hosts: Vec<String>,
    password: Option<String>,
    concurrency: usize,
    json: bool,
) -> io::Result<u8> {
    let width = hosts.iter().map(String::len).max().unwrap_or(4).max(4);
    let (destinations, jumps, jump_errors) =
        prepare_connections(hosts, password.as_deref()).await?;
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let mut tasks = JoinSet::new();
    for (host, destination) in destinations {
        let permit_pool = Arc::clone(&semaphore);
        let password = password.clone();
        let jump = destination
            .proxy_jump
            .as_ref()
            .and_then(|alias| jumps.get(alias))
            .cloned();
        let jump_error = destination
            .proxy_jump
            .as_ref()
            .and_then(|alias| jump_errors.get(alias))
            .cloned();
        tasks.spawn(async move {
            let _permit = permit_pool
                .acquire_owned()
                .await
                .map_err(io::Error::other)?;
            let result = if let Some(error) = jump_error {
                Err(io::Error::other(format!(
                    "ProxyJump connection failed: {error}"
                )))
            } else {
                doctor_host(destination, password.as_deref(), jump.as_ref()).await
            };
            Ok::<_, io::Error>((host, result))
        });
    }

    if !json {
        println!(
            "{:<width$}  {:<13}  {:<12}  {:>7}  CACHE",
            "HOST", "ROUTE", "PLATFORM", "LATENCY"
        );
    }
    let mut failed = 0;
    let mut results = Vec::new();
    while let Some(task) = tasks.join_next().await {
        let (host, result) = task.map_err(io::Error::other)??;
        match result {
            Ok(outcome) => {
                if json {
                    results.push(serde_json::json!({
                        "host": host,
                        "ok": true,
                        "route": outcome.route,
                        "platform": outcome.platform.name(),
                        "latency_ms": outcome.elapsed_ms,
                        "cached_tools": outcome.cached,
                        "total_tools": outcome.tools,
                    }));
                } else {
                    println!(
                        "{host:<width$}  {:<13}  {:<12}  {:>6}ms  {}/{}",
                        outcome.route,
                        outcome.platform.name(),
                        outcome.elapsed_ms,
                        outcome.cached,
                        outcome.tools
                    );
                }
            }
            Err(error) => {
                failed += 1;
                if json {
                    results.push(serde_json::json!({
                        "host": host,
                        "ok": false,
                        "error": error.to_string(),
                    }));
                } else {
                    println!("{host:<width$}  ERROR {error}");
                }
            }
        }
    }
    if json {
        results.sort_by(|left, right| left["host"].as_str().cmp(&right["host"].as_str()));
        println!(
            "{}",
            serde_json::to_string_pretty(&results).map_err(io::Error::other)?
        );
    }
    Ok(if failed == 0 { 0 } else { 1 })
}

async fn doctor_host(
    destination: Destination,
    password: Option<&str>,
    shared_jump: Option<&SharedJump>,
) -> io::Result<DoctorOutcome> {
    let started = Instant::now();
    let ssh = if let Some(jump) = shared_jump {
        NativeSsh::connect_with_jump(&destination, password, jump).await?
    } else {
        NativeSsh::connect(&destination, password).await?
    };
    let (status, stdout, stderr) = ssh.execute_capture_fresh("uname -s; uname -m").await?;
    if status != 0 {
        return Err(io::Error::other(format!(
            "platform detection failed: {stderr}"
        )));
    }
    let mut lines = stdout.lines();
    let platform = Platform::from_uname(
        lines.next().unwrap_or_default(),
        lines.next().unwrap_or_default(),
    )
    .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "unsupported remote platform"))?;
    let lock: toolbox::Lockfile =
        serde_json::from_slice(&fs::read(".binport/toolbox.json")?).map_err(io::Error::other)?;
    let files = lock
        .tools
        .into_iter()
        .filter(|entry| entry.platform == platform.name())
        .map(|entry| {
            let path = PathBuf::from(entry.path);
            let name = safe_tool_name(&path)?;
            let (_, remote) = remote_paths(&sha256_file(&path)?, &name);
            Ok(remote)
        })
        .collect::<io::Result<Vec<_>>>()?;
    let checks = files
        .iter()
        .map(|file| format!("{} && printf 1 || printf 0", cache_check_command(file)))
        .collect::<Vec<_>>()
        .join("; ");
    let cached = if checks.is_empty() {
        0
    } else {
        let (_, output, _) = ssh.execute_capture_fresh(&checks).await?;
        output.bytes().filter(|byte| *byte == b'1').count()
    };
    Ok(DoctorOutcome {
        platform,
        route: destination_route(&destination),
        cached,
        tools: files.len(),
        elapsed_ms: started.elapsed().as_millis(),
    })
}

fn destination_route(destination: &Destination) -> String {
    if let Some(jump) = &destination.proxy_jump {
        return format!("jump:{jump}");
    }
    if let Some(bastion) = &destination.bastion_proxy {
        return format!("bastion:{}", bastion.host);
    }
    "direct".to_owned()
}
