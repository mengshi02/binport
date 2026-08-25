use crate::lockfile::hash_bytes;
use reqwest::Method;
use reqwest::blocking::{Client, Response};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, LOCATION, WWW_AUTHENTICATE};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Duration;

const OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";
const OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const DOCKER_INDEX: &str = "application/vnd.docker.distribution.manifest.list.v2+json";
const DOCKER_MANIFEST: &str = "application/vnd.docker.distribution.manifest.v2+json";
const ACCEPT_MANIFESTS: &str = "application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.docker.distribution.manifest.v2+json";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reference {
    pub registry: String,
    pub repository: String,
    pub reference: String,
}

#[derive(Debug)]
pub struct PullReport {
    pub digest: String,
    pub downloaded: usize,
    pub cached: usize,
}

#[derive(Clone, Debug)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

#[derive(Debug)]
pub struct PushReport {
    pub digest: String,
    pub uploaded: usize,
    pub existing: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Document {
    #[serde(default)]
    media_type: String,
    #[serde(default)]
    manifests: Vec<Descriptor>,
    #[serde(default)]
    config: Option<Descriptor>,
    #[serde(default)]
    layers: Vec<Descriptor>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Descriptor {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    media_type: String,
    digest: String,
    size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    platform: Option<RegistryPlatform>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RegistryPlatform {
    os: String,
    architecture: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalIndex<'a> {
    schema_version: u32,
    media_type: &'static str,
    manifests: &'a [Descriptor],
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
}

impl Reference {
    pub fn parse(value: &str) -> io::Result<Self> {
        let value = value.strip_prefix("oci://").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "OCI reference must start with oci://",
            )
        })?;
        let (registry, rest) = value.split_once('/').ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "OCI reference must include a registry and repository",
            )
        })?;
        if registry.is_empty() || rest.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "OCI registry and repository cannot be empty",
            ));
        }
        let (repository, reference) = if let Some((repository, digest)) = rest.rsplit_once('@') {
            (repository, digest)
        } else {
            let last_slash = rest.rfind('/').map_or(0, |index| index + 1);
            if let Some(colon) = rest[last_slash..].rfind(':') {
                let colon = last_slash + colon;
                (&rest[..colon], &rest[colon + 1..])
            } else {
                (rest, "latest")
            }
        };
        if repository.is_empty()
            || reference.is_empty()
            || !repository.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-')
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid OCI repository or reference",
            ));
        }
        Ok(Self {
            registry: registry.into(),
            repository: repository.into(),
            reference: reference.into(),
        })
    }

    fn manifest_url(&self, reference: &str, scheme: &str) -> String {
        format!(
            "{scheme}://{}/v2/{}/manifests/{reference}",
            self.registry, self.repository
        )
    }

    fn blob_url(&self, digest: &str, scheme: &str) -> String {
        format!(
            "{scheme}://{}/v2/{}/blobs/{digest}",
            self.registry, self.repository
        )
    }

    fn uploads_url(&self, scheme: &str) -> String {
        format!(
            "{scheme}://{}/v2/{}/blobs/uploads/",
            self.registry, self.repository
        )
    }
}

pub fn push_layout(
    reference: &Reference,
    layout: &Path,
    plain_http: bool,
    credentials: Option<Credentials>,
) -> io::Result<PushReport> {
    let index_bytes = fs::read(layout.join("index.json"))?;
    let index: Document = serde_json::from_slice(&index_bytes).map_err(io::Error::other)?;
    if index.media_type != OCI_INDEX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "toolbox layout does not contain an OCI index",
        ));
    }
    let root_digest = format!("sha256:{}", hash_bytes(&index_bytes));
    let mut registry = RegistryClient::new(reference.clone(), plain_http, credentials)?;
    if let Some(existing_index) = registry.get_manifest_optional(&reference.reference)?
        && format!("sha256:{}", hash_bytes(&existing_index)) == root_digest
    {
        let mut blobs = BTreeSet::new();
        for manifest_descriptor in &index.manifests {
            let manifest: Document =
                serde_json::from_slice(&read_layout_blob(layout, manifest_descriptor)?)
                    .map_err(io::Error::other)?;
            for descriptor in manifest.config.iter().chain(manifest.layers.iter()) {
                blobs.insert(descriptor.digest.clone());
            }
        }
        return Ok(PushReport {
            digest: root_digest,
            uploaded: 0,
            existing: blobs.len(),
        });
    }
    let mut uploaded = 0;
    let mut existing = 0;
    for manifest_descriptor in &index.manifests {
        let manifest_bytes = read_layout_blob(layout, manifest_descriptor)?;
        let manifest: Document =
            serde_json::from_slice(&manifest_bytes).map_err(io::Error::other)?;
        for descriptor in manifest.config.iter().chain(manifest.layers.iter()) {
            if registry.blob_exists(descriptor)? {
                existing += 1;
            } else {
                let bytes = read_layout_blob(layout, descriptor)?;
                registry.upload_blob(descriptor, &bytes)?;
                uploaded += 1;
            }
        }
        registry.put_manifest(
            &manifest_descriptor.digest,
            if manifest.media_type.is_empty() {
                OCI_MANIFEST
            } else {
                &manifest.media_type
            },
            &manifest_bytes,
        )?;
    }
    registry.put_manifest(&reference.reference, OCI_INDEX, &index_bytes)?;
    Ok(PushReport {
        digest: root_digest,
        uploaded,
        existing,
    })
}

