use super::runtime::ad_hoc_route;
use binport::progress::TransferProgress;
use binport::remote_command;
use binport::ssh::{Destination, NativeSsh};
use clap::Args;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Args)]
pub struct CpArgs {
    /// Local path or HOST:PATH
    source: String,
    /// Local path or HOST:PATH
    destination: String,
}

#[derive(Debug, Args)]
pub struct RmArgs {
    /// Remote path in HOST:PATH form
    target: String,
    /// Remove directories and their contents
    #[arg(short = 'r', long)]
    recursive: bool,
    /// Ignore a missing path
    #[arg(short = 'f', long)]
    force: bool,
}

#[derive(Debug, Eq, PartialEq)]
struct RemoteFile<'a> {
    host: &'a str,
    path: &'a str,
}

pub fn copy(args: CpArgs, use_password: bool, json: bool) -> io::Result<u8> {
    let source = remote_file(&args.source)?;
    let destination = remote_file(&args.destination)?;
    if source.is_none() && destination.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "at least one cp path must use HOST:PATH",
        ));
    }
    let password = use_password
        .then(|| rpassword::prompt_password("SSH password: "))
        .transpose()?;
    let runtime = tokio::runtime::Runtime::new().map_err(io::Error::other)?;
    let (source_path, remove_source) = match source {
        Some(source) => (
            runtime.block_on(download_remote_file(&source, password.as_deref(), !json))?,
            true,
        ),
        None => {
            let path = PathBuf::from(&args.source);
            if !path.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} is not a regular file", path.display()),
                ));
            }
            (path, false)
        }
    };
    let byte_count = fs::metadata(&source_path)?.len();
    let result = match destination {
        Some(destination) => {
            let name = source_name(&args.source)?;
            runtime.block_on(upload_remote_file(
                &destination,
                &name,
                &source_path,
                password.as_deref(),
                !json,
            ))
        }
        None => write_local_file(&args.destination, &args.source, &source_path),
    };
    if remove_source {
        let _ = fs::remove_file(&source_path);
    }
    result?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "source": args.source,
                "destination": args.destination,
                "bytes": byte_count,
                "ok": true,
            }))
            .map_err(io::Error::other)?
        );
    } else {
        println!(
            "Copied {} bytes: {} -> {}",
            byte_count, args.source, args.destination
        );
    }
    Ok(0)
}

pub fn remove(args: RmArgs, use_password: bool, json: bool) -> io::Result<u8> {
    let target = remote_file(&args.target)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "rm requires a remote path in HOST:PATH form",
        )
    })?;
    validate_remove_path(target.path)?;
    let password = use_password
        .then(|| rpassword::prompt_password("SSH password: "))
        .transpose()?;
    let runtime = tokio::runtime::Runtime::new().map_err(io::Error::other)?;
    let command = remote_command::remove(target.path, args.recursive, args.force)?;
    let (status, _, stderr) = runtime.block_on(async {
        connect_host(target.host, password.as_deref())
            .await?
            .execute_capture_with_input(&command, Vec::new())
            .await
    })?;
    if status != 0 {
        return Err(io::Error::other(format!(
            "remote remove failed for {}: {}",
            args.target,
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "target": args.target,
                "recursive": args.recursive,
                "ok": true,
            }))
            .map_err(io::Error::other)?
        );
    } else {
        println!("Removed {}", args.target);
    }
    Ok(0)
}

fn remote_file(value: &str) -> io::Result<Option<RemoteFile<'_>>> {
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

fn validate_remove_path(path: &str) -> io::Result<()> {
    let trimmed = path.trim_end_matches('/');
    if matches!(trimmed, "" | "." | ".." | "~" | "$HOME")
        || trimmed.split('/').any(|component| component == "..")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to remove dangerous path {path:?}"),
        ));
    }
    Ok(())
}

fn source_name(value: &str) -> io::Result<String> {
    let path = remote_file(value)?.map_or(value, |remote| remote.path);
    PathBuf::from(path)
        .file_name()
        .and_then(OsStr::to_str)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "source has no file name"))
}

