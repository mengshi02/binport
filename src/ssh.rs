use crate::progress::TransferProgress;
use async_ssh2_tokio::client::{AuthMethod, Client, ServerCheckMethod};
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

#[derive(Debug)]
pub struct StreamChunk {
    pub stderr: bool,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Destination {
    pub hostname: String,
    pub port: u16,
    pub user: String,
    pub identity: Option<PathBuf>,
    pub proxy_jump: Option<String>,
}

pub struct NativeSsh {
    client: Client,
    _jump: Option<Client>,
}

#[derive(Clone)]
pub struct SharedJump {
    client: Client,
}

impl Destination {
    pub fn resolve(value: &str) -> io::Result<Self> {
        let (requested_user, alias) = value
            .split_once('@')
            .map_or((None, value), |(user, host)| (Some(user), host));
        let mut destination = Self {
            hostname: alias.to_owned(),
            port: 22,
            user: requested_user
                .map(str::to_owned)
                .or_else(|| env::var("USER").ok())
                .or_else(|| env::var("USERNAME").ok())
                .unwrap_or_else(|| "root".into()),
            identity: None,
            proxy_jump: None,
        };
        if let Some(home) = user_home() {
            let ssh_dir = home.join(".ssh");
            if let Ok(source) = fs::read_to_string(ssh_dir.join("config")) {
                apply_ssh_config(&source, alias, requested_user.is_none(), &mut destination);
            }
            if let Ok(source) = fs::read_to_string(ssh_dir.join(crate::host::MANAGED_CONFIG_NAME)) {
                apply_ssh_config(&source, alias, requested_user.is_none(), &mut destination);
            }
            if destination.identity.is_none() {
                let managed = crate::auth::managed_key_path(value)?;
                if managed.is_file() {
                    destination.identity = Some(managed);
                }
            }
            if destination.identity.is_none() {
                for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
                    let path = ssh_dir.join(name);
                    if path.is_file() {
                        destination.identity = Some(path);
                        break;
                    }
                }
            }
        }
        Ok(destination)
    }
}

pub fn select_hosts(group: &str) -> io::Result<Vec<String>> {
    let home = user_home().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "user home directory is unavailable",
        )
    })?;
    let ssh_dir = home.join(".ssh");
    let mut source = fs::read_to_string(ssh_dir.join("config"))?;
    if let Ok(managed) = fs::read_to_string(ssh_dir.join(crate::host::MANAGED_CONFIG_NAME)) {
        source.push('\n');
        source.push_str(&managed);
    }
    Ok(select_hosts_from_config(&source, group))
}

