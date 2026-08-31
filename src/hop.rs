use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::catalog::Platform;
use crate::progress::TransferProgress;
use crate::ssh::{Destination, NativeSsh};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_HEADER_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecRequest {
    pub version: u16,
    pub target: String,
    pub target_port: u16,
    pub command: String,
    pub stdin_bytes: u64,
    #[serde(default)]
    pub tty: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayRequest {
    pub version: u16,
    pub target: String,
    pub target_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
}

impl RelayRequest {
    pub fn new(
        target: String,
        target_port: u16,
        remote_host: String,
        remote_port: u16,
    ) -> io::Result<Self> {
        for (label, value) in [("target", &target), ("remote host", &remote_host)] {
            if value.is_empty() || value.bytes().any(|byte| byte.is_ascii_control()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("hop relay {label} is invalid"),
                ));
            }
        }
        if target_port == 0 || remote_port == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "hop relay ports must be greater than zero",
            ));
        }
        Ok(Self {
            version: PROTOCOL_VERSION,
            target,
            target_port,
            remote_host,
            remote_port,
        })
    }
}

impl ExecRequest {
    pub fn new(
        target: String,
        target_port: u16,
        command: String,
        stdin_bytes: u64,
    ) -> io::Result<Self> {
        if target.is_empty() || target.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "hop target must be non-empty and contain no control characters",
            ));
        }
        if target_port == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "hop target port must be greater than zero",
            ));
        }
        if command.is_empty() || command.len() > MAX_HEADER_BYTES / 2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "hop command must be non-empty and no larger than 128 KiB",
            ));
        }
        Ok(Self {
            version: PROTOCOL_VERSION,
            target,
            target_port,
            command,
            stdin_bytes,
            tty: false,
        })
    }

    pub fn new_tty(target: String, target_port: u16, command: String) -> io::Result<Self> {
        let mut request = Self::new(target, target_port, command, 0)?;
        request.tty = true;
        Ok(request)
    }
}

pub fn write_request(
    mut writer: impl Write,
    request: &ExecRequest,
    input: &[u8],
) -> io::Result<()> {
    if request.version != PROTOCOL_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unsupported hop protocol version {}", request.version),
        ));
    }
    if request.stdin_bytes != input.len() as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "hop request stdin length does not match its payload",
        ));
    }
    let header = serde_json::to_vec(request).map_err(io::Error::other)?;
    if header.len() > MAX_HEADER_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "hop request header is too large",
        ));
    }
    let length = u32::try_from(header.len()).map_err(io::Error::other)?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(&header)?;
    writer.write_all(input)?;
    writer.flush()
}

pub fn read_request(mut reader: impl Read) -> io::Result<(ExecRequest, Vec<u8>)> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length)?;
    let header_length = u32::from_be_bytes(length) as usize;
    if header_length == 0 || header_length > MAX_HEADER_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid hop request header length {header_length}"),
        ));
    }
    let mut header = vec![0_u8; header_length];
    reader.read_exact(&mut header)?;
    let request: ExecRequest = serde_json::from_slice(&header).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid hop request: {error}"),
        )
    })?;
    if request.version != PROTOCOL_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported hop protocol version {}", request.version),
        ));
    }
    let input_length = usize::try_from(request.stdin_bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "hop stdin payload does not fit in memory on this platform",
        )
    })?;
    let mut input = vec![0_u8; input_length];
    reader.read_exact(&mut input)?;
    Ok((request, input))
}

pub async fn read_request_header_async(
    reader: &mut (impl AsyncRead + Unpin),
) -> io::Result<ExecRequest> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length).await?;
    let header_length = u32::from_be_bytes(length) as usize;
    if header_length == 0 || header_length > MAX_HEADER_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid hop request header length {header_length}"),
        ));
    }
    let mut header = vec![0_u8; header_length];
    reader.read_exact(&mut header).await?;
    parse_header(&header)
}

pub async fn read_relay_header_async(
    reader: &mut (impl AsyncRead + Unpin),
) -> io::Result<RelayRequest> {
    let header = read_raw_header_async(reader).await?;
    let request: RelayRequest = serde_json::from_slice(&header).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid hop relay request: {error}"),
        )
    })?;
    if request.version != PROTOCOL_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported hop protocol version {}", request.version),
        ));
    }
    Ok(request)
}

async fn read_raw_header_async(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<Vec<u8>> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length).await?;
    let header_length = u32::from_be_bytes(length) as usize;
    if header_length == 0 || header_length > MAX_HEADER_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid hop request header length {header_length}"),
        ));
    }
    let mut header = vec![0_u8; header_length];
    reader.read_exact(&mut header).await?;
    Ok(header)
}

