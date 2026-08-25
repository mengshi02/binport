use sha2::{Digest, Sha256};
use ssh_key::{Algorithm, LineEnding, PrivateKey, rand_core::UnwrapErr};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub struct ManagedKey {
    pub private_path: PathBuf,
    pub public_key: String,
    pub created: bool,
}

pub fn config_root() -> io::Result<PathBuf> {
    if let Some(path) = env::var_os("BINPORT_CONFIG_DIR") {
        return Ok(PathBuf::from(path));
    }
    #[cfg(windows)]
    let root = dirs::config_dir();
    #[cfg(not(windows))]
    let root = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")));
    root.map(|path| path.join("binport")).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "user configuration directory is unavailable",
        )
    })
}

pub fn managed_key_path(host: &str) -> io::Result<PathBuf> {
    let readable = host
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .take(48)
        .collect::<String>();
    let readable = readable.trim_matches('.');
    let readable = if readable.is_empty() {
        "host"
    } else {
        readable
    };
    let digest = format!("{:x}", Sha256::digest(host.as_bytes()));
    Ok(config_root()?
        .join("keys")
        .join(format!("{readable}-{}", &digest[..12])))
}

pub fn ensure_managed_key(host: &str) -> io::Result<ManagedKey> {
    let private_path = managed_key_path(host)?;
    ensure_managed_key_at(host, private_path)
}

fn ensure_managed_key_at(host: &str, private_path: PathBuf) -> io::Result<ManagedKey> {
    let public_path = public_path(&private_path);
    if private_path.is_file() && public_path.is_file() {
        return Ok(ManagedKey {
            public_key: fs::read_to_string(public_path)?.trim().to_owned(),
            private_path,
            created: false,
        });
    }
    if private_path.exists() || public_path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "managed key pair is incomplete at {}; remove both files and retry",
                private_path.display()
            ),
        ));
    }
    let parent = private_path
        .parent()
        .ok_or_else(|| io::Error::other("managed key path has no parent"))?;
    fs::create_dir_all(parent)?;
    set_private_permissions(parent, true)?;

    let mut rng = UnwrapErr(getrandom::SysRng);
    let mut key = PrivateKey::random(&mut rng, Algorithm::Ed25519).map_err(io::Error::other)?;
    key.set_comment(format!("binport:{host}"));
    let private = key.to_openssh(LineEnding::LF).map_err(io::Error::other)?;
    let public = key.public_key().to_openssh().map_err(io::Error::other)?;

    write_new_private_file(&private_path, private.as_bytes())?;
    if let Err(error) = fs::write(&public_path, format!("{public}\n")) {
        let _ = fs::remove_file(&private_path);
        return Err(error);
    }
    Ok(ManagedKey {
        private_path,
        public_key: public,
        created: true,
    })
}

pub fn read_managed_public_key(host: &str) -> io::Result<(PathBuf, String)> {
    let private_path = managed_key_path(host)?;
    if !private_path.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no binport key is configured for {host}"),
        ));
    }
    let public = fs::read_to_string(public_path(&private_path))?;
    Ok((private_path, public.trim().to_owned()))
}

pub fn remove_managed_key(host: &str) -> io::Result<()> {
    let private = managed_key_path(host)?;
    let public = public_path(&private);
    if private.exists() {
        fs::remove_file(private)?;
    }
    if public.exists() {
        fs::remove_file(public)?;
    }
    Ok(())
}

pub fn install_key_command() -> &'static str {
    "sh -c 'umask 077; dir=\"$HOME/.ssh\"; file=\"$dir/authorized_keys\"; mkdir -p \"$dir\"; touch \"$file\"; key=$(cat); if grep -qxF \"$key\" \"$file\"; then printf existing; else printf \"%s\\n\" \"$key\" >>\"$file\"; printf installed; fi'"
}

pub fn remove_key_command() -> &'static str {
    "sh -c 'umask 077; file=\"$HOME/.ssh/authorized_keys\"; key=$(cat); [ -f \"$file\" ] || exit 0; tmp=\"$file.binport.$$\"; trap '\"'\"'rm -f \"$tmp\"'\"'\"' EXIT HUP INT TERM; grep -vxF \"$key\" \"$file\" >\"$tmp\" || :; chmod 600 \"$tmp\"; mv \"$tmp\" \"$file\"; trap - EXIT'"
}

fn public_path(private_path: &Path) -> PathBuf {
    let mut path = private_path.as_os_str().to_owned();
    path.push(".pub");
    PathBuf::from(path)
}

fn write_new_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    set_private_permissions(path, false)
}

#[cfg(unix)]
fn set_private_permissions(path: &Path, directory: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if directory { 0o700 } else { 0o600 }),
    )
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path, _directory: bool) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_names_are_safe_and_host_specific() {
        let first = managed_key_path("root@example:22").unwrap();
        let second = managed_key_path("root@example:23").unwrap();
        assert_ne!(first, second);
        let name = first.file_name().unwrap().to_string_lossy();
        assert!(!name.contains(':'));
        assert!(!name.contains('/'));
    }

    #[test]
    fn remote_key_commands_do_not_interpolate_key_material() {
        assert!(install_key_command().contains("key=$(cat)"));
        assert!(remove_key_command().contains("key=$(cat)"));
    }

    #[test]
    fn generates_a_reusable_ed25519_openssh_key_pair() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("managed");
        let created = ensure_managed_key_at("demo", path.clone()).unwrap();
        assert!(created.created);
        assert_eq!(created.private_path, path);
        assert!(created.public_key.starts_with("ssh-ed25519 "));
        assert!(created.public_key.ends_with(" binport:demo"));
        let encoded = fs::read_to_string(&created.private_path).unwrap();
        let parsed = PrivateKey::from_openssh(&encoded).unwrap();
        assert_eq!(
            parsed.public_key().to_openssh().unwrap(),
            created.public_key
        );

        let reused = ensure_managed_key_at("demo", created.private_path).unwrap();
        assert!(!reused.created);
        assert_eq!(reused.public_key, created.public_key);
    }
}