pub fn pull_layout(
    reference: &Reference,
    output: &Path,
    cache_root: &Path,
    plain_http: bool,
    credentials: Option<Credentials>,
) -> io::Result<PullReport> {
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
    let mut registry = RegistryClient::new(reference.clone(), plain_http, credentials)?;
    let root = registry.get_bytes(
        &reference.manifest_url(&reference.reference, registry.scheme),
        Some(ACCEPT_MANIFESTS),
    )?;
    let root_digest = format!("sha256:{}", hash_bytes(&root));
    if reference.reference.starts_with("sha256:") && reference.reference != root_digest {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "registry returned {root_digest}, expected {}",
                reference.reference
            ),
        ));
    }
    let document: Document = serde_json::from_slice(&root).map_err(io::Error::other)?;
    if document.media_type != OCI_INDEX && document.media_type != DOCKER_INDEX {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "registry reference is not a multi-platform OCI index ({})",
                document.media_type
            ),
        ));
    }
    let cache = cache_root.join("oci/blobs/sha256");
    fs::create_dir_all(&cache)?;
    let mut downloaded = 0;
    let mut cached = 0;
    let manifests = document
        .manifests
        .into_iter()
        .filter(supported_platform)
        .collect::<Vec<_>>();
    if manifests.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "OCI index contains no linux/amd64 or linux/arm64 manifests",
        ));
    }
    for manifest in &manifests {
        let bytes = registry.get_bytes(
            &reference.manifest_url(&manifest.digest, registry.scheme),
            Some(ACCEPT_MANIFESTS),
        )?;
        verify_bytes(&bytes, manifest)?;
        store_bytes(&cache, output, &manifest.digest, &bytes)?;
        let manifest_document: Document =
            serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if manifest_document.media_type != OCI_MANIFEST
            && manifest_document.media_type != DOCKER_MANIFEST
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("unsupported manifest type {}", manifest_document.media_type),
            ));
        }
        let descriptors = manifest_document
            .config
            .iter()
            .chain(manifest_document.layers.iter());
        for descriptor in descriptors {
            if materialize_cached(&cache, output, descriptor)? {
                cached += 1;
                continue;
            }
            registry.download_blob(
                &reference.blob_url(&descriptor.digest, registry.scheme),
                descriptor,
                &cache,
            )?;
            materialize(&cache, output, &descriptor.digest)?;
            downloaded += 1;
        }
    }
    let local_index = serde_json::to_vec_pretty(&LocalIndex {
        schema_version: 2,
        media_type: OCI_INDEX,
        manifests: &manifests,
    })
    .map_err(io::Error::other)?;
    fs::write(output.join("index.json"), local_index)?;
    Ok(PullReport {
        digest: root_digest,
        downloaded,
        cached,
    })
}

fn supported_platform(descriptor: &Descriptor) -> bool {
    descriptor.platform.as_ref().is_some_and(|platform| {
        platform.os == "linux" && matches!(platform.architecture.as_str(), "amd64" | "arm64")
    })
}

struct RegistryClient {
    client: Client,
    reference: Reference,
    token: Option<String>,
    scheme: &'static str,
    credentials: Option<Credentials>,
}

