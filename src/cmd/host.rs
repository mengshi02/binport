use binport::catalog::Platform;
use binport::probe::{Capability, CapabilityState, ProbeReport};
use binport::ssh::{BastionProxy, Destination};
use clap::{Args, Subcommand};
use std::env;
use std::fs;
use std::io::{self, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

#[derive(Debug, Args)]
pub struct HostArgs {
    #[command(subcommand)]
    command: HostCommand,
}

#[derive(Debug, Subcommand)]
enum HostCommand {
    /// Add a host or update one with --force
    Add(HostAddArgs),
    /// List hosts managed by binport
    Ls,
    /// Show one managed host
    Show(HostNameArgs),
    /// Test connection, route, and remote platform
    Test(HostNameArgs),
    /// Remove one managed host
    Remove(HostNameArgs),
}

#[derive(Debug, Args)]
struct HostAddArgs {
    /// Alias used in binport and SSH commands
    name: String,
    /// Hostname, IP address, or USER@HOST
    destination: Option<String>,
    /// SSH username (overrides USER@HOST)
    #[arg(long)]
    user: Option<String>,
    /// SSH port
    #[arg(long, default_value_t = 22)]
    port: u16,
    /// Existing SSH alias used as ProxyJump
    #[arg(long, conflicts_with_all = ["bastion", "bastion_user", "bastion_account", "bastion_port", "bastion_format", "bastion_preset"])]
    jump: Option<String>,
    /// Use credentials held on the jump host through the native Rust helper
    #[arg(long, requires = "jump")]
    exec_hop: bool,
    /// Bastion host IP or hostname (cannot be combined with --jump)
    #[arg(long, conflicts_with = "jump")]
    bastion: Option<String>,
    /// Bastion login username
    #[arg(long, requires = "bastion")]
    bastion_user: Option<String>,
    /// Target account on the destination via bastion
    #[arg(long, requires = "bastion")]
    bastion_account: Option<String>,
    /// Bastion SSH port
    #[arg(long, requires = "bastion")]
    bastion_port: Option<u16>,
    /// Built-in bastion login preset (see `binport bastion presets`)
    #[arg(long, requires = "bastion", conflicts_with = "bastion_format")]
    bastion_preset: Option<String>,
    /// Composite username template (placeholders: {user}, {host}, {account})
    #[arg(long, requires = "bastion", conflicts_with = "bastion_preset")]
    bastion_format: Option<String>,
    /// Update an existing binport-managed alias
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Args)]
struct HostNameArgs {
    /// Exact SSH config alias or user@host destination
    host: String,
}

pub fn run(args: HostArgs, use_password: bool, json: bool) -> io::Result<u8> {
    match args.command {
        HostCommand::Add(args) => add(args, use_password, json),
        HostCommand::Ls => list(json),
        HostCommand::Show(args) => show(&args.host, json),
        HostCommand::Test(args) => test(&args.host, use_password, json),
        HostCommand::Remove(args) => remove(&args.host, json),
    }
}

fn add(args: HostAddArgs, use_password: bool, json: bool) -> io::Result<u8> {
    if args.destination.is_none() {
        if json {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "interactive host setup cannot be combined with --json; provide DESTINATION",
            ));
        }
        return add_interactive(args, use_password);
    }
    add_non_interactive(args, json)
}

