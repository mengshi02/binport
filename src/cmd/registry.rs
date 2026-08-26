use binport::toolbox;
use clap::Args;
use std::fs;
use std::io;
use std::path::PathBuf;

#[derive(Debug, Args)]
pub struct PullArgs {
    /// OCI reference, for example oci://ghcr.io/acme/ops:v1
    reference: String,
    /// Project receiving the toolbox
    #[arg(long, default_value = ".")]
    path: PathBuf,
    /// Allow an unencrypted HTTP Registry (development only)
    #[arg(long)]
    plain_http: bool,
    /// Registry username
    #[arg(long)]
    username: Option<String>,
    /// Prompt for a Registry password
    #[arg(long)]
    registry_password: bool,
}

#[derive(Debug, Args)]
pub struct PushArgs {
    /// OCI reference, for example oci://harbor.internal/acme/ops:v1
    reference: String,
    /// Project containing the built toolbox
    #[arg(long, default_value = ".")]
    path: PathBuf,
    /// Allow an unencrypted HTTP Registry (development only)
    #[arg(long)]
    plain_http: bool,
    /// Registry username
    #[arg(long)]
    username: Option<String>,
    /// Prompt for a Registry password
    #[arg(long)]
    registry_password: bool,
}

pub fn pull(args: PullArgs) -> io::Result<u8> {
    let root = args.path.canonicalize()?;
    let reference = binport::registry::Reference::parse(&args.reference)?;
    let credentials = credentials(args.username, args.registry_password)?;
    let staging = root.join(format!(".binport-pull-{}.oci", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    let result = (|| {
        let report = binport::registry::pull_layout(
            &reference,
            &staging,
            &toolbox::cache_root()?,
            args.plain_http,
            credentials,
        )?;
        let lock = binport::oci::unpack(&staging, &root)?;
        println!(
            "Pulled {} artifacts from {}\nDigest: {}\nBlobs: {} downloaded, {} cached",
            lock.tools.len(),
            args.reference,
            report.digest,
            report.downloaded,
            report.cached
        );
        Ok(0)
    })();
    let _ = fs::remove_dir_all(staging);
    result
}

pub fn push(args: PushArgs) -> io::Result<u8> {
    let root = args.path.canonicalize()?;
    let reference = binport::registry::Reference::parse(&args.reference)?;
    let credentials = credentials(args.username, args.registry_password)?;
    let staging = root.join(format!(".binport-push-{}.oci", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    let result = (|| {
        binport::oci::pack(&root, &staging)?;
        let report =
            binport::registry::push_layout(&reference, &staging, args.plain_http, credentials)?;
        println!(
            "Pushed {}\nDigest: {}\nBlobs: {} uploaded, {} already present",
            args.reference, report.digest, report.uploaded, report.existing
        );
        Ok(0)
    })();
    let _ = fs::remove_dir_all(staging);
    result
}

fn credentials(
    username: Option<String>,
    prompt_password: bool,
) -> io::Result<Option<binport::registry::Credentials>> {
    match (username, prompt_password) {
        (None, false) => Ok(None),
        (Some(username), true) => Ok(Some(binport::registry::Credentials {
            username,
            password: rpassword::prompt_password("Registry password: ")?,
        })),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--username and --registry-password must be used together",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::credentials;

    #[test]
    fn registry_credentials_are_an_explicit_pair() {
        assert!(credentials(None, false).unwrap().is_none());
        assert!(credentials(Some("user".into()), false).is_err());
        assert!(credentials(None, true).is_err());
    }
}