impl RegistryClient {
    fn new(
        reference: Reference,
        plain_http: bool,
        credentials: Option<Credentials>,
    ) -> io::Result<Self> {
        Ok(Self {
            client: Client::builder()
                .user_agent(concat!("binport/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(60))
                .build()
                .map_err(io::Error::other)?,
            reference,
            token: None,
            scheme: if plain_http { "http" } else { "https" },
            credentials,
        })
    }

    fn get(&mut self, url: &str, accept: Option<&str>) -> io::Result<Response> {
        self.send(Method::GET, url, accept, None, None)?
            .error_for_status()
            .map_err(io::Error::other)
    }

    fn send(
        &mut self,
        method: Method,
        url: &str,
        accept: Option<&str>,
        content_type: Option<&str>,
        body: Option<&[u8]>,
    ) -> io::Result<Response> {
        let send = |token: Option<&str>| {
            let mut request = self.client.request(method.clone(), url);
            if let Some(accept) = accept {
                request = request.header(ACCEPT, accept);
            }
            if let Some(content_type) = content_type {
                request = request.header(CONTENT_TYPE, content_type);
            }
            if let Some(token) = token {
                request = request.header(AUTHORIZATION, format!("Bearer {token}"));
            } else if let Some(credentials) = &self.credentials {
                request = request.basic_auth(&credentials.username, Some(&credentials.password));
            }
            if let Some(body) = body {
                request = request.body(body.to_vec());
            }
            request.send()
        };
        let mut response = send(self.token.as_deref()).map_err(io::Error::other)?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            let challenge = response
                .headers()
                .get(WWW_AUTHENTICATE)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "registry requires authentication without a bearer challenge",
                    )
                })?
                .to_owned();
            let token = self.fetch_anonymous_token(&challenge)?;
            self.token = Some(token);
            response = send(self.token.as_deref()).map_err(io::Error::other)?;
        }
        Ok(response)
    }

    fn get_bytes(&mut self, url: &str, accept: Option<&str>) -> io::Result<Vec<u8>> {
        self.get(url, accept)?
            .bytes()
            .map(|bytes| bytes.to_vec())
            .map_err(io::Error::other)
    }

    fn get_manifest_optional(&mut self, reference: &str) -> io::Result<Option<Vec<u8>>> {
        let url = self.reference.manifest_url(reference, self.scheme);
        let response = self.send(Method::GET, &url, Some(ACCEPT_MANIFESTS), None, None)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        response
            .error_for_status()
            .map_err(io::Error::other)?
            .bytes()
            .map(|bytes| Some(bytes.to_vec()))
            .map_err(io::Error::other)
    }

    fn fetch_anonymous_token(&self, challenge: &str) -> io::Result<String> {
        if !challenge
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Bearer"))
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "registry authentication scheme is not Bearer",
            ));
        }
        let realm = challenge_parameter(challenge, "realm").ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "Bearer challenge has no realm")
        })?;
        let mut url = reqwest::Url::parse(&realm).map_err(io::Error::other)?;
        {
            let mut query = url.query_pairs_mut();
            if let Some(service) = challenge_parameter(challenge, "service") {
                query.append_pair("service", &service);
            }
            let scope = challenge_parameter(challenge, "scope")
                .unwrap_or_else(|| format!("repository:{}:pull", self.reference.repository));
            query.append_pair("scope", &scope);
        }
        let mut request = self.client.get(url);
        if let Some(credentials) = &self.credentials {
            request = request.basic_auth(&credentials.username, Some(&credentials.password));
        }
        let bytes = request
            .send()
            .map_err(io::Error::other)?
            .error_for_status()
            .map_err(io::Error::other)?
            .bytes()
            .map_err(io::Error::other)?;
        let response: TokenResponse = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        response.token.or(response.access_token).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "token service returned no token",
            )
        })
    }

    fn download_blob(
        &mut self,
        url: &str,
        descriptor: &Descriptor,
        cache: &Path,
    ) -> io::Result<()> {
        let hash = digest_hash(&descriptor.digest)?;
        let final_path = cache.join(hash);
        let temporary = cache.join(format!("{hash}.tmp.{}", std::process::id()));
        let mut response = self.get(url, None)?;
        let mut file = File::create(&temporary)?;
        let mut hasher = Sha256::new();
        let mut size = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = response.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            file.write_all(&buffer[..read])?;
            hasher.update(&buffer[..read]);
            size += read as u64;
        }
        file.flush()?;
        let actual = format!("{:x}", hasher.finalize());
        if actual != hash || size != descriptor.size {
            let _ = fs::remove_file(&temporary);
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "registry blob verification failed for {}",
                    descriptor.digest
                ),
            ));
        }
        fs::rename(temporary, final_path)?;
        Ok(())
    }

    fn blob_exists(&mut self, descriptor: &Descriptor) -> io::Result<bool> {
        let url = self.reference.blob_url(&descriptor.digest, self.scheme);
        let response = self.send(Method::HEAD, &url, None, None, None)?;
        match response.status() {
            reqwest::StatusCode::OK => Ok(true),
            reqwest::StatusCode::NOT_FOUND => Ok(false),
            _ => response
                .error_for_status()
                .map(|_| false)
                .map_err(io::Error::other),
        }
    }

    fn upload_blob(&mut self, descriptor: &Descriptor, bytes: &[u8]) -> io::Result<()> {
        verify_bytes(bytes, descriptor)?;
        let url = self.reference.uploads_url(self.scheme);
        let response = self
            .send(Method::POST, &url, None, None, None)?
            .error_for_status()
            .map_err(io::Error::other)?;
        let location = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Registry upload response has no Location",
                )
            })?;
        let mut upload = if location.starts_with("http://") || location.starts_with("https://") {
            reqwest::Url::parse(location).map_err(io::Error::other)?
        } else {
            let base = format!("{}://{}", self.scheme, self.reference.registry);
            reqwest::Url::parse(&base)
                .map_err(io::Error::other)?
                .join(location)
                .map_err(io::Error::other)?
        };
        upload
            .query_pairs_mut()
            .append_pair("digest", &descriptor.digest);
        self.send(
            Method::PUT,
            upload.as_str(),
            None,
            Some("application/octet-stream"),
            Some(bytes),
        )?
        .error_for_status()
        .map_err(io::Error::other)?;
        Ok(())
    }

    fn put_manifest(&mut self, reference: &str, media_type: &str, bytes: &[u8]) -> io::Result<()> {
        let url = self.reference.manifest_url(reference, self.scheme);
        self.send(Method::PUT, &url, None, Some(media_type), Some(bytes))?
            .error_for_status()
            .map_err(io::Error::other)?;
        Ok(())
    }
}

