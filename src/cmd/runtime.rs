use binport::catalog::Platform;
use binport::hop::ExecHop;
use binport::ssh::{Destination, NativeSsh};
use binport::toolbox;
use binport::{remote_paths, safe_tool_name, sha256_file};
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::PathBuf;

#[derive(Debug, Eq, PartialEq)]
pub struct RemoteFile<'a> {
    pub host: &'a str,
    pub path: &'a str,
}

pub fn parse_remote_file(value: &str) -> io::Result<Option<RemoteFile<'_>>> {
    let Some((host, path)) = value.split_once(':') else {
        return Ok(None);
    };
    if host.len() == 1 && host.as_bytes()[0].is_ascii_alphabetic() {
        return Ok(None);
    }
    if host.is_empty() || path.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "remote paths use HOST:PATH with a non-empty host and path",
        ));
    }
    Ok(Some(RemoteFile { host, path }))
}

pub async fn connect_host(host: &str, password: Option<&str>) -> io::Result<NativeSsh> {
    if let Some((bastion_alias, target_alias)) = ad_hoc_bastion(host)? {
        let bastion_dest = Destination::resolve(bastion_alias)?;
        let mut target_dest = Destination::resolve(target_alias)?;
        apply_ad_hoc_bastion(&mut target_dest, &bastion_dest)?;
        return NativeSsh::connect(&target_dest, password).await;
    }
    if let Some((jump_host, target_host)) = ad_hoc_route(host)? {
        let jump = NativeSsh::connect_jump(jump_host, password).await?;
        let destination = Destination::resolve(target_host)?;
        return NativeSsh::connect_with_jump(&destination, password, &jump).await;
    }
    NativeSsh::connect(&Destination::resolve(host)?, password).await
}

pub async fn connect_exec_hop(
    host: &str,
    password: Option<&str>,
    show_progress: bool,
) -> io::Result<Option<ExecHop>> {
    let Some(entry) = binport::host::find(host)? else {
        return Ok(None);
    };
    if entry.strategy.as_deref() != Some("exec-hop") {
        return Ok(None);
    }
    ExecHop::connect_host(&entry, password, show_progress)
        .await
        .map(Some)
}

pub fn apply_ad_hoc_bastion(target: &mut Destination, bastion: &Destination) -> io::Result<()> {
    if target.proxy_jump.is_some() || target.bastion_proxy.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "target already has a proxy configured; ad-hoc bastion route is not applicable",
        ));
    }
    target.bastion_proxy = Some(binport::ssh::BastionProxy {
        host: bastion.hostname.clone(),
        port: bastion.port,
        user: bastion.user.clone(),
        account: target.user.clone(),
        preset: None,
        format: "{user}/{host}/{account}".into(),
    });
    Ok(())
}

#[derive(Clone, Debug)]
pub struct ToolCandidate {
    pub name: String,
    pub platform: Platform,
    pub local_path: PathBuf,
    pub directory: String,
    pub remote_file: String,
}

pub fn toolbox_candidates(tool: &OsStr) -> io::Result<Vec<ToolCandidate>> {
    let tool = tool.to_str().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "tool name is not valid UTF-8")
    })?;
    Ok(toolbox_all_candidates()?
        .into_iter()
        .filter(|candidate| candidate.name == tool)
        .collect())
}

pub fn toolbox_all_candidates() -> io::Result<Vec<ToolCandidate>> {
    let lock: toolbox::Lockfile =
        serde_json::from_slice(&fs::read(".binport/toolbox.json")?).map_err(io::Error::other)?;
    lock.tools
        .into_iter()
        .map(|entry| {
            let platform = Platform::parse(&entry.platform).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported platform {} in toolbox", entry.platform),
                )
            })?;
            let local_path = PathBuf::from(entry.path);
            let name = safe_tool_name(&local_path)?;
            let (directory, remote_file) = remote_paths(&sha256_file(&local_path)?, &name);
            Ok(ToolCandidate {
                name: entry.name,
                platform,
                local_path,
                directory,
                remote_file,
            })
        })
        .collect()
}

pub fn ad_hoc_route(host: &str) -> io::Result<Option<(&str, &str)>> {
    let Some((jump, target)) = host.split_once(',') else {
        return Ok(None);
    };
    if jump.is_empty() || target.is_empty() || target.contains(',') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ad-hoc routes use JUMP,TARGET with exactly two SSH aliases",
        ));
    }
    Ok(Some((jump, target)))
}

pub fn ad_hoc_bastion(host: &str) -> io::Result<Option<(&str, &str)>> {
    let Some((bastion, target)) = host.split_once('~') else {
        return Ok(None);
    };
    if bastion.is_empty() || target.is_empty() || target.contains('~') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ad-hoc bastion routes use BASTION~TARGET with exactly two SSH aliases",
        ));
    }
    Ok(Some((bastion, target)))
}

pub fn human_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1}MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1}KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes}B")
    }
}

pub fn write_prefixed(host: &str, width: usize, text: &str, stderr: bool) {
    for line in text.lines() {
        if stderr {
            eprintln!("{host:width$}  {line}");
        } else {
            println!("{host:width$}  {line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ad_hoc_bastion, ad_hoc_route};

    #[test]
    fn parses_exactly_one_ad_hoc_jump() {
        assert_eq!(
            ad_hoc_route("jump-a,server-a").unwrap(),
            Some(("jump-a", "server-a"))
        );
        assert_eq!(ad_hoc_route("server-a").unwrap(), None);
        assert!(ad_hoc_route("jump-a,server-a,server-b").is_err());
        assert!(ad_hoc_route("jump-a,").is_err());
    }

    #[test]
    fn parses_exactly_one_ad_hoc_bastion() {
        assert_eq!(
            ad_hoc_bastion("bastion-a~worker-1").unwrap(),
            Some(("bastion-a", "worker-1"))
        );
        assert_eq!(ad_hoc_bastion("worker-1").unwrap(), None);
        assert!(ad_hoc_bastion("bastion-a~worker-1~worker-2").is_err());
        assert!(ad_hoc_bastion("bastion-a~").is_err());
        assert!(ad_hoc_bastion("~worker-1").is_err());
    }
}
