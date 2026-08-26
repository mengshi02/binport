mod cmd;

use binport::catalog::Platform;
use binport::progress::TransferProgress;
use binport::ssh::{Destination, NativeSsh, SharedJump, StreamChunk, select_hosts};
use binport::{cache_check_command, execute_command, probe_execute_command, upload_command};
use clap::{Parser, Subcommand};
use cmd::auth::AuthArgs;
use cmd::fleet::{DoctorArgs, prepare_connections};
use cmd::host::HostArgs;
use cmd::lifecycle::{BuildArgs, FetchArgs, ProjectArgs, TransferArgs};
use cmd::plan::PlanArgs;
use cmd::registry::{PullArgs, PushArgs};
use cmd::runtime::{ToolCandidate, ad_hoc_route, toolbox_candidates, write_prefixed};
use cmd::transfer::{CpArgs, RmArgs};
use cmd::watch::WatchArgs;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Write};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;

#[derive(Debug, Parser)]
#[command(
    name = "binport",
    version,
    about = "Build portable toolboxes and run them on SSH hosts"
)]
struct Cli {
    /// Prompt for an SSH password instead of using keys or an agent
    #[arg(long, global = true)]
    password: bool,

    /// Print platform, cache, and transfer details
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Maximum number of simultaneous Fleet connections
    #[arg(long, global = true, default_value_t = 10)]
    concurrency: usize,

    /// Emit machine-readable JSON
    #[arg(long, global = true)]
    json: bool,

    /// Allocate an interactive terminal for a single remote host
    #[arg(short = 't', long, global = true)]
    tty: bool,

    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Debug, Subcommand)]