fn parse_header(header: &[u8]) -> io::Result<ExecRequest> {
    let request: ExecRequest = serde_json::from_slice(header).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid hop request: {error}"),
        )
    })?;
    if request.version != PROTOCOL_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported hop protocol version {}", request.version),
        ));
    }
    Ok(request)
}

pub fn encode_request_header(request: &ExecRequest) -> io::Result<Vec<u8>> {
    if request.version != PROTOCOL_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unsupported hop protocol version {}", request.version),
        ));
    }
    let header = serde_json::to_vec(request).map_err(io::Error::other)?;
    if header.len() > MAX_HEADER_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "hop request header is too large",
        ));
    }
    let length = u32::try_from(header.len()).map_err(io::Error::other)?;
    let mut encoded = Vec::with_capacity(4 + header.len());
    encoded.extend_from_slice(&length.to_be_bytes());
    encoded.extend_from_slice(&header);
    Ok(encoded)
}

pub fn encode_relay_header(request: &RelayRequest) -> io::Result<Vec<u8>> {
    if request.version != PROTOCOL_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unsupported hop protocol version {}", request.version),
        ));
    }
    encode_json_header(request)
}

fn encode_json_header(value: &impl Serialize) -> io::Result<Vec<u8>> {
    let header = serde_json::to_vec(value).map_err(io::Error::other)?;
    if header.len() > MAX_HEADER_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "hop request header is too large",
        ));
    }
    let length = u32::try_from(header.len()).map_err(io::Error::other)?;
    let mut encoded = Vec::with_capacity(4 + header.len());
    encoded.extend_from_slice(&length.to_be_bytes());
    encoded.extend_from_slice(&header);
    Ok(encoded)
}

pub fn encode_request(request: &ExecRequest, input: &[u8]) -> io::Result<Vec<u8>> {
    let mut encoded = Vec::new();
    write_request(&mut encoded, request, input)?;
    Ok(encoded)
}

pub fn helper_cache_path(platform: Platform) -> io::Result<PathBuf> {
    Ok(crate::toolbox::cache_root()?
        .join("helpers")
        .join(env!("CARGO_PKG_VERSION"))
        .join(platform.name())
        .join(helper_filename()))
}

pub fn locate_helper(platform: Platform) -> io::Result<PathBuf> {
    if let Some(path) = env::var_os("BINPORT_HOP_BINARY") {
        return validate_helper(Path::new(&path));
    }
    let cached = helper_cache_path(platform)?;
    if cached.is_file() {
        return validate_helper(&cached);
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "binport-hop for {} is not available; place it at {} or set BINPORT_HOP_BINARY to a trusted helper artifact",
            platform.name(),
            cached.display()
        ),
    ))
}

fn validate_helper(path: &Path) -> io::Result<PathBuf> {
    if !path.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("binport-hop helper {} is not a file", path.display()),
        ));
    }
    path.canonicalize()
}

fn helper_filename() -> &'static str {
    "binport-hop"
}

pub fn fetch_helper(platform: Platform) -> io::Result<PathBuf> {
    if let Ok(path) = locate_helper(platform) {
        return Ok(path);
    }
    let archive_name = match platform {
        Platform::LinuxAmd64 => "binport-linux-amd64.tar.gz",
        Platform::LinuxArm64 => "binport-linux-arm64.tar.gz",
    };
    let version = env!("CARGO_PKG_VERSION");
    let base = env::var("BINPORT_RELEASE_BASE").unwrap_or_else(|_| {
        format!("https://github.com/mengshi02/binport/releases/download/v{version}")
    });
    let checksums = download_limited(&format!("{base}/SHA256SUMS"), 2 * 1024 * 1024)?;
    let checksums = String::from_utf8(checksums)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "SHA256SUMS is not UTF-8"))?;
    let expected = checksum_for(&checksums, archive_name)?;
    let archive = download_limited(&format!("{base}/{archive_name}"), 128 * 1024 * 1024)?;
    let actual = format!("{:x}", Sha256::digest(&archive));
    if actual != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("checksum mismatch for {archive_name}: expected {expected}, got {actual}"),
        ));
    }

    let wanted = format!("{}/binport-hop", archive_name.trim_end_matches(".tar.gz"));
    let decoder = flate2::read::GzDecoder::new(Cursor::new(archive));
    let mut tar = tar::Archive::new(decoder);
    let mut helper = None;
    for entry in tar.entries()? {
        let mut entry = entry?;
        if entry.path()?.to_string_lossy() == wanted {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            helper = Some(bytes);
            break;
        }
    }
    let helper = helper.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{archive_name} does not contain {wanted}"),
        )
    })?;
    let destination = helper_cache_path(platform)?;
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid helper cache path"))?;
    fs::create_dir_all(parent)?;
    let temporary = destination.with_extension(format!("part-{}", std::process::id()));
    fs::write(&temporary, helper)?;
    set_executable(&temporary)?;
    fs::rename(&temporary, &destination)?;
    destination.canonicalize()
}