async fn connect_host(host: &str, password: Option<&str>) -> io::Result<NativeSsh> {
    if let Some((jump_host, target_host)) = ad_hoc_route(host)? {
        let jump = NativeSsh::connect_jump(jump_host, password).await?;
        let destination = Destination::resolve(target_host)?;
        NativeSsh::connect_with_jump(&destination, password, &jump).await
    } else {
        NativeSsh::connect(&Destination::resolve(host)?, password).await
    }
}

async fn download_remote_file(
    source: &RemoteFile<'_>,
    password: Option<&str>,
    show_progress: bool,
) -> io::Result<PathBuf> {
    let ssh = connect_host(source.host, password).await?;
    let size_command = remote_command::file_size(source.path)?;
    let (size_status, size_stdout, size_stderr) = ssh.execute_capture(&size_command).await?;
    if size_status != 0 {
        return Err(io::Error::other(format!(
            "remote read failed for {}:{}: {}",
            source.host, source.path, size_stderr
        )));
    }
    let total = size_stdout.trim().parse::<u64>().ok();
    let command = remote_command::download_file(source.path)?;
    let temp = copy_temp_path();
    let progress = TransferProgress::new(
        format!("download {}:{}", source.host, source.path),
        total,
        show_progress,
    );
    let (status, stderr) = match ssh.download_file(&command, &temp, progress).await {
        Ok(result) => result,
        Err(error) => {
            let _ = fs::remove_file(&temp);
            return Err(error);
        }
    };
    if status != 0 {
        let _ = fs::remove_file(&temp);
        return Err(io::Error::other(format!(
            "remote read failed for {}:{}: {}",
            source.host,
            source.path,
            String::from_utf8_lossy(&stderr)
        )));
    }
    Ok(temp)
}

async fn upload_remote_file(
    destination: &RemoteFile<'_>,
    source_name: &str,
    source: &Path,
    password: Option<&str>,
    show_progress: bool,
) -> io::Result<()> {
    let ssh = connect_host(destination.host, password).await?;
    let command = remote_command::upload_file(destination.path, source_name)?;
    let total = fs::metadata(source)?.len();
    let progress = TransferProgress::new(
        format!("upload {}:{}", destination.host, destination.path),
        Some(total),
        show_progress,
    );
    let (status, stderr) = ssh.upload_file(&command, source, progress).await?;
    if status != 0 {
        return Err(io::Error::other(format!(
            "remote write failed for {}:{}: {}",
            destination.host,
            destination.path,
            String::from_utf8_lossy(&stderr)
        )));
    }
    Ok(())
}

fn write_local_file(destination: &str, source: &str, input: &Path) -> io::Result<()> {
    let mut path = PathBuf::from(destination);
    if path.is_dir() || destination.ends_with(std::path::MAIN_SEPARATOR) {
        path.push(source_name(source)?);
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension(format!("binport-part-{}", std::process::id()));
    fs::copy(input, &temp)?;
    fs::rename(temp, path)
}

fn copy_temp_path() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "binport-cp-{}-{}.part",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

#[cfg(test)]
mod tests {
    use super::{remote_file, validate_remove_path};

    #[test]
    fn distinguishes_remote_paths_from_windows_drives() {
        let remote = remote_file("server-a:/var/log/app.log").unwrap().unwrap();
        assert_eq!(remote.host, "server-a");
        assert_eq!(remote.path, "/var/log/app.log");
        assert!(remote_file(r"C:\temp\app.log").unwrap().is_none());
        assert!(remote_file("server-a:").is_err());
    }

    #[test]
    fn refuses_dangerous_remote_remove_paths() {
        for path in ["/", "////", ".", "..", "~", "$HOME/", "/tmp/../"] {
            assert!(validate_remove_path(path).is_err(), "accepted {path:?}");
        }
        assert!(validate_remove_path("/tmp/binport-test").is_ok());
    }
}
