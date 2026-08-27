use binport::catalog::Platform;
use binport::ssh::{Destination, NativeSsh};
use clap::{Args, Subcommand};
use std::env;
use std::io;
use std::time::Instant;

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
    destination: String,
    /// SSH username (overrides USER@HOST)
    #[arg(long)]
    user: Option<String>,
    /// SSH port
    #[arg(long, default_value_t = 22)]
    port: u16,
    /// Existing SSH alias used as ProxyJump
    #[arg(long, conflicts_with_all = ["bastion", "bastion_user", "bastion_account", "bastion_port", "bastion_format", "bastion_preset"])]
    jump: Option<String>,
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
        HostCommand::Add(args) => add(args, json),
        HostCommand::Ls => list(json),
        HostCommand::Show(args) => show(&args.host, json),
        HostCommand::Test(args) => test(&args.host, use_password, json),
        HostCommand::Remove(args) => remove(&args.host, json),
    }
}

fn add(args: HostAddArgs, json: bool) -> io::Result<u8> {
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
    let (destination_user, hostname) = args
        .destination
        .split_once('@')
        .map_or((None, args.destination.as_str()), |(user, host)| {
            (Some(user), host)
        });
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
        println!("HOST\tDESTINATION\tROUTE");
        for entry in entries {
            let route = route_label(&entry);
            println!(
                "{}\t{}@{}:{}\t{route}",
                entry.name, entry.user, entry.hostname, entry.port,
            );
        }
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
    let destination = Destination::resolve(name)?;
    let password = use_password
        .then(|| rpassword::prompt_password("SSH password: "))
        .transpose()?;
    let started = Instant::now();
    let runtime = tokio::runtime::Runtime::new().map_err(io::Error::other)?;
    let (status, stdout, stderr) = runtime.block_on(async {
        NativeSsh::connect(&destination, password.as_deref())
            .await?
            .execute_capture("uname -s; uname -m")
            .await
    })?;
    if status != 0 {
        return Err(io::Error::other(format!(
            "host test failed: {}",
            stderr.trim()
        )));
    }
    let mut lines = stdout.lines();
    let os = lines.next().unwrap_or_default();
    let arch = lines.next().unwrap_or_default();
    let platform = Platform::from_uname(os, arch)
        .map(Platform::name)
        .unwrap_or("unsupported");
    let latency_ms = started.elapsed().as_millis();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "host": name,
                "destination": format!("{}@{}:{}", destination.user, destination.hostname, destination.port),
                "proxy_jump": destination.proxy_jump,
                "bastion_proxy": destination.bastion_proxy.as_ref().map(|b| &b.host),
                "platform": platform,
                "latency_ms": latency_ms,
                "ok": true,
            }))
            .map_err(io::Error::other)?
        );
    } else {
        let route = if let Some(jump) = &destination.proxy_jump {
            format!("{jump} → {name}")
        } else if let Some(bastion) = &destination.bastion_proxy {
            format!("bastion:{} → {name}", bastion.host)
        } else {
            "direct".to_owned()
        };
        println!("✓ Host       {name}");
        println!("✓ Route      {route}");
        println!("✓ Platform   {platform}");
        println!("✓ Latency    {latency_ms} ms");
    }
    Ok(0)
}

fn host_json(entry: &binport::host::HostEntry) -> serde_json::Value {
    serde_json::json!({
        "host": entry.name,
        "hostname": entry.hostname,
        "user": entry.user,
        "port": entry.port,
        "proxy_jump": entry.proxy_jump,
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
        return format!("jump:{jump}");
    }
    if let Some(bastion) = &entry.bastion_proxy {
        return format!("bastion:{bastion}");
    }
    "direct".to_owned()
}