fn download_limited(url: &str, maximum: u64) -> io::Result<Vec<u8>> {
    let response = reqwest::blocking::get(url)
        .map_err(io::Error::other)?
        .error_for_status()
        .map_err(io::Error::other)?;
    if response.content_length().is_some_and(|size| size > maximum) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("download from {url} exceeds the {maximum}-byte limit"),
        ));
    }
    let mut bytes = Vec::new();
    response
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(io::Error::other)?;
    if bytes.len() as u64 > maximum {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("download from {url} exceeds the {maximum}-byte limit"),
        ));
    }
    Ok(bytes)
}

fn checksum_for(source: &str, filename: &str) -> io::Result<String> {
    source
        .lines()
        .filter_map(|line| line.split_once(char::is_whitespace))
        .find_map(|(checksum, remainder)| {
            (remainder.trim_start_matches([' ', '*']) == filename).then(|| checksum.to_owned())
        })
        .filter(|checksum| {
            checksum.len() == 64 && checksum.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("SHA256SUMS has no valid entry for {filename}"),
            )
        })
}

#[cfg(unix)]
fn set_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

#[cfg(windows)]
fn set_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[derive(Clone)]
pub struct ExecHop {
    entry: NativeSsh,
    target: String,
    target_port: u16,
    remote_helper: String,
}

