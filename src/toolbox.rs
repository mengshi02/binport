use crate::catalog::{self, Archive, Artifact, Platform};
use crate::lockfile::{self, ProjectLock, ResolvedTool};
use crate::progress::TransferProgress;
use crate::{safe_tool_name, sha256_file};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::env;
use std::fs::{self, File};
use std::io::{self, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use tar::{Archive as TarArchive, Builder as TarBuilder};

#[derive(Debug, Deserialize, Serialize)]
pub struct Lockfile {
    pub format: u32,
    pub tools: Vec<LockedTool>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct LockedTool {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub sha256: String,
    pub path: String,
}

pub fn build(root: &Path, binfile_path: &Path) -> io::Result<Lockfile> {
    let project_lock = if root.join(lockfile::LOCKFILE_NAME).is_file() {
        let lock = ProjectLock::read(root)?;
        lock.verify_binfile(binfile_path)?;
        lock
    } else {
        lockfile::resolve(root, binfile_path)?
    };
    let output = root.join(".binport/toolbox");
    fs::create_dir_all(&output)?;
    let mut locked = Vec::new();
    for tool in &project_lock.tools {
        let platform = Platform::parse(&tool.platform).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported locked platform {}", tool.platform),
            )
        })?;
        let cached = fetch_resolved(tool)?;
        let destination = output
            .join(platform.name().replace('/', "-"))
            .join(&tool.name);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&cached, &destination)?;
        set_executable(&destination)?;
        locked.push(LockedTool {
            name: tool.name.clone(),
            version: tool.version.clone(),
            platform: tool.platform.clone(),
            sha256: sha256_file(&destination)?,
            path: destination
                .strip_prefix(root)
                .unwrap_or(&destination)
                .display()
                .to_string(),
        });
    }
    for copy in project_lock.copies {
        safe_tool_name(Path::new(&copy.name))?;
        let source = lockfile::source_path(root, &copy.source).canonicalize()?;
        if sha256_file(&source)? != copy.sha256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "local source for {} changed; run `binport resolve`",
                    copy.name
                ),
            ));
        }
        let platform = Platform::parse(&copy.platform).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported locked platform {}", copy.platform),
            )
        })?;
        let destination = output
            .join(platform.name().replace('/', "-"))
            .join(&copy.name);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&source, &destination)?;
        set_executable(&destination)?;
        locked.push(LockedTool {
            name: copy.name,
            version: "local".into(),
            platform: copy.platform,
            sha256: sha256_file(&destination)?,
            path: destination
                .strip_prefix(root)
                .unwrap_or(&destination)
                .display()
                .to_string(),
        });
    }
    let lock = Lockfile {
        format: 1,
        tools: locked,
    };
    let bytes = serde_json::to_vec_pretty(&lock).map_err(io::Error::other)?;
    fs::write(root.join(".binport/toolbox.json"), bytes)?;
    Ok(lock)
}

pub fn export(root: &Path, output: &Path) -> io::Result<()> {
    let manifest = root.join(".binport/toolbox.json");
    let toolbox = root.join(".binport/toolbox");
    if !manifest.is_file() || !toolbox.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "toolbox is not built; run `binport build` first",
        ));
    }
    let file = File::create(output)?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut archive = TarBuilder::new(encoder);
    archive.append_path_with_name(&manifest, ".binport/toolbox.json")?;
    append_directory(&mut archive, &toolbox, Path::new(".binport/toolbox"))?;
    archive.finish()?;
    Ok(())
}

pub fn load(root: &Path, input: &Path) -> io::Result<Lockfile> {
    let staging = root.join(format!(".binport-load-{}", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir_all(&staging)?;
    let result = (|| {
        let mut archive = TarArchive::new(GzDecoder::new(File::open(input)?));
        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.into_owned();
            let allowed_path =
                path == Path::new(".binport/toolbox.json") || path.starts_with(".binport/toolbox");
            let allowed_type =
                entry.header().entry_type().is_file() || entry.header().entry_type().is_dir();
            if !allowed_path || !allowed_type || !entry.unpack_in(&staging)? {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("toolbox contains an unsafe entry: {}", path.display()),
                ));
            }
        }
        let imported = staging.join(".binport");
        let lock: Lockfile = serde_json::from_slice(&fs::read(imported.join("toolbox.json"))?)
            .map_err(io::Error::other)?;
        for tool in &lock.tools {
            let relative = Path::new(&tool.path);
            let relative = relative.strip_prefix(".binport").map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid toolbox manifest path")
            })?;
            let path = imported.join(relative);
            if sha256_file(&path)? != tool.sha256 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("checksum mismatch for imported {}", tool.name),
                ));
            }
        }
        let destination = root.join(".binport");
        fs::create_dir_all(&destination)?;
        let old_toolbox = destination.join("toolbox");
        if old_toolbox.exists() {
            fs::remove_dir_all(&old_toolbox)?;
        }
        fs::rename(imported.join("toolbox"), &old_toolbox)?;
        fs::rename(
            imported.join("toolbox.json"),
            destination.join("toolbox.json"),
        )?;
        Ok(lock)
    })();
    let _ = fs::remove_dir_all(&staging);
    result
}