enum CommandKind {
    /// Set up and manage passwordless SSH authentication
    Auth(AuthArgs),
    /// Add and manage reusable SSH hosts and jump routes
    Host(HostArgs),
    /// Resolve Binfile sources into Binport.lock
    Resolve(BuildArgs),
    /// Build a toolbox from a Binfile
    Build(BuildArgs),
    /// List tools in the Binfile or built toolbox
    #[command(alias = "list")]
    Ls(ProjectArgs),
    /// Download tools into the local cache
    Fetch(FetchArgs),
    /// Show toolbox and cache status
    Status(ProjectArgs),
    /// Copy a file between local and remote paths
    Cp(CpArgs),
    /// Remove a remote file or directory
    Rm(RmArgs),
    /// Remove downloaded toolbox cache
    Clean,
    /// Export the built toolbox as one offline file
    Export(TransferArgs),
    /// Load an offline toolbox file
    Load(TransferArgs),
    /// Pack a built toolbox as a local OCI image layout
    Pack(TransferArgs),
    /// Unpack a local OCI image layout into the project
    Unpack(TransferArgs),
    /// Pull an OCI toolbox from a Registry into the project
    Pull(PullArgs),
    /// Push the built toolbox to an OCI Registry
    Push(PushArgs),
    /// Check host reachability, platform, route, latency, and toolbox cache
    Doctor(DoctorArgs),
    /// Preload the complete toolbox onto a host or fleet
    Warm(DoctorArgs),
    /// Show hosts, routes, and artifacts without connecting
    Plan(PlanArgs),
    /// Repeatedly run a toolbox command and report fleet changes
    Watch(WatchArgs),
    /// Execute a toolbox tool on an SSH host
    #[command(external_subcommand)]
    Remote(Vec<OsString>),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    binport::progress::set_enabled(!cli.json);
    match run(cli) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("binport: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> io::Result<u8> {
    let use_password = cli.password;
    let verbose = cli.verbose;
    let concurrency = cli.concurrency;
    let json = cli.json;
    let tty = cli.tty;
    match cli.command {
        CommandKind::Auth(args) => cmd::auth::run(args, json),
        CommandKind::Host(args) => cmd::host::run(args, use_password, json),
        CommandKind::Resolve(args) => cmd::lifecycle::resolve(args),
        CommandKind::Build(args) => cmd::lifecycle::build(args),
        CommandKind::Ls(args) => cmd::lifecycle::list(args),
        CommandKind::Fetch(args) => cmd::lifecycle::fetch(args),
        CommandKind::Status(args) => cmd::lifecycle::status(args),
        CommandKind::Cp(args) => cmd::transfer::copy(args, use_password, json),
        CommandKind::Rm(args) => cmd::transfer::remove(args, use_password, json),
        CommandKind::Clean => cmd::lifecycle::clean(),
        CommandKind::Export(args) => cmd::lifecycle::export(args),
        CommandKind::Load(args) => cmd::lifecycle::load(args),
        CommandKind::Pack(args) => cmd::lifecycle::pack(args),
        CommandKind::Unpack(args) => cmd::lifecycle::unpack(args),
        CommandKind::Pull(args) => cmd::registry::pull(args),
        CommandKind::Push(args) => cmd::registry::push(args),
        CommandKind::Doctor(args) => cmd::fleet::doctor(args, use_password, concurrency, json),
        CommandKind::Warm(args) => cmd::fleet::warm(args, use_password, concurrency, json),
        CommandKind::Plan(args) => cmd::plan::run(args, json),
        CommandKind::Watch(args) => cmd::watch::run(args, use_password, concurrency, json),
        CommandKind::Remote(args) => remote(args, use_password, verbose, concurrency, json, tty),
    }
}

fn remote(
    args: Vec<OsString>,
    use_password: bool,
    verbose: bool,
    concurrency: usize,
    json: bool,
    tty: bool,
) -> io::Result<u8> {
    if args.len() < 2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: binport <HOST> <TOOL> [ARGUMENTS]...",
        ));
    }
    let host = &args[0];
    let requested_tool = &args[1];
    let tool = if requested_tool == OsStr::new("edit") {
        OsString::from("micro")
    } else {
        requested_tool.to_owned()
    };
    let arguments = default_tool_arguments(&tool, &args[2..]);
    let host = host
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "host is not valid UTF-8"))?;
    let password = if use_password {
        Some(rpassword::prompt_password("SSH password: ")?)
    } else {
        None
    };
    let runtime = tokio::runtime::Runtime::new().map_err(io::Error::other)?;
    let tty = tty || matches!(tool.to_str(), Some("btm" | "micro"));
    if let Some(group) = host.strip_prefix('@') {
        if tty {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "TTY mode supports one host at a time, not fleet targets",
            ));
        }
        if concurrency == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--concurrency must be greater than zero",
            ));
        }
        let hosts = select_hosts(group)?;
        if hosts.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no concrete SSH hosts match @{group}"),
            ));
        }
        return runtime.block_on(fleet_async(
            hosts,
            tool,
            arguments,
            password,
            verbose,
            concurrency,
            json,
        ));
    }

    if tty {
        if json {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--tty and --json cannot be used together",
            ));
        }
        return runtime
            .block_on(remote_tty_async(
                host,
                &tool,
                &arguments,
                password.as_deref(),
            ))
            .map(|status| u8::try_from(status).unwrap_or(1))
            .map_err(|error| authentication_hint(host, error));
    }

    let outcome = runtime
        .block_on(remote_async(host, &tool, &arguments, password.as_deref()))
        .map_err(|error| authentication_hint(host, error))?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&remote_json(host, &outcome)).map_err(io::Error::other)?
        );
    } else {
        print_single(&outcome, verbose)?;
    }
    Ok(u8::try_from(outcome.status).unwrap_or(1))
}

fn default_tool_arguments(tool: &OsStr, arguments: &[OsString]) -> Vec<OsString> {
    if tool != OsStr::new("eza") {
        return arguments.to_vec();
    }
    let mut defaults = Vec::new();
    let has_layout = arguments.iter().any(|argument| {
        matches!(
            argument.to_str(),
            Some("-l" | "--long" | "-1" | "--oneline" | "-G" | "--grid" | "-T" | "--tree")
        )
    });
    let has_color = arguments.iter().any(|argument| {
        argument.to_str().is_some_and(|value| {
            value == "--color"
                || value == "--colour"
                || value.starts_with("--color=")
                || value.starts_with("--colour=")
        })
    });
    if !has_layout {
        defaults.push(OsString::from("--long"));
    }
    if !has_color {
        defaults.push(OsString::from("--color=always"));
    }
    defaults.extend_from_slice(arguments);
    defaults
}

