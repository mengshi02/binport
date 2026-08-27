use clap::{Args, Subcommand};
use std::io;
use std::time::{Duration, Instant};

use binport::ssh::{Destination, NativeSsh};

#[derive(Debug, Args)]
pub struct BastionArgs {
    #[command(subcommand)]
    command: BastionCommand,
}

#[derive(Debug, Subcommand)]
enum BastionCommand {
    /// List built-in bastion compatibility presets
    Presets,
    /// Safely test a configured bastion route and its SSH capabilities
    Probe(BastionProbeArgs),
}

#[derive(Debug, Args)]
struct BastionProbeArgs {
    /// Managed host alias routed through BastionProxy
    host: String,
    /// Also request a direct-tcpip channel to the configured target SSH port
    #[arg(long)]
    check_forwarding: bool,
}

pub fn run(args: BastionArgs, use_password: bool, json: bool) -> io::Result<u8> {
    match args.command {
        BastionCommand::Presets => list_presets(json),
        BastionCommand::Probe(args) => probe(args, use_password, json),
    }
}

fn list_presets(json: bool) -> io::Result<u8> {
    let presets = binport::bastion::presets();
    if json {
        let values = presets
            .iter()
            .map(|preset| {
                serde_json::json!({
                    "name": preset.name,
                    "format": preset.format,
                    "product": preset.product,
                    "status": preset.status,
                    "source": preset.source,
                })
            })
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string_pretty(&values).map_err(io::Error::other)?
        );
    } else {
        println!("PRESET\tFORMAT\tPRODUCT\tSTATUS");
        for preset in presets {
            println!(
                "{}\t{}\t{}\t{}",
                preset.name, preset.format, preset.product, preset.status
            );
        }
    }
    Ok(0)
}

fn probe(args: BastionProbeArgs, use_password: bool, json: bool) -> io::Result<u8> {
    let destination = Destination::resolve(&args.host)?;
    let bastion = destination.bastion_proxy.as_ref().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "host {:?} is not configured with BastionProxy; add it with `binport host add --bastion ...`",
                args.host
            ),
        )
    })?;
    let preset = bastion
        .preset
        .as_deref()
        .and_then(binport::bastion::find_preset)
        .or_else(|| {
            let mut matches = binport::bastion::presets()
                .iter()
                .filter(|preset| preset.format == bastion.format);
            let first = matches.next();
            first.filter(|_| matches.next().is_none())
        });
    let password = use_password
        .then(|| rpassword::prompt_password("SSH password: "))
        .transpose()?;
    let started = Instant::now();
    let runtime = tokio::runtime::Runtime::new().map_err(io::Error::other)?;
    let result = runtime.block_on(async {
        let ssh = NativeSsh::connect(&destination, password.as_deref()).await?;
        let connected_ms = started.elapsed().as_millis();
        let exec_started = Instant::now();
        let exec_result = tokio::time::timeout(
            Duration::from_secs(8),
            ssh.execute_capture("printf BINPORT_PROBE_OK"),
        )
        .await;
        let (exec, exec_detail) = match exec_result {
            Ok(Ok((0, stdout, _))) if stdout == "BINPORT_PROBE_OK" => ("supported", None),
            Ok(Ok((status, _, stderr))) => {
                ("failed", Some(format!("exit {status}: {}", stderr.trim())))
            }
            Ok(Err(error)) => ("failed", Some(error.to_string())),
            Err(_) => ("timeout", Some("no result within 8 seconds".to_owned())),
        };
        let exec_ms = exec_started.elapsed().as_millis();
        let (forwarding, forwarding_detail) = if args.check_forwarding {
            let target = format!("{}:{}", destination.hostname, destination.port);
            let forwarding_ssh = if ssh.is_bastion() {
                ssh.reconnect().await?
            } else {
                ssh.clone()
            };
            match tokio::time::timeout(
                Duration::from_secs(5),
                forwarding_ssh
                    .client()
                    .open_direct_tcpip_channel(target.as_str(), None),
            )
            .await
            {
                Ok(Ok(_)) => ("supported", None),
                Ok(Err(error)) => ("denied", Some(error.to_string())),
                Err(_) => ("timeout", Some("no result within 5 seconds".to_owned())),
            }
        } else {
            ("not-checked", None)
        };
        Ok::<_, io::Error>((
            connected_ms,
            exec,
            exec_detail,
            exec_ms,
            forwarding,
            forwarding_detail,
        ))
    })?;
    let (connected_ms, exec, exec_detail, exec_ms, forwarding, forwarding_detail) = result;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "host": args.host,
                "bastion": bastion.host,
                "preset": preset.map(|value| value.name),
                "preset_status": preset.map(|value| value.status),
                "connect": "supported",
                "connect_ms": connected_ms,
                "exec": exec,
                "exec_ms": exec_ms,
                "exec_detail": exec_detail,
                "direct_tcpip": forwarding,
                "direct_tcpip_detail": forwarding_detail,
            }))
            .map_err(io::Error::other)?
        );
    } else {
        println!("Bastion capability report");
        println!("  Host:           {}", args.host);
        println!("  Bastion:        {}:{}", bastion.host, bastion.port);
        println!(
            "  Preset:         {}",
            preset.map_or("custom/unknown", |value| value.name)
        );
        println!("  Connection:     supported ({connected_ms} ms)");
        println!("  Exec:           {exec} ({exec_ms} ms)");
        if let Some(detail) = exec_detail {
            println!("  Exec detail:    {detail}");
        }
        println!("  direct-tcpip:   {forwarding}");
        if let Some(detail) = forwarding_detail {
            println!("  Forward detail: {detail}");
        }
    }
    Ok(u8::from(exec != "supported"))
}