fn challenge_parameter(challenge: &str, key: &str) -> Option<String> {
    let parameters = challenge.split_once(' ')?.1;
    for part in parameters.split(',') {
        let (name, value) = part.trim().split_once('=')?;
        if name.eq_ignore_ascii_case(key) {
            return Some(value.trim().trim_matches('"').to_owned());
        }
    }
    None
}

fn verify_bytes(bytes: &[u8], descriptor: &Descriptor) -> io::Result<()> {
    let hash = digest_hash(&descriptor.digest)?;
    if bytes.len() as u64 != descriptor.size || hash_bytes(bytes) != hash {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "registry manifest verification failed for {}",
                descriptor.digest
            ),
        ));
    }
    Ok(())
}

fn store_bytes(cache: &Path, layout: &Path, digest: &str, bytes: &[u8]) -> io::Result<()> {
    let hash = digest_hash(digest)?;
    let cached = cache.join(hash);
    if !cached.exists() {
        fs::write(&cached, bytes)?;
    }
    materialize(cache, layout, digest)
}

fn materialize_cached(cache: &Path, layout: &Path, descriptor: &Descriptor) -> io::Result<bool> {
    let hash = digest_hash(&descriptor.digest)?;
    let path = cache.join(hash);
    if !path.is_file() {
        return Ok(false);
    }
    let bytes = fs::read(&path)?;
    if bytes.len() as u64 != descriptor.size || hash_bytes(&bytes) != hash {
        fs::remove_file(path)?;
        return Ok(false);
    }
    materialize(cache, layout, &descriptor.digest)?;
    Ok(true)
}

fn materialize(cache: &Path, layout: &Path, digest: &str) -> io::Result<()> {
    let hash = digest_hash(digest)?;
    let source = cache.join(hash);
    let destination = layout.join("blobs/sha256").join(hash);
    if fs::hard_link(&source, &destination).is_err() {
        fs::copy(source, destination)?;
    }
    Ok(())
}

