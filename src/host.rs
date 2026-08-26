use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const MANAGED_CONFIG_NAME: &str = "binport_config";
const INCLUDE_LINE: &str = "Include ~/.ssh/binport_config";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostEntry {
    pub name: String,
    pub hostname: String,
    pub user: String,
    pub port: u16,
    pub proxy_jump: Option<String>,
}

pub fn managed_config_path() -> io::Result<PathBuf> {
    Ok(ssh_dir()?.join(MANAGED_CONFIG_NAME))
}

pub fn main_config_path() -> io::Result<PathBuf> {
    Ok(ssh_dir()?.join("config"))
}

pub fn list() -> io::Result<Vec<HostEntry>> {
    let path = managed_config_path()?;
    if !path.is_file() {
        return Ok(Vec::new());
    }
    parse(&fs::read_to_string(path)?)
}

pub fn find(name: &str) -> io::Result<Option<HostEntry>> {
    Ok(list()?.into_iter().find(|entry| entry.name == name))
}

pub fn add(entry: HostEntry, force: bool) -> io::Result<()> {
    validate_entry(&entry)?;
    let directory = ssh_dir()?;
    fs::create_dir_all(&directory)?;
    set_directory_permissions(&directory)?;
    let main = directory.join("config");
    let managed = directory.join(MANAGED_CONFIG_NAME);
    let main_source = match fs::read_to_string(&main) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    if contains_alias(&main_source, &entry.name) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "host {:?} already exists in {}; edit it there or choose another name",
                entry.name,
                main.display()
            ),
        ));
    }
    let mut entries = if managed.is_file() {
        parse(&fs::read_to_string(&managed)?)?
            .into_iter()
            .map(|entry| (entry.name.clone(), entry))
            .collect::<BTreeMap<_, _>>()
    } else {
        BTreeMap::new()
    };
    if entries.contains_key(&entry.name) && !force {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "host {:?} already exists; retry with --force to update it",
                entry.name
            ),
        ));
    }
    entries.insert(entry.name.clone(), entry);
    write_atomic(&managed, &render(entries.values()))?;
    ensure_include(&main, &main_source)?;
    Ok(())
}

pub fn remove(name: &str) -> io::Result<bool> {
    validate_token("host name", name)?;
    let path = managed_config_path()?;
    if !path.is_file() {
        return Ok(false);
    }
    let mut entries = parse(&fs::read_to_string(&path)?)?
        .into_iter()
        .map(|entry| (entry.name.clone(), entry))
        .collect::<BTreeMap<_, _>>();
    let removed = entries.remove(name).is_some();
    if removed {
        write_atomic(&path, &render(entries.values()))?;
    }
    Ok(removed)
}

pub fn contains_alias(source: &str, alias: &str) -> bool {
    source.lines().any(|raw| {
        let line = raw.split('#').next().unwrap_or_default().trim();
        let Some((key, value)) = line.split_once(char::is_whitespace) else {
            return false;
        };
        key.eq_ignore_ascii_case("host")
            && value.split_whitespace().any(|candidate| candidate == alias)
    })
}

fn parse(source: &str) -> io::Result<Vec<HostEntry>> {
    let mut entries = Vec::new();
    let mut current: Option<HostEntry> = None;
    for raw in source.lines() {
        let line = raw.split('#').next().unwrap_or_default().trim();
        let Some((key, value)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let value = value.trim();
        if key.eq_ignore_ascii_case("host") {
            if let Some(entry) = current.take() {
                validate_entry(&entry)?;
                entries.push(entry);
            }
            current = Some(HostEntry {
                name: value.to_owned(),
                hostname: String::new(),
                user: String::new(),
                port: 22,
                proxy_jump: None,
            });
        } else if let Some(entry) = current.as_mut() {
            if key.eq_ignore_ascii_case("hostname") {
                entry.hostname = value.to_owned();
            } else if key.eq_ignore_ascii_case("user") {
                entry.user = value.to_owned();
            } else if key.eq_ignore_ascii_case("port") {
                entry.port = value.parse().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid Port in binport_config")
                })?;
            } else if key.eq_ignore_ascii_case("proxyjump") {
                entry.proxy_jump = Some(value.to_owned());
            }
        }
    }
    if let Some(entry) = current {
        validate_entry(&entry)?;
        entries.push(entry);
    }
    Ok(entries)
}

