use crate::catalog::Platform;
use crate::lockfile::{ProjectLock, hash_bytes};
use crate::toolbox::{LockedTool, Lockfile};
use crate::{safe_tool_name, sha256_file};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const INDEX_MEDIA_TYPE: &str = "application/vnd.oci.image.index.v1+json";
const MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
const CONFIG_MEDIA_TYPE: &str = "application/vnd.binport.toolbox.config.v1+json";
const TOOL_MEDIA_TYPE: &str = "application/vnd.binport.tool.v1";

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Index {
    schema_version: u32,
    media_type: String,
    manifests: Vec<Descriptor>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    schema_version: u32,
    media_type: String,
    config: Descriptor,
    layers: Vec<Descriptor>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Descriptor {
    media_type: String,
    digest: String,
    size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    platform: Option<OciPlatform>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    annotations: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct OciPlatform {
    os: String,
    architecture: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct ToolboxConfig {
    format: u32,
    platform: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project_lock: Option<ProjectLock>,
}

pub fn pack(root: &Path, output: &Path) -> io::Result<()> {
    let lock: Lockfile = serde_json::from_slice(&fs::read(root.join(".binport/toolbox.json"))?)
        .map_err(io::Error::other)?;
    if output.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("OCI layout already exists: {}", output.display()),
        ));
    }
    fs::create_dir_all(output.join("blobs/sha256"))?;
    fs::write(
        output.join("oci-layout"),
        b"{\"imageLayoutVersion\":\"1.0.0\"}\n",
    )?;
    let project_lock = ProjectLock::read(root).ok();

    let mut manifests = Vec::new();
    for platform in Platform::ALL {
        let tools = lock
            .tools
            .iter()
            .filter(|tool| tool.platform == platform.name())
            .collect::<Vec<_>>();
        if tools.is_empty() {
            continue;
        }
        let config = serde_json::to_vec(&ToolboxConfig {
            format: 1,
            platform: platform.name().into(),
            project_lock: project_lock.clone(),
        })
        .map_err(io::Error::other)?;
        let config = write_blob(output, &config, CONFIG_MEDIA_TYPE)?;
        let mut layers = Vec::new();
        for tool in tools {
            safe_tool_name(Path::new(&tool.name))?;
            let path = root.join(&tool.path);
            let bytes = fs::read(&path)?;
            let actual = hash_bytes(&bytes);
            if actual != tool.sha256 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("checksum mismatch for built tool {}", tool.name),
                ));
            }
            let mut descriptor = write_blob(output, &bytes, TOOL_MEDIA_TYPE)?;
            descriptor
                .annotations
                .insert("org.binport.tool.name".into(), tool.name.clone());
            descriptor
                .annotations
                .insert("org.binport.tool.version".into(), tool.version.clone());
            layers.push(descriptor);
        }
        let manifest = serde_json::to_vec(&Manifest {
            schema_version: 2,
            media_type: MANIFEST_MEDIA_TYPE.into(),
            config,
            layers,
        })
        .map_err(io::Error::other)?;
        let mut descriptor = write_blob(output, &manifest, MANIFEST_MEDIA_TYPE)?;
        descriptor.platform = Some(to_oci_platform(platform));
        manifests.push(descriptor);
    }
    let index = serde_json::to_vec_pretty(&Index {
        schema_version: 2,
        media_type: INDEX_MEDIA_TYPE.into(),
        manifests,
    })
    .map_err(io::Error::other)?;
    fs::write(output.join("index.json"), index)?;
    Ok(())
}

pub fn unpack(layout: &Path, root: &Path) -> io::Result<Lockfile> {
    validate_layout(layout)?;
    let index: Index =
        serde_json::from_slice(&fs::read(layout.join("index.json"))?).map_err(io::Error::other)?;
    if index.schema_version != 2 || index.media_type != INDEX_MEDIA_TYPE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported OCI index",
        ));
    }
    let staging = root.join(format!(".binport-oci-{}", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir_all(staging.join("toolbox"))?;
    let result = (|| {
        let mut tools = Vec::new();
        let mut project_lock: Option<ProjectLock> = None;
        let mut identities = BTreeSet::new();
        let mut platforms = BTreeSet::new();
        for manifest_descriptor in index.manifests {
            let platform =
                from_oci_platform(manifest_descriptor.platform.as_ref().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "manifest has no platform")
                })?)?;
            if !platforms.insert(platform.name()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("duplicate OCI platform {}", platform.name()),
                ));
            }
            let manifest: Manifest =
                serde_json::from_slice(&read_blob(layout, &manifest_descriptor)?)
                    .map_err(io::Error::other)?;
            if manifest.schema_version != 2 || manifest.media_type != MANIFEST_MEDIA_TYPE {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unsupported OCI manifest",
                ));
            }
            let config: ToolboxConfig =
                serde_json::from_slice(&read_blob(layout, &manifest.config)?)
                    .map_err(io::Error::other)?;
            if config.format != 1 || config.platform != platform.name() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "toolbox config does not match manifest platform",
                ));
            }
            if let Some(lock) = config.project_lock {
                if project_lock
                    .as_ref()
                    .is_some_and(|existing| existing != &lock)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "platform manifests contain different Binport.lock files",
                    ));
                }
                project_lock = Some(lock);
            }
            for layer in manifest.layers {
                if layer.media_type != TOOL_MEDIA_TYPE {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("unsupported toolbox layer {}", layer.media_type),
                    ));
                }
                let name = annotation(&layer, "org.binport.tool.name")?;
                let version = annotation(&layer, "org.binport.tool.version")?;
                safe_tool_name(Path::new(name))?;
                if !identities.insert((name.to_owned(), platform.name())) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("duplicate OCI tool {name} for {}", platform.name()),
                    ));
                }
                let relative = PathBuf::from(".binport/toolbox")
                    .join(platform.name().replace('/', "-"))
                    .join(name);
                let destination = staging
                    .join("toolbox")
                    .join(platform.name().replace('/', "-"))
                    .join(name);
                fs::create_dir_all(destination.parent().expect("tool path has parent"))?;
                fs::write(&destination, read_blob(layout, &layer)?)?;
                set_executable(&destination)?;
                tools.push(LockedTool {
                    name: name.into(),
                    version: version.into(),
                    platform: platform.name().into(),
                    sha256: sha256_file(&destination)?,
                    path: relative.display().to_string(),
                });
            }
        }
        let lock = Lockfile { format: 1, tools };
        let mut manifest = serde_json::to_vec_pretty(&lock).map_err(io::Error::other)?;
        manifest.push(b'\n');
        fs::write(staging.join("toolbox.json"), manifest)?;
        let destination = root.join(".binport");
        fs::create_dir_all(&destination)?;
        for name in ["toolbox", "toolbox.json"] {
            let old = destination.join(name);
            if old.exists() {
                if old.is_dir() {
                    fs::remove_dir_all(&old)?;
                } else {
                    fs::remove_file(&old)?;
                }
            }
            fs::rename(staging.join(name), old)?;
        }
        if let Some(project_lock) = project_lock {
            project_lock.write(root)?;
        }
        Ok(lock)
    })();
    let _ = fs::remove_dir_all(staging);
    result
}