fn add_non_interactive(args: HostAddArgs, json: bool) -> io::Result<u8> {
    if args.port == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SSH port must be greater than zero",
        ));
    }
    if args.bastion_port.unwrap_or(0) == 0 && args.bastion.is_some() {
        // default is fine, but explicit 0 is not
        if args.bastion_port == Some(0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "bastion port must be greater than zero",
            ));
        }
    }
    let destination = args.destination.as_deref().expect("destination checked");
    let (destination_user, hostname) = destination
        .split_once('@')
        .map_or((None, destination), |(user, host)| (Some(user), host));
    let user = args
        .user
        .as_deref()
        .or(destination_user)
        .map(str::to_owned)
        .or_else(|| env::var("USER").ok())
        .or_else(|| env::var("USERNAME").ok())
        .unwrap_or_else(|| "root".into());
    let bastion_preset = args.bastion_preset.clone();
    let bastion_format = binport::bastion::resolve_format(
        args.bastion_preset.as_deref(),
        args.bastion_format.as_deref(),
    )?;
    let entry = binport::host::HostEntry {
        name: args.name,
        hostname: hostname.to_owned(),
        user,
        port: args.port,
        proxy_jump: args.jump,
        strategy: args.exec_hop.then(|| "exec-hop".to_owned()),
        bastion_proxy: args.bastion,
        bastion_user: args.bastion_user,
        bastion_account: args.bastion_account,
        bastion_port: args.bastion_port,
        bastion_preset,
        bastion_format,
    };
    binport::host::add(entry.clone(), args.force)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&host_json(&entry)).map_err(io::Error::other)?
        );
    } else {
        println!("Added SSH host {}", entry.name);
        println!(
            "Config: {}",
            binport::host::managed_config_path()?.display()
        );
        println!();
        println!("  binport host test {}", entry.name);
        println!("  binport {} rg --version", entry.name);
    }
    Ok(0)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SetupMode {
    Direct,
    Jump,
    Bastion,
    Auto,
}

fn add_interactive(args: HostAddArgs, use_password: bool) -> io::Result<u8> {
    if args.jump.is_some()
        || args.bastion.is_some()
        || args.exec_hop
        || args.bastion_user.is_some()
        || args.bastion_account.is_some()
        || args.bastion_port.is_some()
        || args.bastion_preset.is_some()
        || args.bastion_format.is_some()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "when DESTINATION is omitted, configure the route in the interactive wizard instead of passing route flags",
        ));
    }

    println!("Configure {}\n", args.name);
    let target = prompt_required("Target (USER@HOST): ")?;
    let (target_user, target_host) = split_destination(&target);
    let target_user = args
        .user
        .clone()
        .or(target_user)
        .unwrap_or_else(default_user);

    println!("\nHow is it reached?");
    println!("  1. Direct SSH");
    println!("  2. SSH jump host");
    println!("  3. Enterprise bastion");
    println!("  4. Auto detect (recommended)");
    let mode = parse_setup_mode(&prompt_default("Select", "4")?)?;

    let mut extra_entry = None;
    let (proxy_jump, bastion) = match mode {
        SetupMode::Direct => (None, None),
        SetupMode::Jump => {
            let (alias, entry) =
                configure_entry_host(&args.name, "Jump host (USER@HOST or SSH alias): ")?;
            extra_entry = entry;
            (Some(alias), None)
        }
        SetupMode::Bastion => {
            let proxy = configure_bastion()?;
            (None, Some(proxy))
        }
        SetupMode::Auto => {
            print!("\nTesting direct reachability... ");
            io::stdout().flush()?;
            if tcp_reachable(&target_host, 22, Duration::from_secs(2)) {
                println!("reachable");
                println!("Detected route: direct SSH");
                (None, None)
            } else {
                println!("not reachable");
                let first = prompt_required("What do you normally connect to first? ")?;
                println!("Entry type:");
                println!("  1. Normal SSH host");
                println!("  2. Enterprise bastion");
                let entry_type = prompt_default("Select", "1")?;
                match entry_type.trim() {
                    "1" => {
                        let (alias, entry) = entry_host_from_input(&args.name, &first)?;
                        extra_entry = entry;
                        (Some(alias), None)
                    }
                    "2" => (None, Some(configure_bastion_from(first)?)),
                    _ => return Err(invalid_choice("entry type", &entry_type)),
                }
            }
        }
    };

    let (
        bastion_proxy,
        bastion_user,
        bastion_account,
        bastion_port,
        bastion_preset,
        bastion_format,
    ) = if let Some(proxy) = bastion {
        (
            Some(proxy.host),
            Some(proxy.user),
            Some(proxy.account),
            proxy.port,
            proxy.preset,
            Some(proxy.format),
        )
    } else {
        (None, None, None, None, None, None)
    };

    let mut entry = binport::host::HostEntry {
        name: args.name,
        hostname: target_host,
        user: target_user,
        port: args.port,
        proxy_jump,
        strategy: None,
        bastion_proxy,
        bastion_user,
        bastion_account,
        bastion_port,
        bastion_preset,
        bastion_format,
    };

    println!("\nConfiguration");
    println!("  Target: {}@{}:{}", entry.user, entry.hostname, entry.port);
    println!("  Route:  {}", route_label(&entry));
    let mut probe = ProbeDecision {
        offer_exec_hop: false,
        entry_password: None,
    };
    if confirm("Test this route before saving?", true)? {
        println!("\nTesting capabilities...");
        probe = probe_before_save(&entry, extra_entry.as_ref(), use_password)?;
        if probe.offer_exec_hop
            && confirm(
                "Use native exec-hop with credentials held on the entry host?",
                true,
            )?
        {
            entry.strategy = Some("exec-hop".to_owned());
            println!("  ✓ Strategy: exec-hop");
        }
    }
    if !confirm("Save this configuration?", true)? {
        println!("Cancelled; no configuration was written.");
        return Ok(0);
    }
    if let Some(jump) = extra_entry {
        binport::host::add(jump, false)?;
    }
    binport::host::add(entry.clone(), args.force)?;
    println!("\n✓ Host saved as {:?}", entry.name);
    let mut needs_password = false;
    if entry.strategy.as_deref() == Some("exec-hop")
        && let (Some(jump_alias), Some(password)) =
            (entry.proxy_jump.as_deref(), probe.entry_password.as_deref())
    {
        println!("\nThe entry-host password was used only for this test and was not saved.");
        if confirm(
            &format!("Set up passwordless access to entry host {jump_alias:?} now?"),
            true,
        )? {
            setup_passwordless_entry(jump_alias, password)?;
        } else {
            needs_password = true;
        }
    }
    println!("\nTry:");
    if needs_password {
        println!("  binport --password {} rg --version", entry.name);
    } else {
        println!("  binport {} rg --version", entry.name);
    }
    Ok(0)
}

