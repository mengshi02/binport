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
    #[arg(long)]
    jump: Option<String>,
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
    let entry = binport::host::HostEntry {
        name: args.name,
        hostname: hostname.to_owned(),
        user,
        port: args.port,
        proxy_jump: args.jump,
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
            println!(
                "{}\t{}@{}:{}\t{}",
                entry.name,
                entry.user,
                entry.hostname,
                entry.port,
                entry.proxy_jump.as_deref().unwrap_or("direct")
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
        println!("Route: {}", entry.proxy_jump.as_deref().unwrap_or("direct"));
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
                "platform": platform,
                "latency_ms": latency_ms,
                "ok": true,
            }))
            .map_err(io::Error::other)?
        );
    } else {
        println!("✓ Host       {name}");
        println!(
            "✓ Route      {}",
            destination
                .proxy_jump
                .as_deref()
                .map_or_else(|| "direct".to_owned(), |jump| format!("{jump} → {name}"))
        );
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
    })
}
