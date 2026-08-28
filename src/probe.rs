use crate::catalog::Platform;
use crate::ssh::{Destination, NativeSsh};
use std::io;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapabilityState {
    Supported,
    Denied,
    Failed,
    Timeout,
    NotChecked,
}

impl CapabilityState {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Denied => "denied",
            Self::Failed => "failed",
            Self::Timeout => "timeout",
            Self::NotChecked => "not-checked",
        }
    }

    pub fn symbol(&self) -> &'static str {
        match self {
            Self::Supported => "✓",
            Self::Denied | Self::Failed => "✗",
            Self::Timeout | Self::NotChecked => "!",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Capability {
    pub state: CapabilityState,
    pub detail: Option<String>,
    pub elapsed_ms: Option<u128>,
}

impl Capability {
    fn supported(elapsed: Duration) -> Self {
        Self {
            state: CapabilityState::Supported,
            detail: None,
            elapsed_ms: Some(elapsed.as_millis()),
        }
    }

    fn result<T>(
        result: Result<io::Result<T>, tokio::time::error::Elapsed>,
        elapsed: Duration,
    ) -> Self {
        match result {
            Ok(Ok(_)) => Self::supported(elapsed),
            Ok(Err(error)) => Self {
                state: CapabilityState::Failed,
                detail: Some(error.to_string()),
                elapsed_ms: Some(elapsed.as_millis()),
            },
            Err(_) => Self {
                state: CapabilityState::Timeout,
                detail: Some("probe timed out".to_owned()),
                elapsed_ms: Some(elapsed.as_millis()),
            },
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProbeReport {
    pub route: String,
    pub destination: String,
    pub platform: Option<String>,
    pub connect: Capability,
    pub exec: Capability,
    pub file_stream: Capability,
    pub direct_tcpip: Capability,
}

#[derive(Clone, Debug)]
pub struct JumpProbeReport {
    pub entry: Capability,
    pub direct_tcpip: Capability,
    pub target: Option<ProbeReport>,
    pub target_detail: Option<String>,
}

impl ProbeReport {
    pub fn command_ready(&self) -> bool {
        self.exec.state == CapabilityState::Supported
    }

    pub fn file_ready(&self) -> bool {
        self.file_stream.state == CapabilityState::Supported
    }
}

pub async fn probe_destination(
    destination: &Destination,
    password: Option<&str>,
    check_forwarding: bool,
) -> io::Result<ProbeReport> {
    let connect_started = Instant::now();
    let ssh = tokio::time::timeout(
        Duration::from_secs(10),
        NativeSsh::connect(destination, password),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "SSH connection timed out"))??;
    let connect = Capability::supported(connect_started.elapsed());
    probe_connected(destination, ssh, connect, check_forwarding).await
}

pub async fn probe_jump_route(
    jump_destination: &Destination,
    target_destination: &Destination,
    jump_password: Option<&str>,
    target_password: Option<&str>,
) -> JumpProbeReport {
    let entry_started = Instant::now();
    let jump = match tokio::time::timeout(
        Duration::from_secs(10),
        NativeSsh::connect_jump_destination(jump_destination, jump_password),
    )
    .await
    {
        Ok(Ok(jump)) => jump,
        result => {
            return JumpProbeReport {
                entry: Capability::result(
                    result.map(|value| value.map(|_| ())),
                    entry_started.elapsed(),
                ),
                direct_tcpip: Capability {
                    state: CapabilityState::NotChecked,
                    detail: None,
                    elapsed_ms: None,
                },
                target: None,
                target_detail: Some("entry host connection failed".to_owned()),
            };
        }
    };
    let entry = Capability::supported(entry_started.elapsed());

    let forwarding_started = Instant::now();
    let forwarding_result = tokio::time::timeout(
        Duration::from_secs(5),
        jump.probe_direct_tcpip(&target_destination.hostname, target_destination.port),
    )
    .await;
    let direct_tcpip = match forwarding_result {
        Ok(Ok(())) => Capability::supported(forwarding_started.elapsed()),
        Ok(Err(error)) => Capability {
            state: CapabilityState::Denied,
            detail: Some(error.to_string()),
            elapsed_ms: Some(forwarding_started.elapsed().as_millis()),
        },
        Err(_) => Capability {
            state: CapabilityState::Timeout,
            detail: Some("jump forwarding probe timed out".to_owned()),
            elapsed_ms: Some(forwarding_started.elapsed().as_millis()),
        },
    };
    if direct_tcpip.state != CapabilityState::Supported {
        return JumpProbeReport {
            entry,
            direct_tcpip,
            target: None,
            target_detail: Some(
                "the jump host cannot open a native SSH channel to the target".to_owned(),
            ),
        };
    }

    let target_started = Instant::now();
    match tokio::time::timeout(
        Duration::from_secs(10),
        NativeSsh::connect_with_jump(target_destination, target_password, &jump),
    )
    .await
    {
        Ok(Ok(ssh)) => {
            let connect = Capability::supported(target_started.elapsed());
            match probe_connected(target_destination, ssh, connect, false).await {
                Ok(target) => JumpProbeReport {
                    entry,
                    direct_tcpip,
                    target: Some(target),
                    target_detail: None,
                },
                Err(error) => JumpProbeReport {
                    entry,
                    direct_tcpip,
                    target: None,
                    target_detail: Some(error.to_string()),
                },
            }
        }
        Ok(Err(error)) => JumpProbeReport {
            entry,
            direct_tcpip,
            target: None,
            target_detail: Some(format!(
                "target authentication failed through the jump host: {error}"
            )),
        },
        Err(_) => JumpProbeReport {
            entry,
            direct_tcpip,
            target: None,
            target_detail: Some("target authentication timed out".to_owned()),
        },
    }
}

async fn probe_connected(
    destination: &Destination,
    ssh: NativeSsh,
    connect: Capability,
    check_forwarding: bool,
) -> io::Result<ProbeReport> {
    let exec_started = Instant::now();
    let exec_result = tokio::time::timeout(
        Duration::from_secs(8),
        ssh.execute_capture("printf 'BINPORT_PROBE_OK\\n'; uname -s; uname -m"),
    )
    .await;
    let (exec, platform) = match exec_result {
        Ok(Ok((0, stdout, _))) => {
            let mut lines = stdout.lines();
            let marker = lines.next().unwrap_or_default();
            let os = lines.next().unwrap_or_default();
            let arch = lines.next().unwrap_or_default();
            if marker == "BINPORT_PROBE_OK" {
                (
                    Capability::supported(exec_started.elapsed()),
                    Platform::from_uname(os, arch).map(|value| value.name().to_owned()),
                )
            } else {
                (
                    Capability {
                        state: CapabilityState::Failed,
                        detail: Some("remote shell did not return the readiness marker".to_owned()),
                        elapsed_ms: Some(exec_started.elapsed().as_millis()),
                    },
                    None,
                )
            }
        }
        Ok(Ok((status, _, stderr))) => (
            Capability {
                state: CapabilityState::Failed,
                detail: Some(format!("exit {status}: {}", stderr.trim())),
                elapsed_ms: Some(exec_started.elapsed().as_millis()),
            },
            None,
        ),
        other => (
            Capability::result(other.map(|value| value.map(|_| ())), exec_started.elapsed()),
            None,
        ),
    };

    let file_started = Instant::now();
    let file_marker = b"BINPORT_STREAM_PROBE_7f93".to_vec();
    let file_result = tokio::time::timeout(
        Duration::from_secs(8),
        ssh.execute_capture_with_input_fresh("cat", file_marker.clone()),
    )
    .await;
    let file_stream = match file_result {
        Ok(Ok((0, stdout, _))) if stdout == file_marker => {
            Capability::supported(file_started.elapsed())
        }
        Ok(Ok((status, _, stderr))) => Capability {
            state: CapabilityState::Failed,
            detail: Some(format!(
                "stream probe exited {status}: {}",
                String::from_utf8_lossy(&stderr).trim()
            )),
            elapsed_ms: Some(file_started.elapsed().as_millis()),
        },
        other => Capability::result(other.map(|value| value.map(|_| ())), file_started.elapsed()),
    };

    let direct_tcpip = if check_forwarding {
        let forwarding_started = Instant::now();
        let forwarding_ssh = if ssh.is_bastion() {
            ssh.reconnect().await?
        } else {
            ssh.clone()
        };
        let target = format!("127.0.0.1:{}", destination.port);
        match tokio::time::timeout(
            Duration::from_secs(5),
            forwarding_ssh
                .client()
                .open_direct_tcpip_channel(target.as_str(), None),
        )
        .await
        {
            Ok(Ok(_)) => Capability::supported(forwarding_started.elapsed()),
            Ok(Err(error)) => Capability {
                state: CapabilityState::Denied,
                detail: Some(error.to_string()),
                elapsed_ms: Some(forwarding_started.elapsed().as_millis()),
            },
            Err(_) => Capability {
                state: CapabilityState::Timeout,
                detail: Some("direct-tcpip probe timed out".to_owned()),
                elapsed_ms: Some(forwarding_started.elapsed().as_millis()),
            },
        }
    } else {
        Capability {
            state: CapabilityState::NotChecked,
            detail: None,
            elapsed_ms: None,
        }
    };

    let route = if let Some(jump) = &destination.proxy_jump {
        format!("{jump} -> {}", destination.hostname)
    } else if let Some(bastion) = &destination.bastion_proxy {
        format!("bastion:{} -> {}", bastion.host, destination.hostname)
    } else {
        "direct".to_owned()
    };
    Ok(ProbeReport {
        route,
        destination: format!(
            "{}@{}:{}",
            destination.user, destination.hostname, destination.port
        ),
        platform,
        connect,
        exec,
        file_stream,
        direct_tcpip,
    })
}