async fn remote_tty_async(
    host: &str,
    tool: &OsStr,
    arguments: &[OsString],
    password: Option<&str>,
) -> io::Result<u32> {
    let ssh = if let Some((jump_host, target_host)) = ad_hoc_route(host)? {
        let jump = NativeSsh::connect_jump(jump_host, password).await?;
        let destination = Destination::resolve(target_host)?;
        NativeSsh::connect_with_jump(&destination, password, &jump).await?
    } else {
        let destination = Destination::resolve(host)?;
        NativeSsh::connect(&destination, password).await?
    };
    let candidates = toolbox_candidates(tool)?;
    if candidates.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("tool {:?} is not built; run `binport build` first", tool),
        ));
    }
    let (probe_status, os, arch) = {
        let (status, stdout, _) = ssh.execute_capture("uname -s; uname -m").await?;
        let mut lines = stdout.lines();
        (
            status,
            lines.next().unwrap_or_default().to_owned(),
            lines.next().unwrap_or_default().to_owned(),
        )
    };
    if probe_status != 0 {
        return Err(io::Error::other("failed to detect remote platform"));
    }
    let platform = Platform::from_uname(&os, &arch).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            format!("remote platform is not supported: {os}/{arch}"),
        )
    })?;
    let candidate = candidates
        .iter()
        .find(|candidate| candidate.platform == platform)
        .ok_or_else(|| io::Error::other("toolbox artifact is missing for the remote platform"))?;
    let (cached, _, _) = ssh
        .execute_capture(&cache_check_command(&candidate.remote_file))
        .await?;
    if cached != 0 {
        upload_tool(&ssh, candidate, true).await?;
    }
    ssh.execute_tty(
        &execute_command(&candidate.remote_file, arguments)?,
        tool == OsStr::new("btm"),
    )
    .await
}

fn authentication_hint(host: &str, error: io::Error) -> io::Error {
    let message = error.to_string();
    let lower = message.to_ascii_lowercase();
    if lower.contains("authentication") || lower.contains("no ssh agent or private key") {
        io::Error::new(
            error.kind(),
            format!(
                "{message}; run `binport auth setup {host}` for passwordless access or retry with --password"
            ),
        )
    } else {
        error
    }
}

#[derive(Debug)]
struct RemoteOutcome {
    status: u32,
    stdout: String,
    stderr: String,
    platform: Platform,
    cache_hit: bool,
    destination: String,
    proxy_jump: Option<String>,
}

struct FleetOutput {
    host: String,
    stderr: bool,
    data: Vec<u8>,
}

fn remote_json(host: &str, outcome: &RemoteOutcome) -> serde_json::Value {
    serde_json::json!({
        "host": host,
        "destination": outcome.destination,
        "proxy_jump": outcome.proxy_jump,
        "platform": outcome.platform.name(),
        "cache_hit": outcome.cache_hit,
        "status": outcome.status,
        "stdout": outcome.stdout,
        "stderr": outcome.stderr,
        "ok": outcome.status == 0,
    })
}

async fn remote_async(
    host: &str,
    tool: &OsStr,
    arguments: &[OsString],
    password: Option<&str>,
) -> io::Result<RemoteOutcome> {
    if let Some((jump_host, target_host)) = ad_hoc_route(host)? {
        let jump = NativeSsh::connect_jump(jump_host, password).await?;
        let mut destination = Destination::resolve(target_host)?;
        destination.proxy_jump = Some(jump_host.to_owned());
        return remote_destination_async(destination, tool, arguments, password, Some(&jump)).await;
    }
    let destination = Destination::resolve(host)?;
    remote_destination_async(destination, tool, arguments, password, None).await
}