impl ExecHop {
    pub async fn connect(
        entry_destination: &Destination,
        target: String,
        target_port: u16,
        entry_password: Option<&str>,
        show_progress: bool,
    ) -> io::Result<Self> {
        let entry = NativeSsh::connect(entry_destination, entry_password).await?;
        let (status, stdout, stderr) = entry.execute_capture("uname -s; uname -m").await?;
        if status != 0 {
            return Err(io::Error::other(format!(
                "cannot detect entry-host platform: {}",
                stderr.trim()
            )));
        }
        let mut lines = stdout.lines();
        let platform = Platform::from_uname(
            lines.next().unwrap_or_default(),
            lines.next().unwrap_or_default(),
        )
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                format!("unsupported entry-host platform: {}", stdout.trim()),
            )
        })?;
        let helper = tokio::task::spawn_blocking(move || fetch_helper(platform))
            .await
            .map_err(io::Error::other)??;
        let hash = crate::sha256_file(&helper)?;
        let (remote_directory, remote_helper) = crate::remote_paths(&hash, "binport-hop");
        let (cache_status, _, _) = entry
            .execute_capture(&crate::cache_check_command(&remote_helper))
            .await?;
        if cache_status != 0 {
            let size = fs::metadata(&helper)?.len();
            let progress = TransferProgress::new("binport-hop", Some(size), show_progress);
            let (status, stderr) = entry
                .upload_file(
                    &crate::upload_command(&remote_directory, &remote_helper),
                    &helper,
                    progress,
                )
                .await?;
            if status != 0 {
                return Err(io::Error::other(format!(
                    "failed to install binport-hop on the entry host: {}",
                    String::from_utf8_lossy(&stderr).trim()
                )));
            }
        }
        Ok(Self {
            entry,
            target,
            target_port,
            remote_helper,
        })
    }

    pub async fn execute_capture_with_input(
        &self,
        command: String,
        input: Vec<u8>,
    ) -> io::Result<(u32, Vec<u8>, Vec<u8>)> {
        let request = ExecRequest::new(
            self.target.clone(),
            self.target_port,
            command,
            input.len() as u64,
        )?;
        let payload = encode_request(&request, &input)?;
        let helper_command = crate::execute_command(&self.remote_helper, &[] as &[OsString])?;
        self.entry
            .execute_capture_with_input(&helper_command, payload)
            .await
    }

    pub async fn execute_tty(&self, command: String, eof_on_quit: bool) -> io::Result<u32> {
        let request = ExecRequest::new_tty(self.target.clone(), self.target_port, command)?;
        let header = encode_request_header(&request)?;
        let helper_command = crate::execute_command(&self.remote_helper, &[] as &[OsString])?;
        self.entry
            .execute_tty_with_prefix(&helper_command, header, eof_on_quit)
            .await
    }

    pub async fn upload_file(
        &self,
        command: String,
        path: &Path,
        progress: TransferProgress,
    ) -> io::Result<(u32, Vec<u8>, Vec<u8>)> {
        let size = fs::metadata(path)?.len();
        let request = ExecRequest::new(self.target.clone(), self.target_port, command, size)?;
        let header = encode_request_header(&request)?;
        let helper_command = crate::execute_command(&self.remote_helper, &[] as &[OsString])?;
        self.entry
            .execute_with_prefix_file(&helper_command, header, path, progress)
            .await
    }

    pub async fn download_file(
        &self,
        command: String,
        path: &Path,
        progress: TransferProgress,
    ) -> io::Result<(u32, Vec<u8>)> {
        let request = ExecRequest::new(self.target.clone(), self.target_port, command, 0)?;
        let header = encode_request_header(&request)?;
        let helper_command = crate::execute_command(&self.remote_helper, &[] as &[OsString])?;
        self.entry
            .download_file_with_input(&helper_command, header, path, progress)
            .await
    }

    pub async fn relay_tcp(
        &self,
        local: TcpStream,
        remote_host: String,
        remote_port: u16,
    ) -> io::Result<()> {
        let request = RelayRequest::new(
            self.target.clone(),
            self.target_port,
            remote_host,
            remote_port,
        )?;
        let header = encode_relay_header(&request)?;
        let helper_command =
            crate::execute_command(&self.remote_helper, &[OsString::from("--relay")])?;
        let (stdout_tx, mut stdout_rx) = mpsc::channel(8);
        let (stderr_tx, mut stderr_rx) = mpsc::channel(8);
        let (stdin_tx, stdin_rx) = mpsc::channel(8);
        let (mut local_read, mut local_write) = local.into_split();
        let feeder = tokio::spawn(async move {
            stdin_tx.send(header).await.map_err(io::Error::other)?;
            let mut buffer = vec![0_u8; 64 * 1024];
            loop {
                let read = local_read.read(&mut buffer).await?;
                if read == 0 {
                    break;
                }
                stdin_tx
                    .send(buffer[..read].to_vec())
                    .await
                    .map_err(io::Error::other)?;
            }
            let _ = stdin_tx.send(Vec::new()).await;
            Ok::<_, io::Error>(())
        });
        let execution = self.entry.client().execute_io(
            &helper_command,
            stdout_tx,
            Some(stderr_tx),
            Some(stdin_rx),
            false,
            None,
        );
        tokio::pin!(execution);
        let mut stderr = Vec::new();
        let status = loop {
            tokio::select! {
                result = &mut execution => break result.map_err(io::Error::other)?,
                Some(data) = stdout_rx.recv() => local_write.write_all(&data).await?,
                Some(data) = stderr_rx.recv() => stderr.extend_from_slice(&data),
            }
        };
        feeder.abort();
        let _ = feeder.await;
        while let Ok(data) = stdout_rx.try_recv() {
            local_write.write_all(&data).await?;
        }
        while let Ok(data) = stderr_rx.try_recv() {
            stderr.extend_from_slice(&data);
        }
        local_write.shutdown().await?;
        if status != 0 {
            return Err(io::Error::other(format!(
                "exec-hop relay failed with exit {status}: {}",
                String::from_utf8_lossy(&stderr).trim()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trip_preserves_binary_stdin() {
        let input = b"zero\0newline\nff\xff";
        let request = ExecRequest::new(
            "root@10.0.0.5".to_owned(),
            22,
            "printf '%s' safe".to_owned(),
            input.len() as u64,
        )
        .unwrap();
        let encoded = encode_request(&request, input).unwrap();
        let (decoded, payload) = read_request(encoded.as_slice()).unwrap();
        assert_eq!(decoded, request);
        assert_eq!(payload, input);
    }

    #[test]
    fn tty_request_round_trip_has_no_fixed_payload() {
        let request = ExecRequest::new_tty("root@10.0.0.5".into(), 22, "btm".into()).unwrap();
        let encoded = encode_request(&request, &[]).unwrap();
        let (decoded, payload) = read_request(encoded.as_slice()).unwrap();
        assert_eq!(decoded, request);
        assert!(decoded.tty);
        assert!(payload.is_empty());
    }

    #[test]
    fn rejects_mismatched_and_oversized_frames() {
        let request = ExecRequest::new("host".into(), 22, "true".into(), 2).unwrap();
        assert!(encode_request(&request, b"x").is_err());

        let encoded = ((MAX_HEADER_BYTES as u32) + 1).to_be_bytes();
        assert!(read_request(encoded.as_slice()).is_err());
    }

    #[test]
    fn selects_only_an_exact_release_checksum() {
        let source = "aaaa  other.tar.gz\n0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  binport-linux-amd64.tar.gz\n";
        assert_eq!(
            checksum_for(source, "binport-linux-amd64.tar.gz").unwrap(),
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
        assert!(checksum_for(source, "binport-linux-arm64.tar.gz").is_err());
    }

    #[tokio::test]
    async fn relay_header_round_trip_is_bounded_and_typed() {
        let request =
            RelayRequest::new("root@target".into(), 22, "127.0.0.1".into(), 8080).unwrap();
        let encoded = encode_relay_header(&request).unwrap();
        let mut input = encoded.as_slice();
        let decoded = read_relay_header_async(&mut input).await.unwrap();
        assert_eq!(decoded, request);
    }
}
