use super::runtime::{ad_hoc_bastion, ad_hoc_route};
use binport::ssh::{Destination, NativeSsh};
use clap::Args;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;

#[derive(Debug, Args)]
pub struct TunnelArgs {
    /// Tunnel specifications in the form LOCAL_PORT:REMOTE_HOST:REMOTE_PORT
    #[arg(required = true)]
    tunnels: Vec<String>,

    /// SSH host alias or user@host destination
    host: String,
}

#[derive(Debug)]
struct TunnelSpec {
    local_port: u16,
    remote_host: String,
    remote_port: u16,
}

pub fn run(args: TunnelArgs, use_password: bool) -> io::Result<u8> {
    let specs = parse_tunnel_specs(&args.tunnels)?;
    let password = use_password
        .then(|| rpassword::prompt_password("SSH password: "))
        .transpose()?;

    let runtime = tokio::runtime::Runtime::new().map_err(io::Error::other)?;
    runtime.block_on(run_tunnels(&args.host, specs, password.as_deref()))?;

    Ok(0)
}

fn parse_tunnel_specs(specs: &[String]) -> io::Result<Vec<TunnelSpec>> {
    specs
        .iter()
        .map(|spec| {
            let parts: Vec<&str> = spec.split(':').collect();
            if parts.len() != 3 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "invalid tunnel spec {spec:?}; expected LOCAL_PORT:REMOTE_HOST:REMOTE_PORT"
                    ),
                ));
            }
            let local_port = parts[0].parse::<u16>().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid local port in {spec:?}"),
                )
            })?;
            if local_port == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("local port must be greater than 0 in {spec:?}"),
                ));
            }
            let remote_host = parts[1].to_owned();
            if remote_host.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("remote host cannot be empty in {spec:?}"),
                ));
            }
            let remote_port = parts[2].parse::<u16>().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid remote port in {spec:?}"),
                )
            })?;
            if remote_port == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("remote port must be greater than 0 in {spec:?}"),
                ));
            }
            Ok(TunnelSpec {
                local_port,
                remote_host,
                remote_port,
            })
        })
        .collect()
}

async fn run_tunnels(host: &str, specs: Vec<TunnelSpec>, password: Option<&str>) -> io::Result<()> {
    let ssh = connect_host(host, password).await?;
    let shared_ssh = Arc::new(RwLock::new(ssh));

    let mut handles = Vec::new();

    for spec in specs {
        let listener = TcpListener::bind(("127.0.0.1", spec.local_port)).await?;
        let actual_port = listener.local_addr()?.port();
        println!(
            "Tunneling 127.0.0.1:{} -> {}:{}",
            actual_port, spec.remote_host, spec.remote_port
        );

        let remote_host = spec.remote_host.clone();
        let remote_port = spec.remote_port;
        let ssh = Arc::clone(&shared_ssh);

        let handle = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, addr)) => {
                        let host = remote_host.clone();
                        let ssh = Arc::clone(&ssh);
                        tokio::spawn(async move {
                            if let Err(e) =
                                handle_connection(stream, addr, &host, remote_port, ssh).await
                            {
                                eprintln!("Connection error: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        eprintln!("Accept error: {e}");
                        // Transient errors (EINTR, EMFILE) should not kill the tunnel.
                        // Only break on fatal errors (EINVAL means the listener is closed).
                        if e.kind() == io::ErrorKind::InvalidInput {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                }
            }
        });

        handles.push(handle);
    }

    // Wait for all tunnel tasks (they run forever until Ctrl+C)
    for handle in handles {
        let _ = handle.await;
    }

    Ok(())
}

async fn handle_connection(
    local_stream: TcpStream,
    local_addr: SocketAddr,
    remote_host: &str,
    remote_port: u16,
    shared_ssh: Arc<RwLock<NativeSsh>>,
) -> io::Result<()> {
    eprintln!(
        "[tunnel] Connection from {} -> {}:{}",
        local_addr, remote_host, remote_port
    );

    let target = format!("{remote_host}:{remote_port}");
    let ssh = shared_ssh.read().await.clone();
    let channel = match ssh
        .client()
        .open_direct_tcpip_channel(target.as_str(), None)
        .await
    {
        Ok(channel) => channel,
        Err(first_error) => {
            eprintln!("[tunnel] SSH channel failed; reconnecting: {first_error}");
            let reconnected = ssh.reconnect().await?;
            let channel = reconnected
                .client()
                .open_direct_tcpip_channel(target.as_str(), None)
                .await
                .map_err(|retry_error| {
                    tunnel_channel_error(remote_host, remote_port, retry_error)
                })?;
            *shared_ssh.write().await = reconnected;
            channel
        }
    };

    relay(local_stream, channel.into_stream()).await
}

fn tunnel_channel_error(
    remote_host: &str,
    remote_port: u16,
    error: impl std::fmt::Display,
) -> io::Error {
    io::Error::new(
        io::ErrorKind::ConnectionRefused,
        format!(
            "SSH gateway rejected or could not open direct-tcpip to {remote_host}:{remote_port}: {error}; enable TCP forwarding in the SSH/bastion policy (AllowTcpForwarding/PermitOpen), or use a direct host or ProxyJump"
        ),
    )
}

async fn relay<L, R>(mut local: L, mut remote: R) -> io::Result<()>
where
    L: AsyncRead + AsyncWrite + Unpin,
    R: AsyncRead + AsyncWrite + Unpin,
{
    tokio::io::copy_bidirectional(&mut local, &mut remote).await?;
    Ok(())
}