struct ProbeDecision {
    offer_exec_hop: bool,
    entry_password: Option<String>,
}

fn probe_before_save(
    entry: &binport::host::HostEntry,
    unsaved_jump: Option<&binport::host::HostEntry>,
    use_password: bool,
) -> io::Result<ProbeDecision> {
    let runtime = tokio::runtime::Runtime::new().map_err(io::Error::other)?;
    if let Some(jump_alias) = &entry.proxy_jump {
        let jump_destination = match unsaved_jump {
            Some(jump) => destination_from_entry(jump)?,
            None => Destination::resolve(jump_alias)?,
        };
        let mut target_destination = destination_from_entry(entry)?;
        target_destination.proxy_jump = None;
        let jump_password = use_password
            .then(|| {
                rpassword::prompt_password(format!(
                    "Password for {}@{} (leave empty for key/agent): ",
                    jump_destination.user, jump_destination.hostname
                ))
            })
            .transpose()?
            .filter(|value| !value.is_empty());
        let target_password = use_password
            .then(|| {
                rpassword::prompt_password(format!(
                    "Password for {}@{} (leave empty for key/agent): ",
                    target_destination.user, target_destination.hostname
                ))
            })
            .transpose()?
            .filter(|value| !value.is_empty());
        let report = runtime.block_on(binport::probe::probe_jump_route(
            &jump_destination,
            &target_destination,
            jump_password.as_deref(),
            target_password.as_deref(),
        ));
        print_jump_probe_report(jump_alias, entry, &report);
        return Ok(ProbeDecision {
            offer_exec_hop: report.entry.state == CapabilityState::Supported
                && report.target.is_none(),
            entry_password: jump_password,
        });
    }

    let destination = destination_from_entry(entry)?;
    let password = use_password
        .then(|| rpassword::prompt_password("SSH password (leave empty for key/agent): "))
        .transpose()?
        .filter(|value| !value.is_empty());
    match runtime.block_on(binport::probe::probe_destination(
        &destination,
        password.as_deref(),
        true,
    )) {
        Ok(report) => print_probe_report(&entry.name, &report),
        Err(error) => {
            println!("  ✗ Connection: failed");
            println!("      {error}");
            println!("  ! The route may still be saved and tested later.");
        }
    }
    Ok(ProbeDecision {
        offer_exec_hop: false,
        entry_password: None,
    })
}

