use binport::ssh::{Destination, NativeSsh};
use clap::{Args, Subcommand};
use std::io::{self, IsTerminal, Write};

#[derive(Debug, Args)]
pub struct AuthArgs {
    #[command(subcommand)]
    command: AuthCommand,
}

#[derive(Debug, Subcommand)]
enum AuthCommand {
    /// Generate a dedicated key and install it on a host
    Setup(AuthHostArgs),
    /// Verify the dedicated key for a host
    Status(AuthHostArgs),
    /// Remove the dedicated key locally and remotely
    Remove(AuthRemoveArgs),
}

#[derive(Debug, Args)]
struct AuthHostArgs {
    /// Exact SSH config alias or user@host destination
    host: String,
}

#[derive(Debug, Args)]
struct AuthRemoveArgs {
    /// Exact SSH config alias or user@host destination
    host: String,
    /// Skip the interactive confirmation
    #[arg(long)]
    yes: bool,
}

pub fn run(args: AuthArgs, json: bool) -> io::Result<u8> {
    match args.command {
        AuthCommand::Setup(args) => setup(&args.host, json),
        AuthCommand::Status(args) => status(&args.host, json),
        AuthCommand::Remove(args) => remove(&args.host, args.yes, json),
    }
}

fn setup(host: &str, json: bool) -> io::Result<u8> {
    let mut destination = Destination::resolve(host)?;
    let key = binport::auth::ensure_managed_key(host)?;
    let runtime = tokio::runtime::Runtime::new().map_err(io::Error::other)?;
    if !key.created {
        destination.identity = Some(key.private_path.clone());
        if managed_key_works(&runtime, &destination) {
            return print_ready(host, &key, "existing", json);
        }
    }
    let password = rpassword::prompt_password("SSH password: ")?;
    let remote_state = runtime.block_on(async {
        let ssh = NativeSsh::connect(&destination, Some(&password)).await?;
        let (status, stdout, stderr) = ssh
            .execute_capture_with_input(
                binport::auth::install_key_command(),
                key.public_key.as_bytes().to_vec(),
            )
            .await?;
        if status != 0 {
            return Err(io::Error::other(format!(
                "remote key installation failed: {}",
                String::from_utf8_lossy(&stderr).trim()
            )));
        }
        destination.identity = Some(key.private_path.clone());
        let verification = NativeSsh::connect(&destination, None).await?;
        let (status, _, stderr) = verification.execute_capture("true").await?;
        if status != 0 {
            return Err(io::Error::other(format!(
                "key verification failed: {}",
                stderr.trim()
            )));
        }
        Ok::<_, io::Error>(String::from_utf8_lossy(&stdout).trim().to_owned())
    })?;
    print_ready(host, &key, &remote_state, json)
}

fn print_ready(
    host: &str,
    key: &binport::auth::ManagedKey,
    remote_state: &str,
    json: bool,
) -> io::Result<u8> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "host": host,
                "ready": true,
                "created": key.created,
                "remote": remote_state,
                "identity_file": key.private_path,
            }))
            .map_err(io::Error::other)?
        );
    } else {
        println!("Passwordless authentication is ready for {host}");
        println!("Identity: {}", key.private_path.display());
        println!();
        println!("  binport {host} rg --version");
    }
    Ok(0)
}

fn managed_key_works(runtime: &tokio::runtime::Runtime, destination: &Destination) -> bool {
    runtime
        .block_on(async {
            let ssh = NativeSsh::connect(destination, None).await?;
            let (status, _, _) = ssh.execute_capture("true").await?;
            Ok::<_, io::Error>(status == 0)
        })
        .unwrap_or(false)
}

fn status(host: &str, json: bool) -> io::Result<u8> {
    let (private_path, _) = binport::auth::read_managed_public_key(host)?;
    let mut destination = Destination::resolve(host)?;
    destination.identity = Some(private_path.clone());
    let runtime = tokio::runtime::Runtime::new().map_err(io::Error::other)?;
    let ready = managed_key_works(&runtime, &destination);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "host": host,
                "ready": ready,
                "identity_file": private_path,
            }))
            .map_err(io::Error::other)?
        );
    } else if ready {
        println!("{host}: ready ({})", private_path.display());
    } else {
        println!("{host}: local key exists but remote authentication failed");
    }
    Ok(if ready { 0 } else { 1 })
}

fn remove(host: &str, yes: bool, json: bool) -> io::Result<u8> {
    let (private_path, public_key) = binport::auth::read_managed_public_key(host)?;
    if !yes && !confirm_removal(host)? {
        println!("Cancelled");
        return Ok(0);
    }
    let mut destination = Destination::resolve(host)?;
    destination.identity = Some(private_path);
    let runtime = tokio::runtime::Runtime::new().map_err(io::Error::other)?;
    runtime.block_on(async {
        let ssh = NativeSsh::connect(&destination, None).await?;
        let (status, _, stderr) = ssh
            .execute_capture_with_input(
                binport::auth::remove_key_command(),
                public_key.as_bytes().to_vec(),
            )
            .await?;
        if status != 0 {
            return Err(io::Error::other(format!(
                "remote key removal failed: {}",
                String::from_utf8_lossy(&stderr).trim()
            )));
        }
        Ok::<_, io::Error>(())
    })?;
    binport::auth::remove_managed_key(host)?;
    if json {
        println!(
            "{{\"host\":{},\"removed\":true}}",
            serde_json::to_string(host).unwrap()
        );
    } else {
        println!("Removed passwordless authentication for {host}");
    }
    Ok(0)
}

fn confirm_removal(host: &str) -> io::Result<bool> {
    if !io::stdin().is_terminal() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "auth remove requires --yes when stdin is not interactive",
        ));
    }
    print!("Remove binport authentication for {host}? [y/N] ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}
