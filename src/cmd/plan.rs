use super::runtime::{human_bytes, toolbox_candidates};
use binport::ssh::{Destination, select_hosts};
use clap::Args;
use std::ffi::OsString;
use std::fs;
use std::io;

#[derive(Debug, Args)]
pub struct PlanArgs {
    /// SSH host, @group, or @all
    target: String,
    /// Toolbox tool to inspect
    tool: OsString,
}

pub fn run(args: PlanArgs, json: bool) -> io::Result<u8> {
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
    let destinations = hosts
        .iter()
        .map(|host| Destination::resolve(host).map(|destination| (host, destination)))
        .collect::<io::Result<Vec<_>>>()?;
    if json {
        let hosts = destinations
            .iter()
            .map(|(host, destination)| {
                serde_json::json!({
                    "host": host,
                    "destination": format!("{}@{}:{}", destination.user, destination.hostname, destination.port),
                    "proxy_jump": destination.proxy_jump,
                })
            })
            .collect::<Vec<_>>();
        let artifacts = candidates
            .iter()
            .map(|candidate| {
                Ok(serde_json::json!({
                    "platform": candidate.platform.name(),
                    "local_path": candidate.local_path,
                    "size_bytes": fs::metadata(&candidate.local_path)?.len(),
                    "remote_path": candidate.remote_file,
                }))
            })
            .collect::<io::Result<Vec<_>>>()?;
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "network_connections": 0,
                "hosts": hosts,
                "artifacts": artifacts,
            }))
            .map_err(io::Error::other)?
        );
    } else {
        println!("HOST\tDESTINATION\tROUTE");
        for (host, destination) in destinations {
            println!(
                "{host}\t{}@{}:{}\t{}",
                destination.user,
                destination.hostname,
                destination.port,
                destination.proxy_jump.as_deref().unwrap_or("direct")
            );
        }
        println!("\nARTIFACT\tSIZE\tREMOTE CACHE PATH");
        for candidate in candidates {
            println!(
                "{}\t{}\t{}",
                candidate.platform.name(),
                human_bytes(fs::metadata(&candidate.local_path)?.len()),
                candidate.remote_file
            );
        }
        println!("\nPlan only · no network connections made");
    }
    Ok(0)
}