fn append_directory(
    archive: &mut TarBuilder<GzEncoder<File>>,
    source: &Path,
    name: &Path,
) -> io::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        let archive_path = name.join(entry.file_name());
        if path.is_dir() {
            append_directory(archive, &path, &archive_path)?;
        } else if path.is_file() {
            archive.append_path_with_name(path, archive_path)?;
        }
    }
    Ok(())
}

pub fn fetch(tool: &str, platform: Platform) -> io::Result<PathBuf> {
    let artifact = catalog::artifact(tool, None, platform)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("unknown tool {tool:?}")))?;
    fetch_artifact(artifact)
}

pub fn cache_root() -> io::Result<PathBuf> {
    if let Some(path) = env::var_os("BINPORT_CACHE_DIR") {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = env::var_os("XDG_CACHE_HOME") {
        return Ok(PathBuf::from(path).join("binport"));
    }
    #[cfg(windows)]
    if let Some(path) = dirs::cache_dir() {
        return Ok(path.join("binport"));
    }
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "user cache directory is unavailable",
            )
        })?;
    Ok(home.join(".cache/binport"))
}

pub fn fetch_artifact(artifact: Artifact) -> io::Result<PathBuf> {
    fetch_source(
        artifact.tool,
        artifact.version,
        artifact.platform,
        artifact.url,
        artifact.sha256,
        artifact.archive,
    )
}

fn fetch_resolved(tool: &ResolvedTool) -> io::Result<PathBuf> {
    let platform = Platform::parse(&tool.platform).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid platform in Binport.lock",
        )
    })?;
    let archive = Archive::parse(&tool.archive).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid archive type in Binport.lock",
        )
    })?;
    fetch_source(
        &tool.name,
        &tool.version,
        platform,
        &tool.source,
        &tool.source_sha256,
        archive,
    )
}

fn fetch_source(
    tool: &str,
    version: &str,
    platform: Platform,
    url: &str,
    expected_sha256: &str,
    archive: Archive,
) -> io::Result<PathBuf> {
    let destination = cache_root()?
        .join("downloads")
        .join(platform.name().replace('/', "-"))
        .join(version)
        .join(tool);
    if destination.is_file() {
        let expected = if matches!(archive, Archive::Binary) {
            Some(expected_sha256.to_owned())
        } else {
            fs::read_to_string(destination.with_extension("sha256")).ok()
        };
        if expected.as_deref() == Some(sha256_file(&destination)?.as_str()) {
            return Ok(destination);
        }
    }
    eprintln!(
        "binport: fetching {}@{} for {}",
        tool,
        version,
        platform.name()
    );
    let mut response = reqwest::blocking::get(url)
        .map_err(io::Error::other)?
        .error_for_status()
        .map_err(io::Error::other)?;
    let progress = TransferProgress::new(
        format!("fetch {tool}@{version}"),
        response.content_length(),
        true,
    );
    let mut bytes = Vec::with_capacity(
        response
            .content_length()
            .and_then(|size| usize::try_from(size).ok())
            .unwrap_or(0),
    );
    let mut hasher = sha2::Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = response.read(&mut buffer).map_err(io::Error::other)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        bytes.extend_from_slice(&buffer[..read]);
        progress.inc(read);
    }
    progress.finish();
    let archive_hash = format!("{:x}", hasher.finalize());
    if archive_hash != expected_sha256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "checksum mismatch for {}: expected {}, got {archive_hash}",
                tool, expected_sha256
            ),
        ));
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = destination.with_extension(format!("tmp.{}", std::process::id()));
    match archive {
        Archive::Binary => fs::write(&temp, &bytes)?,
        Archive::TarGz => extract_binary(&bytes, tool, &temp)?,
    }
    set_executable(&temp)?;
    let binary_hash = sha256_file(&temp)?;
    fs::rename(&temp, &destination)?;
    fs::write(destination.with_extension("sha256"), binary_hash)?;
    Ok(destination)
}

fn extract_binary(bytes: &[u8], name: &str, destination: &Path) -> io::Result<()> {
    let mut archive = TarArchive::new(GzDecoder::new(Cursor::new(bytes)));
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.path()?.file_name().and_then(|part| part.to_str()) == Some(name)
            && entry.header().entry_type().is_file()
        {
            let mut output = File::create(destination)?;
            io::copy(&mut entry, &mut output)?;
            output.flush()?;
            return Ok(());
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("archive does not contain {name}"),
    ))
}

#[cfg(unix)]
fn set_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_exports_and_loads_a_copied_tool() {
        let source_project = tempfile::tempdir().unwrap();
        let destination_project = tempfile::tempdir().unwrap();
        fs::write(
            source_project.path().join("hello"),
            b"#!/bin/sh\necho hello\n",
        )
        .unwrap();
        fs::write(
            source_project.path().join("Binfile"),
            "COPY ./hello hello --target linux/amd64\n",
        )
        .unwrap();
        let built = build(
            source_project.path(),
            &source_project.path().join("Binfile"),
        )
        .unwrap();
        assert_eq!(built.tools.len(), 1);

        let archive = source_project.path().join("hello.toolbox");
        export(source_project.path(), &archive).unwrap();
        let loaded = load(destination_project.path(), &archive).unwrap();
        assert_eq!(loaded.tools[0].name, "hello");
        assert_eq!(
            fs::read(
                destination_project
                    .path()
                    .join(".binport/toolbox/linux-amd64/hello")
            )
            .unwrap(),
            b"#!/bin/sh\necho hello\n"
        );
    }
}