async fn remote_destination_async(
    destination: Destination,
    tool: &OsStr,
    arguments: &[OsString],
    password: Option<&str>,
    shared_jump: Option<&SharedJump>,
) -> io::Result<RemoteOutcome> {
    let ssh = if let Some(jump) = shared_jump {
        NativeSsh::connect_with_jump(&destination, password, jump).await?
    } else {
        NativeSsh::connect(&destination, password).await?
    };
    let candidates = toolbox_candidates(tool)?;
    if candidates.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("tool {:?} is not built; run `binport build` first", tool),
        ));
    }
    let (status, stdout, stderr, platform, cache_hit) =
        run_remote(&ssh, &candidates, arguments).await?;
    Ok(RemoteOutcome {
        status,
        stdout,
        stderr,
        platform,
        cache_hit,
        destination: format!(
            "{}@{}:{}",
            destination.user, destination.hostname, destination.port
        ),
        proxy_jump: ssh
            .uses_proxy_jump()
            .then(|| destination.proxy_jump.unwrap_or_else(|| "jump host".into())),
    })
}

pub(crate) async fn run_remote(
    ssh: &NativeSsh,
    candidates: &[ToolCandidate],
    arguments: &[OsString],
) -> io::Result<(u32, String, String, Platform, bool)> {
    let remote_for = |platform| {
        candidates
            .iter()
            .find(|candidate| candidate.platform == platform)
            .map(|candidate| candidate.remote_file.as_str())
    };
    let command = probe_execute_command(
        remote_for(Platform::LinuxAmd64),
        remote_for(Platform::LinuxArm64),
        arguments,
    )?;
    let (status, stdout, stderr) = ssh.execute_capture(&command).await?;
    let (protocol, tool_stderr) = stderr.split_once('\n').unwrap_or((&stderr, ""));
    let fields = protocol.split_whitespace().collect::<Vec<_>>();
    if fields.first() != Some(&"__BINPORT__") || fields.len() < 3 {
        return Err(io::Error::other(format!(
            "invalid remote bootstrap response: {protocol}"
        )));
    }
    let platform = Platform::parse(fields[2]).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            format!("remote platform is not supported: {}", fields[2]),
        )
    })?;
    if fields[1] == "hit" {
        return Ok((status, stdout, tool_stderr.to_owned(), platform, true));
    }
    if fields[1] != "miss" {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "tool is unavailable for remote platform {}",
                platform.name()
            ),
        ));
    }
    let candidate = candidates
        .iter()
        .find(|candidate| candidate.platform == platform)
        .ok_or_else(|| io::Error::other("bootstrap selected a missing toolbox artifact"))?;
    upload_tool(ssh, candidate, true).await?;
    let (status, stdout, stderr) = ssh
        .execute_capture(&execute_command(&candidate.remote_file, arguments)?)
        .await?;
    Ok((status, stdout, stderr, platform, false))
}

async fn upload_tool(
    ssh: &NativeSsh,
    candidate: &ToolCandidate,
    show_progress: bool,
) -> io::Result<()> {
    let total = fs::metadata(&candidate.local_path)?.len();
    let label = candidate
        .local_path
        .file_name()
        .and_then(OsStr::to_str)
        .map_or_else(|| "tool".to_owned(), |name| format!("upload {name}"));
    let progress = TransferProgress::new(label, Some(total), show_progress);
    let (status, stderr) = ssh
        .upload_file(
            &upload_command(&candidate.directory, &candidate.remote_file),
            &candidate.local_path,
            progress,
        )
        .await?;
    if status != 0 {
        return Err(io::Error::other(format!(
            "tool upload failed: {}",
            String::from_utf8_lossy(&stderr)
        )));
    }
    Ok(())
}