fn setup_passwordless_entry(alias: &str, password: &str) -> io::Result<()> {
    let key = binport::auth::ensure_managed_key(alias)?;
    let mut destination = Destination::resolve(alias)?;
    let runtime = tokio::runtime::Runtime::new().map_err(io::Error::other)?;
    runtime.block_on(async {
        let ssh = binport::ssh::NativeSsh::connect(&destination, Some(password)).await?;
        let (status, _, stderr) = ssh
            .execute_capture_with_input(
                binport::auth::install_key_command(),
                key.public_key.as_bytes().to_vec(),
            )
            .await?;
        if status != 0 {
            return Err(io::Error::other(format!(
                "entry-host key installation failed: {}",
                String::from_utf8_lossy(&stderr).trim()
            )));
        }
        destination.identity = Some(key.private_path.clone());
        let verification = binport::ssh::NativeSsh::connect(&destination, None).await?;
        let (status, _, stderr) = verification.execute_capture("true").await?;
        if status != 0 {
            return Err(io::Error::other(format!(
                "entry-host key verification failed: {}",
                stderr.trim()
            )));
        }
        Ok::<_, io::Error>(())
    })?;
    println!("  ✓ Passwordless access ready for entry host {alias:?}");
    Ok(())
}

fn destination_from_entry(entry: &binport::host::HostEntry) -> io::Result<Destination> {
    let mut destination = Destination::resolve(&format!("{}@{}", entry.user, entry.hostname))?;
    destination.port = entry.port;
    destination.proxy_jump = entry.proxy_jump.clone();
    destination.bastion_proxy = entry.bastion_proxy.as_ref().map(|host| BastionProxy {
        host: host.clone(),
        port: entry.bastion_port.unwrap_or(22),
        user: entry.bastion_user.clone().unwrap_or_default(),
        account: entry.bastion_account.clone().unwrap_or_default(),
        preset: entry.bastion_preset.clone(),
        format: entry
            .bastion_format
            .clone()
            .unwrap_or_else(|| "{user}/{host}/{account}".to_owned()),
    });
    Ok(destination)
}

fn print_jump_probe_report(
    jump_alias: &str,
    entry: &binport::host::HostEntry,
    report: &binport::probe::JumpProbeReport,
) {
    println!("Route capability report");
    println!("  Route: {jump_alias} -> {}", entry.hostname);
    print_capability("Entry host", &report.entry);
    print_capability("Forwarding", &report.direct_tcpip);
    if let Some(target) = &report.target {
        print_capability("Commands", &target.exec);
        print_capability("File stream", &target.file_stream);
    } else {
        println!("  ✗ Target:      unavailable");
        if let Some(detail) = &report.target_detail {
            println!("      {detail}");
        }
        if report.entry.state == CapabilityState::Supported
            && report.direct_tcpip.state == CapabilityState::Supported
        {
            println!("  ! The route works, but target credentials are not available locally.");
            println!("    Native exec-hop can use credentials held on the entry host.");
        } else if report.entry.state == CapabilityState::Supported
            && report.direct_tcpip.state != CapabilityState::Supported
        {
            println!(
                "  ! Native forwarding is unavailable; exec-hop can try the route from inside the entry host."
            );
        }
    }
}