async fn connect_host(host: &str, password: Option<&str>) -> io::Result<NativeSsh> {
    if let Some((bastion_alias, target_alias)) = ad_hoc_bastion(host)? {
        let bastion_dest = Destination::resolve(bastion_alias)?;
        let mut target_dest = Destination::resolve(target_alias)?;
        apply_ad_hoc_bastion(&mut target_dest, &bastion_dest)?;
        return NativeSsh::connect(&target_dest, password).await;
    }
    if let Some((jump_host, target_host)) = ad_hoc_route(host)? {
        let jump = NativeSsh::connect_jump(jump_host, password).await?;
        let mut destination = Destination::resolve(target_host)?;
        destination.proxy_jump = Some(jump_host.to_owned());
        return NativeSsh::connect_with_jump(&destination, password, &jump).await;
    }
    NativeSsh::connect(&Destination::resolve(host)?, password).await
}

fn apply_ad_hoc_bastion(target: &mut Destination, bastion: &Destination) -> io::Result<()> {
    if target.proxy_jump.is_some() || target.bastion_proxy.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "target already has a proxy configured; ad-hoc bastion route is not applicable",
        ));
    }
    target.bastion_proxy = Some(binport::ssh::BastionProxy {
        host: bastion.hostname.clone(),
        port: bastion.port,
        user: bastion.user.clone(),
        account: target.user.clone(),
        preset: None,
        format: "{user}/{host}/{account}".into(),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn rejects_port_zero_for_local() {
        let err = parse_tunnel_specs(&["0:10.0.0.1:8080".into()]).unwrap_err();
        assert!(
            err.to_string().contains("greater than 0"),
            "expected port validation error, got: {err}"
        );
    }

    #[test]
    fn rejects_port_zero_for_remote() {
        let err = parse_tunnel_specs(&["8080:10.0.0.1:0".into()]).unwrap_err();
        assert!(
            err.to_string().contains("greater than 0"),
            "expected port validation error, got: {err}"
        );
    }

    #[test]
    fn accepts_valid_spec_with_hostname() {
        let specs = parse_tunnel_specs(&["8080:localhost:3000".into()]).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].local_port, 8080);
        assert_eq!(specs[0].remote_host, "localhost");
        assert_eq!(specs[0].remote_port, 3000);
    }

    #[test]
    fn accepts_valid_spec_with_ip() {
        let specs = parse_tunnel_specs(&["9090:10.0.0.5:5432".into()]).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].remote_host, "10.0.0.5");
    }

    #[test]
    fn rejects_empty_remote_host() {
        let err = parse_tunnel_specs(&["8080::3000".into()]).unwrap_err();
        assert!(
            err.to_string().contains("remote host cannot be empty"),
            "expected empty host error, got: {err}"
        );
    }

    #[test]
    fn rejects_malformed_spec() {
        assert!(parse_tunnel_specs(&["not-a-port:host:80".into()]).is_err());
        assert!(parse_tunnel_specs(&["80:host".into()]).is_err());
        assert!(parse_tunnel_specs(&["80:host:not-a-port".into()]).is_err());
    }

    #[test]
    fn parses_multiple_specs() {
        let specs =
            parse_tunnel_specs(&["8080:localhost:3000".into(), "9090:10.0.0.1:5432".into()])
                .unwrap();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].local_port, 8080);
        assert_eq!(specs[1].local_port, 9090);
    }

    #[test]
    fn rejects_both_ports_zero() {
        let err = parse_tunnel_specs(&["0:10.0.0.1:0".into()]).unwrap_err();
        // Should fail on local port first
        assert!(
            err.to_string()
                .contains("local port must be greater than 0"),
            "expected local port validation error, got: {err}"
        );
    }

    #[test]
    fn accepts_high_port_numbers() {
        let specs = parse_tunnel_specs(&["65535:localhost:65534".into()]).unwrap();
        assert_eq!(specs[0].local_port, 65535);
        assert_eq!(specs[0].remote_port, 65534);
    }

    #[test]
    fn rejects_ipv6_host_with_brackets() {
        // IPv6 addresses with brackets are not supported because the parser splits on ':'
        let err = parse_tunnel_specs(&["8080:[::1]:3000".into()]).unwrap_err();
        assert!(
            err.to_string().contains("invalid tunnel spec"),
            "expected parsing error for IPv6, got: {err}"
        );
    }

    #[test]
    fn accepts_host_with_dots_and_dashes() {
        let specs = parse_tunnel_specs(&["8080:my-server.example.com:3000".into()]).unwrap();
        assert_eq!(specs[0].remote_host, "my-server.example.com");
    }

    #[test]
    fn explains_direct_tcpip_policy_failures() {
        let error = tunnel_channel_error("database.internal", 5432, "administratively prohibited");
        assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
        assert!(error.to_string().contains("AllowTcpForwarding/PermitOpen"));
    }

    #[tokio::test]
    async fn relay_preserves_response_after_client_half_close() {
        let (mut client, relay_local) = tokio::io::duplex(1024);
        let (relay_remote, mut server) = tokio::io::duplex(1024);

        let relay_task = tokio::spawn(relay(relay_local, relay_remote));
        let server_task = tokio::spawn(async move {
            let mut request = Vec::new();
            server.read_to_end(&mut request).await.unwrap();
            assert_eq!(request, b"test data");
            server.write_all(b"response after EOF").await.unwrap();
            server.shutdown().await.unwrap();
        });

        client.write_all(b"test data").await.unwrap();
        client.shutdown().await.unwrap();

        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        assert_eq!(response, b"response after EOF");

        server_task.await.unwrap();
        relay_task.await.unwrap().unwrap();
    }
}