async fn remote_destination_stream_async(
    host: &str,
    destination: Destination,
    tool: &OsStr,
    arguments: &[OsString],
    password: Option<&str>,
    shared_jump: Option<&SharedJump>,
    output: mpsc::UnboundedSender<FleetOutput>,
) -> io::Result<RemoteOutcome> {
    let ssh = if let Some(jump) = shared_jump {
        NativeSsh::connect_with_jump(&destination, password, jump).await?
    } else {
        NativeSsh::connect(&destination, password).await?
    };
    let candidates = toolbox_candidates(tool)?;
    if candidates.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("tool {:?} is not built; run `binport build` first", tool),
        ));
    }
    let (status, platform, cache_hit) =
        run_remote_stream(&ssh, &candidates, arguments, host, output).await?;
    Ok(RemoteOutcome {
        status,
        stdout: String::new(),
        stderr: String::new(),
        platform,
        cache_hit,
        destination: format!(
            "{}@{}:{}",
            destination.user, destination.hostname, destination.port
        ),
        proxy_jump: ssh
            .uses_proxy_jump()
            .then(|| destination.proxy_jump.unwrap_or_else(|| "jump host".into())),
    })
}

async fn run_remote_stream(
    ssh: &NativeSsh,
    candidates: &[ToolCandidate],
    arguments: &[OsString],
    host: &str,
    output: mpsc::UnboundedSender<FleetOutput>,
) -> io::Result<(u32, Platform, bool)> {
    let remote_for = |platform| {
        candidates
            .iter()
            .find(|candidate| candidate.platform == platform)
            .map(|candidate| candidate.remote_file.as_str())
    };
    let command = probe_execute_command(
        remote_for(Platform::LinuxAmd64),
        remote_for(Platform::LinuxArm64),
        arguments,
    )?;
    let (status, protocol) = stream_probe(ssh, &command, host, &output).await?;
    let fields = protocol.split_whitespace().collect::<Vec<_>>();
    if fields.first() != Some(&"__BINPORT__") || fields.len() < 3 {
        return Err(io::Error::other(format!(
            "invalid remote bootstrap response: {protocol}"
        )));
    }
    let platform = Platform::parse(fields[2]).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            format!("remote platform is not supported: {}", fields[2]),
        )
    })?;
    if fields[1] == "hit" {
        return Ok((status, platform, true));
    }
    if fields[1] != "miss" {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "tool is unavailable for remote platform {}",
                platform.name()
            ),
        ));
    }
    let candidate = candidates
        .iter()
        .find(|candidate| candidate.platform == platform)
        .ok_or_else(|| io::Error::other("bootstrap selected a missing toolbox artifact"))?;
    let (uploaded, _, upload_error) = ssh
        .execute_capture_with_input(
            &upload_command(&candidate.directory, &candidate.remote_file),
            fs::read(&candidate.local_path)?,
        )
        .await?;
    if uploaded != 0 {
        return Err(io::Error::other(format!(
            "tool upload failed: {}",
            String::from_utf8_lossy(&upload_error)
        )));
    }
    let command = execute_command(&candidate.remote_file, arguments)?;
    let status = stream_plain(ssh, &command, host, &output).await?;
    Ok((status, platform, false))
}

async fn stream_probe(
    ssh: &NativeSsh,
    command: &str,
    host: &str,
    output: &mpsc::UnboundedSender<FleetOutput>,
) -> io::Result<(u32, String)> {
    let (raw_tx, mut raw_rx) = mpsc::unbounded_channel();
    let execution = ssh.execute_stream(command, raw_tx);
    tokio::pin!(execution);
    let mut protocol = Vec::new();
    let mut protocol_done = false;
    loop {
        tokio::select! {
            result = &mut execution => {
                let status = result?;
                while let Ok(chunk) = raw_rx.try_recv() {
                    consume_probe_chunk(chunk, host, output, &mut protocol, &mut protocol_done);
                }
                return Ok((status, String::from_utf8_lossy(&protocol).trim().to_owned()));
            }
            Some(chunk) = raw_rx.recv() => {
                consume_probe_chunk(chunk, host, output, &mut protocol, &mut protocol_done);
            }
        }
    }
}