#[derive(Debug)]
struct WizardBastion {
    host: String,
    user: String,
    account: String,
    port: Option<u16>,
    preset: Option<String>,
    format: String,
}

fn configure_bastion() -> io::Result<WizardBastion> {
    configure_bastion_from(prompt_required("Bastion (USER@HOST): ")?)
}

fn configure_bastion_from(input: String) -> io::Result<WizardBastion> {
    let (input_user, host) = split_destination(&input);
    let user = input_user.map_or_else(|| prompt_required("Bastion user: "), Ok)?;
    let account = prompt_default("Target account", "root")?;
    println!("Bastion preset:");
    for (index, preset) in binport::bastion::presets().iter().enumerate() {
        println!("  {}. {} ({})", index + 1, preset.name, preset.product);
    }
    println!(
        "  {}. Custom composite username",
        binport::bastion::presets().len() + 1
    );
    println!(
        "  {}. Interactive menu (detect only in this release)",
        binport::bastion::presets().len() + 2
    );
    let selection = prompt_default("Select", "1")?;
    let selected = selection
        .trim()
        .parse::<usize>()
        .map_err(|_| invalid_choice("bastion preset", &selection))?;
    if selected == binport::bastion::presets().len() + 2 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "interactive-menu bastions are detected but cannot be saved for automatic replay in this release",
        ));
    }
    let (preset, format) = if selected == binport::bastion::presets().len() + 1 {
        (None, prompt_required("Composite username format: ")?)
    } else {
        let preset = binport::bastion::presets()
            .get(selected.saturating_sub(1))
            .ok_or_else(|| invalid_choice("bastion preset", &selection))?;
        (Some(preset.name.to_owned()), preset.format.to_owned())
    };
    Ok(WizardBastion {
        host,
        user,
        account,
        port: None,
        preset,
        format,
    })
}

fn configure_entry_host(
    target_name: &str,
    label: &str,
) -> io::Result<(String, Option<binport::host::HostEntry>)> {
    entry_host_from_input(target_name, &prompt_required(label)?)
}

fn entry_host_from_input(
    target_name: &str,
    input: &str,
) -> io::Result<(String, Option<binport::host::HostEntry>)> {
    if !input.contains('@') && is_known_ssh_alias(input)? {
        return Ok((input.to_owned(), None));
    }
    let (user, host) = split_destination(input);
    let alias = prompt_default("Name this entry host", &format!("{target_name}-jump"))?;
    let entry = binport::host::HostEntry {
        name: alias.clone(),
        hostname: host,
        user: user.unwrap_or_else(default_user),
        port: 22,
        proxy_jump: None,
        strategy: None,
        bastion_proxy: None,
        bastion_user: None,
        bastion_account: None,
        bastion_port: None,
        bastion_preset: None,
        bastion_format: None,
    };
    Ok((alias, Some(entry)))
}

fn is_known_ssh_alias(name: &str) -> io::Result<bool> {
    if binport::host::find(name)?.is_some() {
        return Ok(true);
    }
    match fs::read_to_string(binport::host::main_config_path()?) {
        Ok(source) => Ok(binport::host::contains_alias(&source, name)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn split_destination(value: &str) -> (Option<String>, String) {
    value.trim().split_once('@').map_or_else(
        || (None, value.trim().to_owned()),
        |(user, host)| (Some(user.to_owned()), host.to_owned()),
    )
}

fn default_user() -> String {
    env::var("USER")
        .ok()
        .or_else(|| env::var("USERNAME").ok())
        .unwrap_or_else(|| "root".into())
}

fn parse_setup_mode(value: &str) -> io::Result<SetupMode> {
    match value.trim() {
        "1" => Ok(SetupMode::Direct),
        "2" => Ok(SetupMode::Jump),
        "3" => Ok(SetupMode::Bastion),
        "4" => Ok(SetupMode::Auto),
        _ => Err(invalid_choice("connection type", value)),
    }
}

fn prompt_required(label: &str) -> io::Result<String> {
    let value = prompt(label)?;
    if value.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label} is required"),
        ));
    }
    Ok(value)
}

