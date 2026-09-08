use super::runtime::{RemoteFile, connect_exec_hop, connect_host, parse_remote_file};
use binport::progress::TransferProgress;
use binport::remote_command;
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
    /// Copy a directory tree recursively
    #[arg(short = 'r', long)]
    recursive: bool,
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

pub fn copy(args: CpArgs, use_password: bool, json: bool) -> io::Result<u8> {
    if args.recursive {
        let destination = recursive_destination(&args.source, &args.destination)?;
        return copy_directory(&args.source, &destination, use_password, json);
    }
    let source = parse_remote_file(&args.source)?;
    let destination = parse_remote_file(&args.destination)?;
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

fn copy_directory(
    source_value: &str,
    destination_value: &str,
    use_password: bool,
    json: bool,
) -> io::Result<u8> {
    let source = parse_remote_file(source_value)?;
    let destination = parse_remote_file(destination_value)?;
    if source.is_none() && destination.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "at least one cp path must use HOST:PATH",
        ));
    }
    if source.is_none() && !Path::new(source_value).is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{source_value} is not a directory"),
        ));
    }
    let password = use_password
        .then(|| rpassword::prompt_password("SSH password: "))
        .transpose()?;
    let archive = copy_temp_path();
    let runtime = tokio::runtime::Runtime::new().map_err(io::Error::other)?;
    let result = runtime.block_on(async {
        match source {
            Some(ref remote) => {
                download_remote_directory(remote, &archive, password.as_deref(), !json).await?
            }
            None => archive_local_directory(Path::new(source_value), &archive)?,
        }
        match destination {
            Some(ref remote) => {
                upload_remote_directory(remote, &archive, password.as_deref(), !json).await
            }
            None => extract_local_directory(&archive, Path::new(destination_value)),
        }
    });
    let bytes = fs::metadata(&archive).map(|m| m.len()).unwrap_or(0);
    let _ = fs::remove_file(&archive);
    result?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "source": source_value, "destination": destination_value,
                "archive_bytes": bytes, "recursive": true, "ok": true,
            }))
            .map_err(io::Error::other)?
        );
    } else {
        println!("Copied directory: {source_value} -> {destination_value}");
    }
    Ok(0)
}

fn archive_local_directory(source: &Path, archive: &Path) -> io::Result<()> {
    let file = fs::File::create(archive)?;
    let mut builder = tar::Builder::new(file);
    builder.follow_symlinks(false);
    builder.append_dir_all(".", source)?;
    builder.finish()
}

fn extract_local_directory(archive: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir_all(destination)?;
    tar::Archive::new(fs::File::open(archive)?).unpack(destination)
}

