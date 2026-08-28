pub mod auth;
pub mod bastion;
pub mod binfile;
pub mod catalog;
pub mod hop;
pub mod host;
pub mod lockfile;
pub mod oci;
pub mod probe;
pub mod progress;
pub mod registry;
pub mod remote_command;
pub mod ssh;
pub mod toolbox;

use sha2::{Digest, Sha256};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

pub const CACHE_ROOT: &str = ".cache/binport";

pub fn find_executable(tool: &OsStr, explicit_path: Option<&Path>) -> io::Result<PathBuf> {
    if let Some(path) = explicit_path {
        return validate_executable(path);
    }

    let tool_path = Path::new(tool);
    if tool_path.components().count() > 1 {
        return validate_executable(tool_path);
    }

    let path = env::var_os("PATH").unwrap_or_default();
    for directory in env::split_paths(&path) {
        let candidate = directory.join(tool);
        if candidate.is_file() {
            return candidate.canonicalize();
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("tool {:?} was not found in PATH", tool),
    ))
}

fn validate_executable(path: &Path) -> io::Result<PathBuf> {
    if !path.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("tool {} is not a file", path.display()),
        ));
    }
    path.canonicalize()
}

pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn safe_tool_name(tool_path: &Path) -> io::Result<String> {
    let name = tool_path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "tool name is not valid UTF-8")
        })?;

    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("tool name {name:?} contains unsupported characters"),
        ));
    }
    Ok(name.to_owned())
}

pub fn remote_paths(hash: &str, tool_name: &str) -> (String, String) {
    let directory = format!("$HOME/{CACHE_ROOT}/{hash}");
    let file = format!("{directory}/{tool_name}");
    (directory, file)
}

pub fn shell_quote(value: &OsStr) -> io::Result<String> {
    let value = value.to_str().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "argument is not valid UTF-8")
    })?;
    Ok(format!("'{}'", value.replace('\'', "'\\''")))
}

pub fn cache_check_command(remote_file: &str) -> String {
    format!("test -x {}", quote_remote_path(remote_file))
}

pub fn upload_command(remote_directory: &str, remote_file: &str) -> String {
    format!(
        "sh -c 'umask 077; mkdir -p \"$1\"; tmp=\"$2.tmp.$$\"; trap '\"'\"'rm -f \"$tmp\"'\"'\"' EXIT HUP INT TERM; cat >\"$tmp\" && chmod 755 \"$tmp\" && mv \"$tmp\" \"$2\"; status=$?; trap - EXIT; exit $status' sh {} {}",
        quote_remote_path(remote_directory),
        quote_remote_path(remote_file)
    )
}

pub fn execute_command(remote_file: &str, arguments: &[OsString]) -> io::Result<String> {
    let mut command = format!(
        "sh -c 'tool=$1; shift; exec \"$tool\" \"$@\"' sh {}",
        quote_remote_path(remote_file)
    );
    for argument in arguments {
        command.push(' ');
        command.push_str(&shell_quote(argument)?);
    }
    Ok(command)
}

pub fn probe_execute_command(
    amd64_file: Option<&str>,
    arm64_file: Option<&str>,
    arguments: &[OsString],
) -> io::Result<String> {
    let amd64 = amd64_file.unwrap_or("");
    let arm64 = arm64_file.unwrap_or("");
    let mut command = format!(
        "sh -c 'os=$(uname -s) || exit 126; arch=$(uname -m) || exit 126; \
case \"$os/$arch\" in Linux/x86_64|Linux/amd64) platform=linux/amd64; tool=$1;; \
Linux/aarch64|Linux/arm64) platform=linux/arm64; tool=$2;; \
*) printf \"__BINPORT__ unsupported %s/%s\\n\" \"$os\" \"$arch\" >&2; exit 126;; esac; \
shift 2; if [ -z \"$tool\" ]; then printf \"__BINPORT__ missing %s\\n\" \"$platform\" >&2; exit 126; fi; \
if [ ! -x \"$tool\" ]; then printf \"__BINPORT__ miss %s\\n\" \"$platform\" >&2; exit 125; fi; \
printf \"__BINPORT__ hit %s\\n\" \"$platform\" >&2; exec \"$tool\" \"$@\"' sh {} {}",
        quote_remote_path(amd64),
        quote_remote_path(arm64)
    );
    for argument in arguments {
        command.push(' ');
        command.push_str(&shell_quote(argument)?);
    }
    Ok(command)
}

fn quote_remote_path(path: &str) -> String {
    // $HOME must expand remotely; the generated suffix only contains safe characters.
    if let Some(suffix) = path.strip_prefix("$HOME/") {
        format!("\"$HOME/{suffix}\"")
    } else {
        format!("'{}'", path.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_shell_arguments_without_interpolation() {
        assert_eq!(
            shell_quote(OsStr::new("hello world")).unwrap(),
            "'hello world'"
        );
        assert_eq!(
            shell_quote(OsStr::new("it's $HOME")).unwrap(),
            "'it'\\''s $HOME'"
        );
    }

    #[test]
    fn builds_injection_safe_execution_command() {
        let command = execute_command(
            "$HOME/.cache/binport/abc/rg",
            &[
                OsString::from("hello world"),
                OsString::from("$(touch /tmp/nope)"),
            ],
        )
        .unwrap();
        assert_eq!(
            command,
            "sh -c 'tool=$1; shift; exec \"$tool\" \"$@\"' sh \"$HOME/.cache/binport/abc/rg\" 'hello world' '$(touch /tmp/nope)'"
        );
    }

    #[test]
    fn builds_single_round_trip_probe_command() {
        let command = probe_execute_command(
            Some("$HOME/.cache/binport/amd/rg"),
            Some("$HOME/.cache/binport/arm/rg"),
            &[OsString::from("$(touch /tmp/nope)")],
        )
        .unwrap();
        assert!(command.contains("__BINPORT__ hit"));
        assert!(command.contains("\"$HOME/.cache/binport/amd/rg\""));
        assert!(command.ends_with("'$(touch /tmp/nope)'"));
    }

    #[test]
    fn remote_cache_is_content_addressed() {
        assert_eq!(
            remote_paths("deadbeef", "rg"),
            (
                "$HOME/.cache/binport/deadbeef".into(),
                "$HOME/.cache/binport/deadbeef/rg".into()
            )
        );
    }

    #[test]
    fn rejects_unsafe_tool_names() {
        assert!(safe_tool_name(Path::new("/tmp/not safe")).is_err());
        assert_eq!(safe_tool_name(Path::new("/tmp/rg")).unwrap(), "rg");
    }
}