fn prompt_default(label: &str, default: &str) -> io::Result<String> {
    let value = prompt(&format!("{label} [{default}]: "))?;
    Ok(if value.is_empty() {
        default.to_owned()
    } else {
        value
    })
}

fn prompt(label: &str) -> io::Result<String> {
    print!("{label}");
    io::stdout().flush()?;
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    Ok(value.trim().to_owned())
}

fn confirm(label: &str, default: bool) -> io::Result<bool> {
    let suffix = if default { "Y/n" } else { "y/N" };
    let value = prompt(&format!("{label} [{suffix}] "))?;
    match value.to_ascii_lowercase().as_str() {
        "" => Ok(default),
        "y" | "yes" => Ok(true),
        "n" | "no" => Ok(false),
        _ => Err(invalid_choice(label, &value)),
    }
}

fn invalid_choice(label: &str, value: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("invalid {label}: {value:?}"),
    )
}

fn tcp_reachable(host: &str, port: u16, timeout: Duration) -> bool {
    let Ok(addresses) = format!("{host}:{port}").to_socket_addrs() else {
        return false;
    };
    addresses
        .into_iter()
        .any(|address: SocketAddr| TcpStream::connect_timeout(&address, timeout).is_ok())
}

fn list(json: bool) -> io::Result<u8> {
    let entries = binport::host::list()?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &entries
                    .iter()
                    .map(host_json)
                    .collect::<Vec<serde_json::Value>>()
            )
            .map_err(io::Error::other)?
        );
    } else {
        let rows = entries
            .into_iter()
            .map(|entry| {
                let route = route_label(&entry);
                vec![
                    entry.name,
                    format!("{}@{}:{}", entry.user, entry.hostname, entry.port),
                    route,
                ]
            })
            .collect::<Vec<_>>();
        print!(
            "{}",
            super::table::render(&["HOST", "DESTINATION", "ROUTE"], &rows)
        );
    }
    Ok(0)
}

fn show(name: &str, json: bool) -> io::Result<u8> {
    let entry = binport::host::find(name)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("host {name:?} is not managed by binport"),
        )
    })?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&host_json(&entry)).map_err(io::Error::other)?
        );
    } else {
        println!("Host: {}", entry.name);
        println!(
            "Destination: {}@{}:{}",
            entry.user, entry.hostname, entry.port
        );
        println!("Route: {}", route_label(&entry));
        println!(
            "Config: {}",
            binport::host::managed_config_path()?.display()
        );
    }
    Ok(0)
}

fn remove(name: &str, json: bool) -> io::Result<u8> {
    if !binport::host::remove(name)? {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("host {name:?} is not managed by binport"),
        ));
    }
    if json {
        println!(
            "{{\"host\":{},\"removed\":true}}",
            serde_json::to_string(name).unwrap()
        );
    } else {
        println!("Removed SSH host {name}");
    }
    Ok(0)
}

fn test(name: &str, use_password: bool, json: bool) -> io::Result<u8> {
    if let Some(entry) = binport::host::find(name)?
        && entry.strategy.as_deref() == Some("exec-hop")
    {
        return test_exec_hop(&entry, use_password, json);
    }
    let destination = Destination::resolve(name)?;
    let password = use_password
        .then(|| rpassword::prompt_password("SSH password: "))
        .transpose()?;
    let runtime = tokio::runtime::Runtime::new().map_err(io::Error::other)?;
    let report = runtime.block_on(binport::probe::probe_destination(
        &destination,
        password.as_deref(),
        true,
    ))?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "host": name,
                "destination": report.destination,
                "proxy_jump": destination.proxy_jump,
                "bastion_proxy": destination.bastion_proxy.as_ref().map(|b| &b.host),
                "route": report.route,
                "platform": report.platform,
                "capabilities": {
                    "connect": capability_json(&report.connect),
                    "exec": capability_json(&report.exec),
                    "file_stream": capability_json(&report.file_stream),
                    "direct_tcpip": capability_json(&report.direct_tcpip),
                },
                "ok": report.command_ready(),
            }))
            .map_err(io::Error::other)?
        );
    } else {
        print_probe_report(name, &report);
    }
    Ok(u8::from(!report.command_ready()))
}