async fn download_remote_directory(
    source: &RemoteFile<'_>,
    archive: &Path,
    password: Option<&str>,
    show_progress: bool,
) -> io::Result<()> {
    let command = remote_command::download_directory(source.path)?;
    let progress = TransferProgress::new(
        format!("archive {}:{}", source.host, source.path),
        None,
        show_progress,
    );
    let (status, stderr) =
        if let Some(hop) = connect_exec_hop(source.host, password, show_progress).await? {
            hop.download_file(command, archive, progress).await?
        } else {
            connect_host(source.host, password)
                .await?
                .download_file(&command, archive, progress)
                .await?
        };
    if status != 0 {
        return Err(io::Error::other(format!(
            "remote directory read failed for {}:{}: {}",
            source.host,
            source.path,
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    Ok(())
}

async fn upload_remote_directory(
    destination: &RemoteFile<'_>,
    archive: &Path,
    password: Option<&str>,
    show_progress: bool,
) -> io::Result<()> {
    let command = remote_command::upload_directory(destination.path)?;
    let progress = TransferProgress::new(
        format!("extract {}:{}", destination.host, destination.path),
        Some(fs::metadata(archive)?.len()),
        show_progress,
    );
    let (status, stderr) =
        if let Some(hop) = connect_exec_hop(destination.host, password, show_progress).await? {
            let (status, _, stderr) = hop.upload_file(command, archive, progress).await?;
            (status, stderr)
        } else {
            connect_host(destination.host, password)
                .await?
                .upload_file(&command, archive, progress)
                .await?
        };
    if status != 0 {
        return Err(io::Error::other(format!(
            "remote directory write failed for {}:{}: {}",
            destination.host,
            destination.path,
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    Ok(())
}

fn recursive_destination(source: &str, destination: &str) -> io::Result<String> {
    if !destination.ends_with('/') && !destination.ends_with('\\') {
        return Ok(destination.to_owned());
    }
    let name = source_name(source.trim_end_matches(['/', '\\']))?;
    Ok(format!("{destination}{name}"))
}

pub fn remove(args: RmArgs, use_password: bool, json: bool) -> io::Result<u8> {
    let target = parse_remote_file(&args.target)?.ok_or_else(|| {
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
    let (status, stderr) = runtime.block_on(async {
        if let Some(hop) = connect_exec_hop(target.host, password.as_deref(), !json).await? {
            let (status, _, stderr) = hop.execute_capture_with_input(command, Vec::new()).await?;
            Ok::<_, io::Error>((status, stderr))
        } else {
            let (status, _, stderr) = connect_host(target.host, password.as_deref())
                .await?
                .execute_capture_with_input(&command, Vec::new())
                .await?;
            Ok((status, stderr))
        }
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
    let path = parse_remote_file(value)?.map_or(value, |remote| remote.path);
    PathBuf::from(path)
        .file_name()
        .and_then(OsStr::to_str)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "source has no file name"))
}

async fn download_remote_file(
    source: &RemoteFile<'_>,
    password: Option<&str>,
    show_progress: bool,
) -> io::Result<PathBuf> {
    if let Some(hop) = connect_exec_hop(source.host, password, show_progress).await? {
        let size_command = remote_command::file_size(source.path)?;
        let (size_status, size_stdout, size_stderr) = hop
            .execute_capture_with_input(size_command, Vec::new())
            .await?;
        if size_status != 0 {
            return Err(io::Error::other(format!(
                "remote read failed for {}:{}: {}",
                source.host,
                source.path,
                String::from_utf8_lossy(&size_stderr).trim()
            )));
        }
        let total = String::from_utf8_lossy(&size_stdout)
            .trim()
            .parse::<u64>()
            .map_err(|_| io::Error::other("invalid exec-hop file size response"))?;
        let progress = TransferProgress::new(
            format!("download {}:{}", source.host, source.path),
            Some(total),
            show_progress,
        );
        let temp = copy_temp_path();
        let (status, stderr) = hop
            .download_file(remote_command::download_file(source.path)?, &temp, progress)
            .await?;
        if status != 0 {
            let _ = fs::remove_file(&temp);
            return Err(io::Error::other(format!(
                "remote read failed for {}:{}: {}",
                source.host,
                source.path,
                String::from_utf8_lossy(&stderr).trim()
            )));
        }
        let received = fs::metadata(&temp)?.len();
        if received != total {
            let _ = fs::remove_file(&temp);
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "exec-hop download size changed: expected {total} bytes, received {}",
                    received
                ),
            ));
        }
        return Ok(temp);
    }
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
    // Bastion hosts only support one exec channel per connection, so open a
    // fresh connection for the actual file transfer.
    let transfer_ssh = if ssh.is_bastion() {
        ssh.reconnect().await?
    } else {
        ssh
    };
    let command = remote_command::download_file(source.path)?;
    let temp = copy_temp_path();
    let progress = TransferProgress::new(
        format!("download {}:{}", source.host, source.path),
        total,
        show_progress,
    );
    let (status, stderr) = match transfer_ssh.download_file(&command, &temp, progress).await {
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
    if let Some(hop) = connect_exec_hop(destination.host, password, show_progress).await? {
        let total = fs::metadata(source)?.len();
        let progress = TransferProgress::new(
            format!("upload {}:{}", destination.host, destination.path),
            Some(total),
            show_progress,
        );
        let command = remote_command::upload_file(destination.path, source_name)?;
        let (status, _, stderr) = hop.upload_file(command, source, progress).await?;
        if status != 0 {
            return Err(io::Error::other(format!(
                "remote write failed for {}:{}: {}",
                destination.host,
                destination.path,
                String::from_utf8_lossy(&stderr).trim()
            )));
        }
        return Ok(());
    }
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
    use super::{parse_remote_file, recursive_destination, validate_remove_path};

    #[test]
    fn distinguishes_remote_paths_from_windows_drives() {
        let remote = parse_remote_file("server-a:/var/log/app.log")
            .unwrap()
            .unwrap();
        assert_eq!(remote.host, "server-a");
        assert_eq!(remote.path, "/var/log/app.log");
        assert!(parse_remote_file(r"C:\temp\app.log").unwrap().is_none());
        assert!(parse_remote_file("server-a:").is_err());
    }

    #[test]
    fn refuses_dangerous_remote_remove_paths() {
        for path in ["/", "////", ".", "..", "~", "$HOME/", "/tmp/../"] {
            assert!(validate_remove_path(path).is_err(), "accepted {path:?}");
        }
        assert!(validate_remove_path("/tmp/binport-test").is_ok());
    }

    #[test]
    fn recursive_copy_resolves_destination_root() {
        assert_eq!(
            recursive_destination("./assets", "server:/tmp/").unwrap(),
            "server:/tmp/assets"
        );
        assert_eq!(
            recursive_destination("server:/tmp/assets/", "./backup/").unwrap(),
            "./backup/assets"
        );
        assert_eq!(
            recursive_destination("./assets", "server:/tmp/new-name").unwrap(),
            "server:/tmp/new-name"
        );
    }
}
