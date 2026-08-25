use crate::binfile::Binfile;
use crate::catalog;
use crate::{safe_tool_name, sha256_file};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const LOCKFILE_NAME: &str = "Binport.lock";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectLock {
    pub format: u32,
    pub binfile_sha256: String,
    pub tools: Vec<ResolvedTool>,
    pub copies: Vec<ResolvedCopy>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResolvedTool {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub source: String,
    pub source_sha256: String,
    pub archive: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResolvedCopy {
    pub name: String,
    pub platform: String,
    pub source: String,
    pub sha256: String,
}

impl ProjectLock {
    pub fn read(root: &Path) -> io::Result<Self> {
        let lock: Self = serde_json::from_slice(&fs::read(root.join(LOCKFILE_NAME))?)
            .map_err(io::Error::other)?;
        if lock.format != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported Binport.lock format {}", lock.format),
            ));
        }
        Ok(lock)
    }

    pub fn write(&self, root: &Path) -> io::Result<()> {
        let mut bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        bytes.push(b'\n');
        fs::write(root.join(LOCKFILE_NAME), bytes)
    }

    pub fn verify_binfile(&self, binfile: &Path) -> io::Result<()> {
        let actual = hash_bytes(&fs::read(binfile)?);
        if actual != self.binfile_sha256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Binfile changed since Binport.lock was generated; run `binport resolve`",
            ));
        }
        Ok(())
    }
}

pub fn resolve(root: &Path, binfile_path: &Path) -> io::Result<ProjectLock> {
    let spec = Binfile::read(binfile_path)?;
    let mut tools = Vec::new();
    let mut identities = BTreeSet::new();
    for platform in spec.platforms {
        for tool in &spec.tools {
            let artifact = catalog::artifact(&tool.name, tool.version.as_deref(), platform)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!(
                            "no catalog artifact for {}{} on {}",
                            tool.name,
                            tool.version
                                .as_ref()
                                .map(|version| format!("@{version}"))
                                .unwrap_or_default(),
                            platform.name()
                        ),
                    )
                })?;
            safe_tool_name(Path::new(artifact.tool))?;
            if !identities.insert((artifact.tool.to_owned(), platform.name().to_owned())) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("duplicate tool {} for {}", artifact.tool, platform.name()),
                ));
            }
            tools.push(ResolvedTool {
                name: artifact.tool.into(),
                version: artifact.version.into(),
                platform: artifact.platform.name().into(),
                source: artifact.url.into(),
                source_sha256: artifact.sha256.into(),
                archive: artifact.archive.name().into(),
            });
        }
    }
    let binfile_root = binfile_path.parent().unwrap_or(root);
    let mut copies = Vec::new();
    for copy in spec.copies {
        safe_tool_name(Path::new(&copy.name))?;
        if !identities.insert((copy.name.clone(), copy.platform.name().to_owned())) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("duplicate tool {} for {}", copy.name, copy.platform.name()),
            ));
        }
        let source = binfile_root.join(&copy.source).canonicalize()?;
        copies.push(ResolvedCopy {
            name: copy.name,
            platform: copy.platform.name().into(),
            source: relative_or_absolute(root, &source),
            sha256: sha256_file(&source)?,
        });
    }
    let lock = ProjectLock {
        format: 1,
        binfile_sha256: hash_bytes(&fs::read(binfile_path)?),
        tools,
        copies,
    };
    lock.write(root)?;
    Ok(lock)
}

pub fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn relative_or_absolute(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

pub fn source_path(root: &Path, source: &str) -> PathBuf {
    let path = Path::new(source);
    if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_catalog_entries_and_detects_binfile_changes() {
        let root = tempfile::tempdir().unwrap();
        let binfile = root.path().join("Binfile");
        fs::write(&binfile, "TARGET linux/amd64\nTOOL rg@15.2.0\n").unwrap();
        let lock = resolve(root.path(), &binfile).unwrap();
        assert_eq!(lock.tools.len(), 1);
        assert_eq!(lock.tools[0].archive, "tar+gzip");
        assert!(root.path().join(LOCKFILE_NAME).is_file());
        lock.verify_binfile(&binfile).unwrap();
        fs::write(&binfile, "TARGET linux/amd64\nTOOL jq@1.8.2\n").unwrap();
        assert!(lock.verify_binfile(&binfile).is_err());
    }
}