fn digest_hash(digest: &str) -> io::Result<&str> {
    let hash = digest.strip_prefix("sha256:").ok_or_else(|| {
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
    Ok(hash)
}

fn read_layout_blob(layout: &Path, descriptor: &Descriptor) -> io::Result<Vec<u8>> {
    let bytes = fs::read(
        layout
            .join("blobs/sha256")
            .join(digest_hash(&descriptor.digest)?),
    )?;
    verify_bytes(&bytes, descriptor)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci;
    use crate::sha256_file;
    use crate::toolbox::{LockedTool, Lockfile};
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::thread;

    #[test]
    fn parses_tags_digests_ports_and_default_tags() {
        assert_eq!(
            Reference::parse("oci://ghcr.io/acme/ops:v1").unwrap(),
            Reference {
                registry: "ghcr.io".into(),
                repository: "acme/ops".into(),
                reference: "v1".into(),
            }
        );
        assert_eq!(
            Reference::parse("oci://harbor.local:8443/team/ops@sha256:abc").unwrap(),
            Reference {
                registry: "harbor.local:8443".into(),
                repository: "team/ops".into(),
                reference: "sha256:abc".into(),
            }
        );
        assert_eq!(
            Reference::parse("oci://ghcr.io/acme/ops")
                .unwrap()
                .reference,
            "latest"
        );
    }

    #[test]
    fn parses_bearer_challenges() {
        let challenge = "Bearer realm=\"https://auth.example/token\",service=\"registry.example\",scope=\"repository:acme/ops:pull\"";
        assert_eq!(
            challenge_parameter(challenge, "realm").as_deref(),
            Some("https://auth.example/token")
        );
        assert_eq!(
            challenge_parameter(challenge, "scope").as_deref(),
            Some("repository:acme/ops:pull")
        );
    }

    #[test]
    fn pulls_and_unpacks_from_a_plain_http_registry() {
        let source = tempfile::tempdir().unwrap();
        let layout_parent = tempfile::tempdir().unwrap();
        let layout = layout_parent.path().join("source.oci");
        let relative = PathBuf::from(".binport/toolbox/linux-amd64/demo");
        let executable = source.path().join(&relative);
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, b"portable-demo").unwrap();
        fs::write(
            source.path().join(".binport/toolbox.json"),
            serde_json::to_vec(&Lockfile {
                format: 1,
                tools: vec![LockedTool {
                    name: "demo".into(),
                    version: "1.0.0".into(),
                    platform: "linux/amd64".into(),
                    sha256: sha256_file(&executable).unwrap(),
                    path: relative.display().to_string(),
                }],
            })
            .unwrap(),
        )
        .unwrap();
        oci::pack(source.path(), &layout).unwrap();

        let index = fs::read(layout.join("index.json")).unwrap();
        let document: Document = serde_json::from_slice(&index).unwrap();
        let manifest_count = document.manifests.len();
        let mut routes = HashMap::new();
        routes.insert("/v2/acme/ops/manifests/v1".to_owned(), index);
        for descriptor in document.manifests {
            let hash = digest_hash(&descriptor.digest).unwrap();
            let manifest = fs::read(layout.join("blobs/sha256").join(hash)).unwrap();
            routes.insert(
                format!("/v2/acme/ops/manifests/{}", descriptor.digest),
                manifest.clone(),
            );
            let manifest: Document = serde_json::from_slice(&manifest).unwrap();
            for blob in manifest.config.iter().chain(manifest.layers.iter()) {
                routes.insert(
                    format!("/v2/acme/ops/blobs/{}", blob.digest),
                    fs::read(
                        layout
                            .join("blobs/sha256")
                            .join(digest_hash(&blob.digest).unwrap()),
                    )
                    .unwrap(),
                );
            }
        }
        let expected_requests = routes.len() + 1 + manifest_count;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let routes = Arc::new(routes);
        let server = thread::spawn(move || {
            for stream in listener.incoming().take(expected_requests) {
                let mut stream = stream.unwrap();
                let request_line = {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    line
                };
                let path = request_line.split_whitespace().nth(1).unwrap();
                let body = routes.get(path).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(body).unwrap();
            }
        });

        let pulled_parent = tempfile::tempdir().unwrap();
        let pulled = pulled_parent.path().join("pulled.oci");
        let cache = tempfile::tempdir().unwrap();
        let reference = Reference::parse(&format!("oci://{address}/acme/ops:v1")).unwrap();
        let report = pull_layout(&reference, &pulled, cache.path(), true, None).unwrap();
        assert_eq!(report.downloaded, 2);
        let restored = tempfile::tempdir().unwrap();
        let lock = oci::unpack(&pulled, restored.path()).unwrap();
        assert_eq!(lock.tools.len(), 1);
        assert_eq!(
            fs::read(restored.path().join(&lock.tools[0].path)).unwrap(),
            b"portable-demo"
        );
        let pulled_again = pulled_parent.path().join("pulled-again.oci");
        let cached = pull_layout(&reference, &pulled_again, cache.path(), true, None).unwrap();
        assert_eq!(cached.downloaded, 0);
        assert_eq!(cached.cached, 2);
        server.join().unwrap();
    }

    #[test]
    fn follows_anonymous_bearer_challenge() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for stream in listener.incoming().take(3) {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    request.push_str(&line);
                }
                let first = request.lines().next().unwrap();
                if first.contains("/token") {
                    let body = b"{\"token\":\"test-token\"}";
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .unwrap();
                    stream.write_all(body).unwrap();
                } else if request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer test-token")
                {
                    let body = b"authorized";
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .unwrap();
                    stream.write_all(body).unwrap();
                } else {
                    write!(
                        stream,
                        "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Bearer realm=\"http://{address}/token\",service=\"test\",scope=\"repository:acme/ops:pull\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .unwrap();
                }
            }
        });
        let reference = Reference::parse(&format!("oci://{address}/acme/ops:v1")).unwrap();
        let url = reference.manifest_url("v1", "http");
        let mut client = RegistryClient::new(reference, true, None).unwrap();
        assert_eq!(client.get_bytes(&url, None).unwrap(), b"authorized");
        server.join().unwrap();
    }

    #[test]
    fn pushes_only_missing_blobs() {
        let source = tempfile::tempdir().unwrap();
        let layout_parent = tempfile::tempdir().unwrap();
        let layout = layout_parent.path().join("push.oci");
        let relative = PathBuf::from(".binport/toolbox/linux-amd64/demo");
        let executable = source.path().join(&relative);
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, b"push-demo").unwrap();
        fs::write(
            source.path().join(".binport/toolbox.json"),
            serde_json::to_vec(&Lockfile {
                format: 1,
                tools: vec![LockedTool {
                    name: "demo".into(),
                    version: "1.0.0".into(),
                    platform: "linux/amd64".into(),
                    sha256: sha256_file(&executable).unwrap(),
                    path: relative.display().to_string(),
                }],
            })
            .unwrap(),
        )
        .unwrap();
        oci::pack(source.path(), &layout).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let tag_body = Arc::new(Mutex::new(None::<Vec<u8>>));
        let server_tag_body = Arc::clone(&tag_body);
        let server = thread::spawn(move || {
            for stream in listener.incoming().take(10) {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();
                let mut content_length = 0_usize;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        content_length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; content_length];
                reader.read_exact(&mut body).unwrap();
                let method = request_line.split_whitespace().next().unwrap();
                let path = request_line.split_whitespace().nth(1).unwrap();
                let (status, headers, response_body) = match method {
                    "GET" if path.ends_with("/manifests/v1") => {
                        if let Some(body) = server_tag_body.lock().unwrap().clone() {
                            ("200 OK", "", body)
                        } else {
                            ("404 Not Found", "", Vec::new())
                        }
                    }
                    "HEAD" if server_tag_body.lock().unwrap().is_some() => {
                        ("200 OK", "", Vec::new())
                    }
                    "HEAD" => ("404 Not Found", "", Vec::new()),
                    "POST" => (
                        "202 Accepted",
                        "Location: /v2/acme/ops/blobs/uploads/test-upload\r\n",
                        Vec::new(),
                    ),
                    "PUT" if path.contains("/blobs/uploads/") => ("201 Created", "", Vec::new()),
                    "PUT" if path.ends_with("/manifests/v1") => {
                        *server_tag_body.lock().unwrap() = Some(body);
                        ("201 Created", "", Vec::new())
                    }
                    "PUT" if path.contains("/manifests/") => ("201 Created", "", Vec::new()),
                    _ => panic!("unexpected registry request {request_line}"),
                };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    response_body.len()
                )
                .unwrap();
                stream.write_all(&response_body).unwrap();
            }
        });
        let reference = Reference::parse(&format!("oci://{address}/acme/ops:v1")).unwrap();
        let first = push_layout(&reference, &layout, true, None).unwrap();
        assert_eq!(first.uploaded, 2);
        assert_eq!(first.existing, 0);
        let second = push_layout(&reference, &layout, true, None).unwrap();
        assert_eq!(second.uploaded, 0);
        assert_eq!(second.existing, 2);
        server.join().unwrap();
    }
}