fn test_exec_hop(
    entry: &binport::host::HostEntry,
    use_password: bool,
    json: bool,
) -> io::Result<u8> {
    let jump_alias = entry.proxy_jump.as_deref().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "exec-hop host has no entry host",
        )
    })?;
    let password = use_password
        .then(|| rpassword::prompt_password("Entry-host SSH password: "))
        .transpose()?;
    let runtime = tokio::runtime::Runtime::new().map_err(io::Error::other)?;
    let started = std::time::Instant::now();
    let (command_result, stream_result) = runtime.block_on(async {
        let hop = binport::hop::ExecHop::connect_host(entry, password.as_deref(), !json).await?;
        let command = hop
            .execute_capture_with_input(
                "printf 'BINPORT_HOP_OK\\n'; uname -s; uname -m".to_owned(),
                Vec::new(),
            )
            .await?;
        let marker = b"BINPORT_HOP_STREAM_3d6a".to_vec();
        let stream = hop
            .execute_capture_with_input("cat".to_owned(), marker.clone())
            .await?;
        Ok::<_, io::Error>((command, (stream, marker)))
    })?;
    let elapsed_ms = started.elapsed().as_millis();
    let (status, stdout, stderr) = command_result;
    let mut lines = stdout.split(|byte| *byte == b'\n');
    let marker = lines.next().unwrap_or_default();
    let os = String::from_utf8_lossy(lines.next().unwrap_or_default());
    let arch = String::from_utf8_lossy(lines.next().unwrap_or_default());
    let command_ok = status == 0 && marker == b"BINPORT_HOP_OK";
    let ((stream_status, stream_stdout, stream_stderr), expected_stream) = stream_result;
    let stream_ok = stream_status == 0 && stream_stdout == expected_stream;
    let platform = Platform::from_uname(&os, &arch)
        .map(Platform::name)
        .unwrap_or("unsupported");
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "host": entry.name,
                "destination": format!("{}@{}:{}", entry.user, entry.hostname, entry.port),
                "route": format!("exec-hop:{jump_alias}"),
                "platform": platform,
                "capabilities": {
                    "entry_and_helper": "supported",
                    "exec": if command_ok { "supported" } else { "failed" },
                    "file_stream": if stream_ok { "supported" } else { "failed" },
                    "direct_tcpip": "not-applicable",
                    "relay": "available",
                },
                "elapsed_ms": elapsed_ms,
                "ok": command_ok && stream_ok,
            }))
            .map_err(io::Error::other)?
        );
    } else {
        println!("Host capability report");
        println!("  Host:        {}", entry.name);
        println!(
            "  Destination: {}@{}:{}",
            entry.user, entry.hostname, entry.port
        );
        println!("  Route:       exec-hop:{jump_alias}");
        println!("  Platform:    {platform}");
        println!("  ✓ Entry/helper: supported ({elapsed_ms} ms)");
        println!(
            "  {} Commands:     {}",
            if command_ok { "✓" } else { "✗" },
            if command_ok { "supported" } else { "failed" }
        );
        println!(
            "  {} File stream:  {}",
            if stream_ok { "✓" } else { "✗" },
            if stream_ok { "supported" } else { "failed" }
        );
        println!("  ✓ TCP relay:    available (tested when a tunnel is opened)");
        if !command_ok && !stderr.is_empty() {
            println!("      {}", String::from_utf8_lossy(&stderr).trim());
        }
        if !stream_ok && !stream_stderr.is_empty() {
            println!("      {}", String::from_utf8_lossy(&stream_stderr).trim());
        }
    }
    Ok(u8::from(!command_ok || !stream_ok))
}