fn write_blob(layout: &Path, bytes: &[u8], media_type: &str) -> io::Result<Descriptor> {
    let hash = hash_bytes(bytes);
    let path = layout.join("blobs/sha256").join(&hash);
    if !path.exists() {
        fs::write(path, bytes)?;
    }
    Ok(Descriptor {
        media_type: media_type.into(),
        digest: format!("sha256:{hash}"),
        size: bytes.len() as u64,
        platform: None,
        annotations: BTreeMap::new(),
    })
}

fn read_blob(layout: &Path, descriptor: &Descriptor) -> io::Result<Vec<u8>> {
    let hash = descriptor.digest.strip_prefix("sha256:").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "only sha256 OCI digests are supported",
        )
    })?;
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid OCI digest",
        ));
    }
    let bytes = fs::read(layout.join("blobs/sha256").join(hash))?;
    if bytes.len() as u64 != descriptor.size || hash_bytes(&bytes) != hash {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("OCI blob verification failed for {}", descriptor.digest),
        ));
    }
    Ok(bytes)
}

fn validate_layout(layout: &Path) -> io::Result<()> {
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(layout.join("oci-layout"))?).map_err(io::Error::other)?;
    if value["imageLayoutVersion"] != "1.0.0" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported OCI layout version",
        ));
    }
    Ok(())
}

fn annotation<'a>(descriptor: &'a Descriptor, key: &str) -> io::Result<&'a str> {
    descriptor
        .annotations
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("missing {key}")))
}

fn to_oci_platform(platform: Platform) -> OciPlatform {
    let architecture = match platform {
        Platform::LinuxAmd64 => "amd64",
        Platform::LinuxArm64 => "arm64",
    };
    OciPlatform {
        os: "linux".into(),
        architecture: architecture.into(),
    }
}

fn from_oci_platform(platform: &OciPlatform) -> io::Result<Platform> {
    Platform::parse(&format!("{}/{}", platform.os, platform.architecture)).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "unsupported OCI platform {}/{}",
                platform.os, platform.architecture
            ),
        )
    })
}

#[cfg(unix)]
fn set_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_and_unpacks_a_multi_platform_toolbox() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let layout_parent = tempfile::tempdir().unwrap();
        let layout = layout_parent.path().join("ops.oci");
        let mut tools = Vec::new();
        for (platform, contents) in [("linux/amd64", b"amd64"), ("linux/arm64", b"arm64")] {
            let relative = PathBuf::from(".binport/toolbox")
                .join(platform.replace('/', "-"))
                .join("demo");
            let path = source.path().join(&relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
            tools.push(LockedTool {
                name: "demo".into(),
                version: "1.0.0".into(),
                platform: platform.into(),
                sha256: sha256_file(&path).unwrap(),
                path: relative.display().to_string(),
            });
        }
        fs::write(
            source.path().join(".binport/toolbox.json"),
            serde_json::to_vec(&Lockfile { format: 1, tools }).unwrap(),
        )
        .unwrap();

        pack(source.path(), &layout).unwrap();
        let restored = unpack(&layout, destination.path()).unwrap();
        assert_eq!(restored.tools.len(), 2);
        for tool in restored.tools {
            assert_eq!(
                sha256_file(&destination.path().join(&tool.path)).unwrap(),
                tool.sha256
            );
        }

        let blob = fs::read_dir(layout.join("blobs/sha256"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        fs::write(blob, b"tampered").unwrap();
        let rejected = tempfile::tempdir().unwrap();
        assert!(unpack(&layout, rejected.path()).is_err());
    }
}