fn consume_probe_chunk(
    chunk: StreamChunk,
    host: &str,
    output: &mpsc::UnboundedSender<FleetOutput>,
    protocol: &mut Vec<u8>,
    protocol_done: &mut bool,
) {
    if !chunk.stderr || *protocol_done {
        let _ = output.send(FleetOutput {
            host: host.to_owned(),
            stderr: chunk.stderr,
            data: chunk.data,
        });
        return;
    }
    protocol.extend_from_slice(&chunk.data);
    if let Some(newline) = protocol.iter().position(|byte| *byte == b'\n') {
        let remainder = protocol.split_off(newline + 1);
        protocol.truncate(newline);
        *protocol_done = true;
        if !remainder.is_empty() {
            let _ = output.send(FleetOutput {
                host: host.to_owned(),
                stderr: true,
                data: remainder,
            });
        }
    }
}

async fn stream_plain(
    ssh: &NativeSsh,
    command: &str,
    host: &str,
    output: &mpsc::UnboundedSender<FleetOutput>,
) -> io::Result<u32> {
    let (raw_tx, mut raw_rx) = mpsc::unbounded_channel();
    let execution = ssh.execute_stream(command, raw_tx);
    tokio::pin!(execution);
    loop {
        tokio::select! {
            result = &mut execution => {
                let status = result?;
                while let Ok(chunk) = raw_rx.try_recv() {
                    let _ = output.send(FleetOutput { host: host.to_owned(), stderr: chunk.stderr, data: chunk.data });
                }
                return Ok(status);
            }
            Some(chunk) = raw_rx.recv() => {
                let _ = output.send(FleetOutput { host: host.to_owned(), stderr: chunk.stderr, data: chunk.data });
            }
        }
    }
}

fn print_single(outcome: &RemoteOutcome, verbose: bool) -> io::Result<()> {
    if verbose {
        let route = outcome
            .proxy_jump
            .as_ref()
            .map(|jump| format!(" via {jump}"))
            .unwrap_or_default();
        eprintln!(
            "binport: connected to {}{} ({})",
            outcome.destination,
            route,
            outcome.platform.name()
        );
        eprintln!(
            "binport: cache {}",
            if outcome.cache_hit { "hit" } else { "miss" }
        );
    }
    print!("{}", outcome.stdout);
    eprint!("{}", outcome.stderr);
    io::stdout().flush()?;
    io::stderr().flush()
}