fn select_hosts_from_config(source: &str, group: &str) -> Vec<String> {
    let prefix = group
        .strip_suffix('*')
        .unwrap_or(group)
        .trim_end_matches('-');
    let mut hosts = Vec::new();
    for raw in source.lines() {
        let line = raw.split('#').next().unwrap_or_default().trim();
        let Some((key, value)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        if !key.eq_ignore_ascii_case("host") {
            continue;
        }
        for alias in value.split_whitespace() {
            if alias.contains(['*', '?', '!']) {
                continue;
            }
            let matches = group == "all"
                || alias == group
                || alias
                    .strip_prefix(prefix)
                    .is_some_and(|suffix| suffix.starts_with('-'));
            if matches && !hosts.iter().any(|host| host == alias) {
                hosts.push(alias.to_owned());
            }
        }
    }
    hosts.sort();
    hosts
}

impl NativeSsh {
    pub async fn connect_jump(alias: &str, password: Option<&str>) -> io::Result<SharedJump> {
        let destination = Destination::resolve(alias)?;
        if destination.proxy_jump.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "nested ProxyJump chains are not supported yet",
            ));
        }
        Ok(SharedJump {
            client: connect_direct(&destination, password).await?,
        })
    }

    pub async fn connect_with_jump(
        destination: &Destination,
        password: Option<&str>,
        jump: &SharedJump,
    ) -> io::Result<Self> {
        let client = Client::connect_via(
            &jump.client,
            (destination.hostname.as_str(), destination.port),
            &destination.user,
            auth_method(destination, password)?,
            ServerCheckMethod::DefaultKnownHostsFile,
        )
        .await
        .map_err(io::Error::other)?;
        Ok(Self {
            client,
            _jump: Some(jump.client.clone()),
        })
    }

    pub async fn connect(destination: &Destination, password: Option<&str>) -> io::Result<Self> {
        if let Some(proxy) = &destination.proxy_jump {
            // `password` belongs to the target. The jump host uses its own key or agent.
            let jump = Self::connect_jump(proxy, None).await?;
            return Self::connect_with_jump(destination, password, &jump).await;
        }

        Ok(Self {
            client: connect_direct(destination, password).await?,
            _jump: None,
        })
    }

    pub fn uses_proxy_jump(&self) -> bool {
        self._jump.is_some()
    }

    pub async fn execute(&self, command: &str) -> io::Result<u32> {
        let (status, stdout, stderr) = self.execute_capture(command).await?;
        print!("{stdout}");
        eprint!("{stderr}");
        io::stdout().flush()?;
        io::stderr().flush()?;
        Ok(status)
    }

    pub async fn execute_capture(&self, command: &str) -> io::Result<(u32, String, String)> {
        let result = self
            .client
            .execute(command)
            .await
            .map_err(io::Error::other)?;
        Ok((result.exit_status, result.stdout, result.stderr))
    }

    pub async fn execute_stream(
        &self,
        command: &str,
        output: mpsc::UnboundedSender<StreamChunk>,
    ) -> io::Result<u32> {
        let (stdout_tx, mut stdout_rx) = mpsc::channel(8);
        let (stderr_tx, mut stderr_rx) = mpsc::channel(8);
        let execution =
            self.client
                .execute_io(command, stdout_tx, Some(stderr_tx), None, false, None);
        tokio::pin!(execution);
        loop {
            tokio::select! {
                result = &mut execution => {
                    let status = result.map_err(io::Error::other)?;
                    while let Ok(data) = stdout_rx.try_recv() {
                        let _ = output.send(StreamChunk { stderr: false, data });
                    }
                    while let Ok(data) = stderr_rx.try_recv() {
                        let _ = output.send(StreamChunk { stderr: true, data });
                    }
                    return Ok(status);
                },
                Some(data) = stdout_rx.recv() => {
                    let _ = output.send(StreamChunk { stderr: false, data });
                },
                Some(data) = stderr_rx.recv() => {
                    let _ = output.send(StreamChunk { stderr: true, data });
                },
            }
        }
    }

    pub async fn execute_with_input(&self, command: &str, input: Vec<u8>) -> io::Result<u32> {
        let (status, stdout, stderr) = self.execute_capture_with_input(command, input).await?;
        io::stdout().write_all(&stdout)?;
        io::stderr().write_all(&stderr)?;
        io::stdout().flush()?;
        io::stderr().flush()?;
        Ok(status)
    }

    pub async fn execute_tty(&self, command: &str, eof_on_quit: bool) -> io::Result<u32> {
        if !std::io::IsTerminal::is_terminal(&io::stdin())
            || !std::io::IsTerminal::is_terminal(&io::stdout())
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "TTY mode requires an interactive terminal",
            ));
        }
        crossterm::terminal::enable_raw_mode().map_err(io::Error::other)?;
        let _raw_mode = RawModeGuard;
        let (output_tx, mut output_rx) = mpsc::channel(32);
        let (input_tx, input_rx) = mpsc::channel(8);
        let input_task = tokio::spawn(async move {
            let mut stdin = tokio::io::stdin();
            let mut buffer = vec![0_u8; 4096];
            loop {
                let read = stdin.read(&mut buffer).await?;
                if read == 0 {
                    break;
                }
                let input = buffer[..read].to_vec();
                let quits = matches!(input.as_slice(), [3] | [4] | [17])
                    || (eof_on_quit && matches!(input.as_slice(), [b'q'] | [b'Q']));
                if input_tx.send(input).await.is_err() {
                    break;
                }
                if quits {
                    let _ = input_tx.send(Vec::new()).await;
                    break;
                }
            }
            Ok::<_, io::Error>(())
        });
        let execution =
            self.client
                .execute_io(command, output_tx, None, Some(input_rx), true, None);
        tokio::pin!(execution);
        let mut stdout = io::stdout();
        let result = loop {
            tokio::select! {
                result = &mut execution => break result.map_err(io::Error::other),
                Some(data) = output_rx.recv() => {
                    stdout.write_all(&data)?;
                    stdout.flush()?;
                }
            }
        };
        input_task.abort();
        while let Ok(data) = output_rx.try_recv() {
            stdout.write_all(&data)?;
        }
        stdout.flush()?;
        result
    }

    pub async fn execute_capture_with_input(
        &self,
        command: &str,
        input: Vec<u8>,
    ) -> io::Result<(u32, Vec<u8>, Vec<u8>)> {
        let (stdout_tx, mut stdout_rx) = mpsc::channel(8);
        let (stderr_tx, mut stderr_rx) = mpsc::channel(8);
        let (stdin_tx, stdin_rx) = mpsc::channel(2);
        stdin_tx.send(input).await.map_err(io::Error::other)?;
        stdin_tx.send(Vec::new()).await.map_err(io::Error::other)?;
        drop(stdin_tx);
        let execution = self.client.execute_io(
            command,
            stdout_tx,
            Some(stderr_tx),
            Some(stdin_rx),
            false,
            None,
        );
        tokio::pin!(execution);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        loop {
            tokio::select! {
                result = &mut execution => {
                    let status = result.map_err(io::Error::other)?;
                    while let Ok(data) = stdout_rx.try_recv() { stdout.extend_from_slice(&data); }
                    while let Ok(data) = stderr_rx.try_recv() { stderr.extend_from_slice(&data); }
                    return Ok((status, stdout, stderr));
                },
                Some(data) = stdout_rx.recv() => stdout.extend_from_slice(&data),
                Some(data) = stderr_rx.recv() => stderr.extend_from_slice(&data),
            }
        }
    }

    pub async fn upload_file(
        &self,
        command: &str,
        path: &Path,
        progress: TransferProgress,
    ) -> io::Result<(u32, Vec<u8>)> {
        let (stdout_tx, mut stdout_rx) = mpsc::channel(8);
        let (stderr_tx, mut stderr_rx) = mpsc::channel(8);
        let (stdin_tx, stdin_rx) = mpsc::channel(4);
        let path = path.to_owned();
        let feeder_progress = progress.clone();
        let feeder = tokio::spawn(async move {
            let result = async {
                let mut file = tokio::fs::File::open(path).await?;
                let mut buffer = vec![0_u8; 64 * 1024];
                loop {
                    let read = file.read(&mut buffer).await?;
                    if read == 0 {
                        break;
                    }
                    stdin_tx
                        .send(buffer[..read].to_vec())
                        .await
                        .map_err(io::Error::other)?;
                    feeder_progress.inc(read);
                }
                Ok::<(), io::Error>(())
            }
            .await;
            let _ = stdin_tx.send(Vec::new()).await;
            result
        });
        let execution = self.client.execute_io(
            command,
            stdout_tx,
            Some(stderr_tx),
            Some(stdin_rx),
            false,
            None,
        );
        tokio::pin!(execution);
        let mut stderr = Vec::new();
        let status = loop {
            tokio::select! {
                result = &mut execution => break result.map_err(io::Error::other)?,
                Some(_) = stdout_rx.recv() => {},
                Some(data) = stderr_rx.recv() => stderr.extend_from_slice(&data),
            }
        };
        feeder.await.map_err(io::Error::other)??;
        while let Ok(data) = stderr_rx.try_recv() {
            stderr.extend_from_slice(&data);
        }
        progress.finish();
        Ok((status, stderr))
    }

    pub async fn download_file(
        &self,
        command: &str,
        path: &Path,
        progress: TransferProgress,
    ) -> io::Result<(u32, Vec<u8>)> {
        let (stdout_tx, mut stdout_rx) = mpsc::channel(8);
        let (stderr_tx, mut stderr_rx) = mpsc::channel(8);
        let execution =
            self.client
                .execute_io(command, stdout_tx, Some(stderr_tx), None, false, None);
        tokio::pin!(execution);
        let mut file = tokio::fs::File::create(path).await?;
        let mut stderr = Vec::new();
        let status = loop {
            tokio::select! {
                result = &mut execution => break result.map_err(io::Error::other)?,
                Some(data) = stdout_rx.recv() => {
                    file.write_all(&data).await?;
                    progress.inc(data.len());
                },
                Some(data) = stderr_rx.recv() => stderr.extend_from_slice(&data),
            }
        };
        while let Ok(data) = stdout_rx.try_recv() {
            file.write_all(&data).await?;
            progress.inc(data.len());
        }
        while let Ok(data) = stderr_rx.try_recv() {
            stderr.extend_from_slice(&data);
        }
        file.flush().await?;
        progress.finish();
        Ok((status, stderr))
    }
}

struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

async fn connect_direct(destination: &Destination, password: Option<&str>) -> io::Result<Client> {
    Client::connect(
        (destination.hostname.as_str(), destination.port),
        &destination.user,
        auth_method(destination, password)?,
        ServerCheckMethod::DefaultKnownHostsFile,
    )
    .await
    .map_err(io::Error::other)
}

fn auth_method(destination: &Destination, password: Option<&str>) -> io::Result<AuthMethod> {
    if let Some(password) = password {
        return Ok(AuthMethod::with_password(password));
    }
    #[cfg(unix)]
    let auth = if let Some(identity) = &destination.identity {
        AuthMethod::with_key_file(identity, None)
    } else if env::var_os("SSH_AUTH_SOCK").is_some() {
        AuthMethod::with_agent()
    } else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no SSH agent or private key found",
        ));
    };
    #[cfg(windows)]
    let auth = destination
        .identity
        .as_ref()
        .map(|path| AuthMethod::with_key_file(path, None))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no private key found"))?;
    Ok(auth)
}

fn apply_ssh_config(source: &str, alias: &str, allow_user: bool, destination: &mut Destination) {
    let mut active = false;
    let mut hostname_set = false;
    let mut user_set = !allow_user;
    let mut port_set = false;
    let mut identity_set = false;
    let mut proxy_jump_set = false;
    for raw in source.lines() {
        let line = raw.split('#').next().unwrap_or_default().trim();
        let Some((key, value)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let value = value.trim();
        if key.eq_ignore_ascii_case("host") {
            active = value
                .split_whitespace()
                .any(|pattern| pattern == alias || pattern == "*");
        } else if active {
            if key.eq_ignore_ascii_case("hostname") && !hostname_set {
                destination.hostname = value.into();
                hostname_set = true;
            } else if key.eq_ignore_ascii_case("user") && !user_set {
                destination.user = value.into();
                user_set = true;
            } else if key.eq_ignore_ascii_case("port") && !port_set {
                if let Ok(port) = value.parse() {
                    destination.port = port;
                    port_set = true;
                }
            } else if key.eq_ignore_ascii_case("identityfile") && !identity_set {
                destination.identity = Some(expand_home(value));
                identity_set = true;
            } else if key.eq_ignore_ascii_case("proxyjump") && !proxy_jump_set {
                if value != "none" {
                    destination.proxy_jump = Some(value.into());
                }
                proxy_jump_set = true;
            }
        }
    }
}

fn expand_home(value: &str) -> PathBuf {
    value
        .strip_prefix("~/")
        .and_then(|suffix| user_home().map(|home| home.join(suffix)))
        .unwrap_or_else(|| PathBuf::from(value))
}

fn user_home() -> Option<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn destination() -> Destination {
        Destination {
            hostname: "prod".into(),
            port: 22,
            user: "me".into(),
            identity: None,
            proxy_jump: None,
        }
    }

    #[test]
    fn resolves_exact_ssh_config_alias() {
        let mut dest = destination();
        apply_ssh_config(
            "Host prod\n HostName 192.0.2.8\n User deploy\n Port 2202\n",
            "prod",
            true,
            &mut dest,
        );
        assert_eq!(dest.hostname, "192.0.2.8");
        assert_eq!(dest.user, "deploy");
        assert_eq!(dest.port, 2202);
    }

    #[test]
    fn resolves_proxy_jump_alias() {
        let mut dest = destination();
        apply_ssh_config(
            "Host prod\n HostName 192.0.2.8\n ProxyJump bastion\n",
            "prod",
            true,
            &mut dest,
        );
        assert_eq!(dest.proxy_jump.as_deref(), Some("bastion"));
    }

    #[test]
    fn selects_concrete_hosts_by_group_prefix() {
        let config = "Host prod-api-02\n HostName 192.0.2.2\nHost prod-api-01 prod-worker-01\n HostName ignored\nHost prod-*\n User deploy\nHost *\n Port 22\n";
        assert_eq!(
            select_hosts_from_config(config, "prod"),
            vec!["prod-api-01", "prod-api-02", "prod-worker-01"]
        );
        assert_eq!(
            select_hosts_from_config(config, "prod-api"),
            vec!["prod-api-01", "prod-api-02"]
        );
    }
}