fn capability_json(capability: &Capability) -> serde_json::Value {
    serde_json::json!({
        "state": capability.state.label(),
        "detail": capability.detail,
        "elapsed_ms": capability.elapsed_ms,
    })
}

fn print_probe_report(name: &str, report: &ProbeReport) {
    println!("Host capability report");
    println!("  Host:        {name}");
    println!("  Destination: {}", report.destination);
    println!("  Route:       {}", report.route);
    println!(
        "  Platform:    {}",
        report.platform.as_deref().unwrap_or("unknown")
    );
    print_capability("Connection", &report.connect);
    print_capability("Commands", &report.exec);
    print_capability("File stream", &report.file_stream);
    print_capability("TCP tunnel", &report.direct_tcpip);
    if report.direct_tcpip.state == CapabilityState::Denied {
        println!("  Relay:       not implemented for this route");
    }
}

fn print_capability(label: &str, capability: &Capability) {
    let elapsed = capability
        .elapsed_ms
        .map(|value| format!(" ({value} ms)"))
        .unwrap_or_default();
    println!(
        "  {} {:<11} {}{}",
        capability.state.symbol(),
        format!("{label}:"),
        capability.state.label(),
        elapsed
    );
    if let Some(detail) = &capability.detail {
        println!("      {detail}");
    }
}

fn host_json(entry: &binport::host::HostEntry) -> serde_json::Value {
    serde_json::json!({
        "host": entry.name,
        "hostname": entry.hostname,
        "user": entry.user,
        "port": entry.port,
        "proxy_jump": entry.proxy_jump,
        "strategy": entry.strategy,
        "bastion_proxy": entry.bastion_proxy,
        "bastion_user": entry.bastion_user,
        "bastion_account": entry.bastion_account,
        "bastion_port": entry.bastion_port,
        "bastion_preset": entry.bastion_preset,
        "bastion_format": entry.bastion_format,
    })
}

fn route_label(entry: &binport::host::HostEntry) -> String {
    if let Some(jump) = &entry.proxy_jump {
        return if entry.strategy.as_deref() == Some("exec-hop") {
            let mut chain = vec![jump.clone()];
            let mut cursor = jump.clone();
            while chain.len() < 4 {
                let Ok(Some(parent)) = binport::host::find(&cursor) else {
                    break;
                };
                if parent.strategy.as_deref() != Some("exec-hop") {
                    break;
                }
                let Some(next) = parent.proxy_jump else {
                    break;
                };
                if chain.contains(&next) {
                    break;
                }
                cursor = next.clone();
                chain.push(next);
            }
            chain.reverse();
            format!("exec-hop:{}", chain.join(" -> "))
        } else {
            format!("jump:{jump}")
        };
    }
    if let Some(bastion) = &entry.bastion_proxy {
        return format!("bastion:{bastion}");
    }
    "direct".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_setup_modes_and_rejects_unknown_choices() {
        assert_eq!(parse_setup_mode("1").unwrap(), SetupMode::Direct);
        assert_eq!(parse_setup_mode("2").unwrap(), SetupMode::Jump);
        assert_eq!(parse_setup_mode("3").unwrap(), SetupMode::Bastion);
        assert_eq!(parse_setup_mode("4").unwrap(), SetupMode::Auto);
        assert!(parse_setup_mode("5").is_err());
    }

    #[test]
    fn splits_user_from_host_without_changing_bare_hosts() {
        assert_eq!(
            split_destination("root@10.0.0.5"),
            (Some("root".to_owned()), "10.0.0.5".to_owned())
        );
        assert_eq!(
            split_destination("jumpserver-51"),
            (None, "jumpserver-51".to_owned())
        );
    }
}