fn render<'a>(entries: impl Iterator<Item = &'a HostEntry>) -> String {
    let mut output = String::from("# Managed by binport. Use `binport host` to edit.\n\n");
    for entry in entries {
        output.push_str(&format!(
            "Host {}\n    HostName {}\n    User {}\n    Port {}\n",
            entry.name, entry.hostname, entry.user, entry.port
        ));
        if let Some(jump) = &entry.proxy_jump {
            output.push_str(&format!("    ProxyJump {jump}\n"));
        }
        output.push('\n');
    }
    output
}

fn validate_entry(entry: &HostEntry) -> io::Result<()> {
    validate_token("host name", &entry.name)?;
    validate_token("hostname", &entry.hostname)?;
    validate_token("user", &entry.user)?;
    if let Some(jump) = &entry.proxy_jump {
        validate_token("jump host", jump)?;
        if jump == &entry.name {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a host cannot use itself as ProxyJump",
            ));
        }
    }
    Ok(())
}

fn validate_token(label: &str, value: &str) -> io::Result<()> {
    if value.is_empty()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control() || byte == b'#')
        || value.contains(['*', '?', '!'])
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid {label} {value:?}"),
        ));
    }
    Ok(())
}

fn ensure_include(path: &Path, source: &str) -> io::Result<()> {
    let included = source.lines().any(|raw| {
        let line = raw.split('#').next().unwrap_or_default().trim();
        line.split_once(char::is_whitespace)
            .is_some_and(|(key, value)| {
                key.eq_ignore_ascii_case("include")
                    && value.trim().eq_ignore_ascii_case("~/.ssh/binport_config")
            })
    });
    if included {
        return Ok(());
    }
    let output = if source.is_empty() {
        format!("{INCLUDE_LINE}\n")
    } else {
        format!("{INCLUDE_LINE}\n\n{source}")
    };
    write_atomic(path, &output)
}

fn write_atomic(path: &Path, source: &str) -> io::Result<()> {
    let temp = path.with_extension(format!("binport-{}", std::process::id()));
    fs::write(&temp, source)?;
    set_file_permissions(&temp)?;
    replace_file(&temp, path)
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    if destination.is_file() {
        fs::remove_file(destination)?;
    }
    fs::rename(source, destination)
}

fn ssh_dir() -> io::Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .map(|home| home.join(".ssh"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "home directory is unavailable"))
}

#[cfg(unix)]
fn set_directory_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_directory_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_file_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_file_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_managed_hosts() {
        let source =
            "# managed\nHost app\n HostName 10.0.0.2\n User deploy\n Port 2202\n ProxyJump jump\n";
        let entries = parse(source).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].proxy_jump.as_deref(), Some("jump"));
        assert!(render(entries.iter()).contains("    ProxyJump jump"));
    }

    #[test]
    fn detects_exact_aliases_and_rejects_patterns() {
        assert!(contains_alias("Host app db\n", "app"));
        assert!(!contains_alias("Host app-*\n", "app-01"));
        let mut entry = HostEntry {
            name: "bad*".into(),
            hostname: "10.0.0.2".into(),
            user: "root".into(),
            port: 22,
            proxy_jump: None,
        };
        assert!(validate_entry(&entry).is_err());
        entry.name = "app".into();
        entry.proxy_jump = Some("app".into());
        assert!(validate_entry(&entry).is_err());
    }

    #[test]
    fn installs_include_once_without_rewriting_existing_content() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config");
        fs::write(&config, "Host existing\n    User deploy\n").unwrap();
        let source = fs::read_to_string(&config).unwrap();
        ensure_include(&config, &source).unwrap();
        let first = fs::read_to_string(&config).unwrap();
        ensure_include(&config, &first).unwrap();
        let second = fs::read_to_string(&config).unwrap();
        assert_eq!(first, second);
        assert_eq!(second.matches(INCLUDE_LINE).count(), 1);
        assert!(second.contains("Host existing"));
    }
}