async fn fleet_async(
    hosts: Vec<String>,
    tool: OsString,
    arguments: Vec<OsString>,
    password: Option<String>,
    verbose: bool,
    concurrency: usize,
    json: bool,
) -> io::Result<u8> {
    let started = Instant::now();
    let width = hosts.iter().map(String::len).max().unwrap_or(1);
    let total = hosts.len();
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let mut tasks = JoinSet::new();
    let (output_tx, output_rx) = mpsc::unbounded_channel();
    let output_task = (!json).then(|| tokio::spawn(print_fleet_stream(output_rx, width)));
    let (destinations, jumps, jump_errors) =
        prepare_connections(hosts, password.as_deref()).await?;
    let jump_count = jumps.len() + jump_errors.len();

    for (host, destination) in destinations {
        let permit_pool = Arc::clone(&semaphore);
        let tool = tool.clone();
        let arguments = arguments.clone();
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
        let output = output_tx.clone();
        tasks.spawn(async move {
            let _permit = permit_pool
                .acquire_owned()
                .await
                .map_err(io::Error::other)?;
            let result = if let Some(error) = jump_error {
                Err(io::Error::other(format!(
                    "ProxyJump connection failed: {error}"
                )))
            } else if json {
                remote_destination_async(
                    destination,
                    &tool,
                    &arguments,
                    password.as_deref(),
                    jump.as_ref(),
                )
                .await
            } else {
                remote_destination_stream_async(
                    &host,
                    destination,
                    &tool,
                    &arguments,
                    password.as_deref(),
                    jump.as_ref(),
                    output,
                )
                .await
            };
            Ok::<_, io::Error>((host, result))
        });
    }
    drop(output_tx);

    let mut succeeded = 0_usize;
    let mut failed = 0_usize;
    let mut results = Vec::with_capacity(total);
    let mut notices = Vec::new();
    while let Some(task) = tasks.join_next().await {
        let (host, result) = task.map_err(io::Error::other)??;
        match result {
            Ok(outcome) => {
                if json {
                    results.push(remote_json(&host, &outcome));
                } else if verbose {
                    let route = outcome
                        .proxy_jump
                        .as_ref()
                        .map(|jump| format!(" via {jump}"))
                        .unwrap_or_default();
                    notices.push(format!(
                        "{host:width$}  connected {}{} · {} · cache {}",
                        outcome.destination,
                        route,
                        outcome.platform.name(),
                        if outcome.cache_hit { "hit" } else { "miss" }
                    ));
                }
                if !json {
                    write_prefixed(&host, width, &outcome.stdout, false);
                    write_prefixed(&host, width, &outcome.stderr, true);
                }
                if outcome.status == 0 {
                    succeeded += 1;
                } else {
                    failed += 1;
                    if !json {
                        notices.push(format!(
                            "{host:width$}  exited with status {}",
                            outcome.status
                        ));
                    }
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
                    notices.push(format!("{host:width$}  ERROR {error}"));
                }
            }
        }
    }
    if let Some(task) = output_task {
        task.await.map_err(io::Error::other)?;
    }
    for notice in notices {
        eprintln!("{notice}");
    }

    let jump_summary = (jump_count > 0).then(|| {
        format!(
            " · {jump_count} jump{} reused",
            if jump_count == 1 { "" } else { "s" }
        )
    });
    let elapsed = started.elapsed().as_secs_f64();
    if json {
        results.sort_by(|left, right| left["host"].as_str().cmp(&right["host"].as_str()));
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "results": results,
                "summary": {
                    "hosts": total,
                    "succeeded": succeeded,
                    "failed": failed,
                    "jumps_reused": jump_count,
                    "elapsed_seconds": elapsed,
                }
            }))
            .map_err(io::Error::other)?
        );
    } else {
        eprintln!(
            "\n{total} hosts · {succeeded} succeeded · {failed} failed{} · {elapsed:.2}s",
            jump_summary.as_deref().unwrap_or_default(),
        );
    }
    Ok(if failed == 0 { 0 } else { 1 })
}

async fn print_fleet_stream(mut input: mpsc::UnboundedReceiver<FleetOutput>, width: usize) {
    let mut buffers: HashMap<(String, bool), Vec<u8>> = HashMap::new();
    while let Some(chunk) = input.recv().await {
        let key = (chunk.host, chunk.stderr);
        let buffer = buffers.entry(key.clone()).or_default();
        buffer.extend_from_slice(&chunk.data);
        while let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
            let line = buffer.drain(..=newline).collect::<Vec<_>>();
            print_fleet_line(&key.0, width, &line[..line.len() - 1], key.1);
        }
    }
    for ((host, stderr), buffer) in buffers {
        if !buffer.is_empty() {
            print_fleet_line(&host, width, &buffer, stderr);
        }
    }
}

fn print_fleet_line(host: &str, width: usize, line: &[u8], stderr: bool) {
    let line = String::from_utf8_lossy(line);
    if stderr {
        eprintln!("{host:width$}  {line}");
    } else {
        println!("{host:width$}  {line}");
    }
}

#[cfg(test)]
mod tests {
    use super::default_tool_arguments;
    use std::ffi::{OsStr, OsString};

    #[test]
    fn gives_eza_human_friendly_defaults_without_overriding_choices() {
        assert_eq!(
            default_tool_arguments(OsStr::new("eza"), &[OsString::from("/export")]),
            ["--long", "--color=always", "/export"]
                .map(OsString::from)
                .to_vec()
        );
        assert_eq!(
            default_tool_arguments(
                OsStr::new("eza"),
                &[OsString::from("--tree"), OsString::from("--color=never")]
            ),
            ["--tree", "--color=never"].map(OsString::from).to_vec()
        );
    }
}
