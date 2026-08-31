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
    pub bastion_proxy: Option<BastionProxy>,
}

#[derive(Clone, Debug)]
pub struct BastionProxy {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub account: String,
    pub preset: Option<String>,
    pub format: String,
}

impl BastionProxy {
    pub fn format_username(&self, target_host: &str) -> String {
        self.format
            .replace("{user}", &self.user)
            .replace("{host}", target_host)
            .replace("{account}", &self.account)
    }
}

#[derive(Clone)]
pub struct NativeSsh {
    client: Client,
    _jump: Option<Client>,
    bastion: Option<String>,
    destination: Option<Destination>,
    password: Option<String>,
}

#[derive(Clone)]
pub struct SharedJump {
    client: Client,
}

impl SharedJump {
    pub async fn probe_direct_tcpip(&self, host: &str, port: u16) -> io::Result<()> {
        let target = format!("{host}:{port}");
        self.client
            .open_direct_tcpip_channel(target.as_str(), None)
            .await
            .map(|_| ())
            .map_err(io::Error::other)
    }
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
            bastion_proxy: None,
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
        Self::connect_jump_destination(&destination, password).await
    }

    pub async fn connect_jump_destination(
        destination: &Destination,
        password: Option<&str>,
    ) -> io::Result<SharedJump> {
        if destination.proxy_jump.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "nested ProxyJump chains are not supported yet",
            ));
        }
        Ok(SharedJump {
            client: connect_direct(destination, password).await?,
        })
    }

    pub async fn connect_with_jump(
        destination: &Destination,
        password: Option<&str>,
        jump: &SharedJump,
    ) -> io::Result<Self> {
        let mut last_error = None;
        let mut client = None;
        for auth in auth_methods(destination, password)? {
            match Client::connect_via(
                &jump.client,
                (destination.hostname.as_str(), destination.port),
                &destination.user,
                auth,
                ServerCheckMethod::DefaultKnownHostsFile,
            )
            .await
            {
                Ok(connected) => {
                    client = Some(connected);
                    break;
                }
                Err(error) => last_error = Some(error),
            }
        }
        let client =
            client.ok_or_else(|| io::Error::other(last_error.expect("auth candidates")))?;
        Ok(Self {
            client,
            _jump: Some(jump.client.clone()),
            bastion: None,
            destination: Some(destination.clone()),
            password: password.map(str::to_owned),
        })
    }

    pub async fn connect(destination: &Destination, password: Option<&str>) -> io::Result<Self> {
        if destination.proxy_jump.is_some() && destination.bastion_proxy.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ProxyJump and BastionProxy cannot be used together",
            ));
        }
        if let Some(bastion) = &destination.bastion_proxy {
            let client = connect_bastion(bastion, &destination.hostname, password).await?;
            return Ok(Self {
                client,
                _jump: None,
                bastion: Some(bastion.host.clone()),
                destination: Some(destination.clone()),
                password: password.map(str::to_owned),
            });
        }
        if let Some(proxy) = &destination.proxy_jump {
            // When --password is used, pass it to the jump host too so password
            // authentication is available. Without a password, the jump host falls
            // back to key file or SSH agent authentication.
            let jump = Self::connect_jump(proxy, password).await?;
            return Self::connect_with_jump(destination, password, &jump).await;
        }

        Ok(Self {
            client: connect_direct(destination, password).await?,
            _jump: None,
            bastion: None,
            destination: Some(destination.clone()),
            password: password.map(str::to_owned),
        })
    }

    pub fn uses_proxy_jump(&self) -> bool {
        self._jump.is_some()
    }

    pub fn route_label(&self) -> Option<String> {
        if self._jump.is_some() {
            return Some("proxy_jump".to_owned());
        }
        self.bastion.as_ref().map(|host| format!("bastion:{host}"))
    }

    pub fn is_bastion(&self) -> bool {
        self.bastion.is_some()
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub async fn reconnect(&self) -> io::Result<Self> {
        let destination = self.destination.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "connection cannot be recreated; no stored destination",
            )
        })?;
        Self::connect(destination, self.password.as_deref()).await
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
                    let _ = input_tx.send(Vec::new()).await;
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

    /// Run a stream protocol over SSH while the local endpoint behaves like a TTY.
    /// The entry command itself must not receive a PTY because `prefix` is binary.
    pub async fn execute_tty_with_prefix(
        &self,
        command: &str,
        prefix: Vec<u8>,
        eof_on_quit: bool,
    ) -> io::Result<u32> {
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
        input_tx.send(prefix).await.map_err(io::Error::other)?;
        let input_task = tokio::spawn(async move {
            let mut stdin = tokio::io::stdin();
            let mut buffer = vec![0_u8; 4096];
            loop {
                let read = stdin.read(&mut buffer).await?;
                if read == 0 {
                    let _ = input_tx.send(Vec::new()).await;
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
        let execution = self.client.execute_io(
            command,
            output_tx.clone(),
            Some(output_tx),
            Some(input_rx),
            false,
            None,
        );
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

    pub async fn execute_with_prefix_file(
        &self,
        command: &str,
        prefix: Vec<u8>,
        path: &Path,
        progress: TransferProgress,
    ) -> io::Result<(u32, Vec<u8>, Vec<u8>)> {
        let (stdout_tx, mut stdout_rx) = mpsc::channel(8);
        let (stderr_tx, mut stderr_rx) = mpsc::channel(8);
        let (stdin_tx, stdin_rx) = mpsc::channel(8);
        let path = path.to_owned();
        let feeder_progress = progress.clone();
        let feeder = tokio::spawn(async move {
            stdin_tx.send(prefix).await.map_err(io::Error::other)?;
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
            let _ = stdin_tx.send(Vec::new()).await;
            Ok::<_, io::Error>(())
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
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let status = loop {
            tokio::select! {
                result = &mut execution => break result.map_err(io::Error::other)?,
                Some(data) = stdout_rx.recv() => stdout.extend_from_slice(&data),
                Some(data) = stderr_rx.recv() => stderr.extend_from_slice(&data),
            }
        };
        feeder.await.map_err(io::Error::other)??;
        while let Ok(data) = stdout_rx.try_recv() {
            stdout.extend_from_slice(&data);
        }
        while let Ok(data) = stderr_rx.try_recv() {
            stderr.extend_from_slice(&data);
        }
        progress.finish();
        Ok((status, stdout, stderr))
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

    pub async fn download_file_with_input(
        &self,
        command: &str,
        input: Vec<u8>,
        path: &Path,
        progress: TransferProgress,
    ) -> io::Result<(u32, Vec<u8>)> {
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

    /// Execute a command on a fresh connection if this is a bastion proxy.
    /// Bastion hosts only support one exec channel per connection.
    pub async fn execute_capture_fresh(&self, command: &str) -> io::Result<(u32, String, String)> {
        if self.is_bastion() {
            self.reconnect().await?.execute_capture(command).await
        } else {
            self.execute_capture(command).await
        }
    }

    /// Execute a streaming command on a fresh connection if this is a bastion proxy.
    pub async fn execute_stream_fresh(
        &self,
        command: &str,
        output: mpsc::UnboundedSender<StreamChunk>,
    ) -> io::Result<u32> {
        if self.is_bastion() {
            self.reconnect()
                .await?
                .execute_stream(command, output)
                .await
        } else {
            self.execute_stream(command, output).await
        }
    }

    /// Execute a command with stdin input on a fresh connection if bastion.
    pub async fn execute_capture_with_input_fresh(
        &self,
        command: &str,
        input: Vec<u8>,
    ) -> io::Result<(u32, Vec<u8>, Vec<u8>)> {
        if self.is_bastion() {
            self.reconnect()
                .await?
                .execute_capture_with_input(command, input)
                .await
        } else {
            self.execute_capture_with_input(command, input).await
        }
    }

    /// Upload a file on a fresh connection if this is a bastion proxy.
    pub async fn upload_file_fresh(
        &self,
        command: &str,
        path: &Path,
        progress: TransferProgress,
    ) -> io::Result<(u32, Vec<u8>)> {
        if self.is_bastion() {
            self.reconnect()
                .await?
                .upload_file(command, path, progress)
                .await
        } else {
            self.upload_file(command, path, progress).await
        }
    }

    /// Execute a TTY command on a fresh connection if this is a bastion proxy.
    pub async fn execute_tty_fresh(&self, command: &str, eof_on_quit: bool) -> io::Result<u32> {
        if self.is_bastion() {
            self.reconnect()
                .await?
                .execute_tty(command, eof_on_quit)
                .await
        } else {
            self.execute_tty(command, eof_on_quit).await
        }
    }
}

struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

async fn connect_direct(destination: &Destination, password: Option<&str>) -> io::Result<Client> {
    let mut last_error = None;
    for auth in auth_methods(destination, password)? {
        match Client::connect(
            (destination.hostname.as_str(), destination.port),
            &destination.user,
            auth,
            ServerCheckMethod::DefaultKnownHostsFile,
        )
        .await
        {
            Ok(client) => return Ok(client),
            Err(error) => last_error = Some(error),
        }
    }
    Err(io::Error::other(last_error.expect("auth candidates")))
}

fn bastion_password_for_host(host: &str) -> Option<String> {
    // Try per-host env var first: BINPORT_BASTION_PASSWORD_10_121_61_3
    let suffix = bastion_env_suffix(host);
    let host_key = format!("BINPORT_BASTION_PASSWORD_{suffix}");
    if let Ok(password) = env::var(&host_key) {
        return Some(password);
    }
    // Fall back to generic env var: BINPORT_BASTION_PASSWORD
    env::var("BINPORT_BASTION_PASSWORD").ok()
}

async fn connect_bastion(
    bastion: &BastionProxy,
    target_host: &str,
    password: Option<&str>,
) -> io::Result<Client> {
    let composite_user = bastion.format_username(target_host);
    let auth = if let Some(password) = password {
        AuthMethod::with_password(password)
    } else if let Some(password) = bastion_password_for_host(&bastion.host) {
        AuthMethod::with_password(&password)
    } else if let Some(auth) = agent_auth_method() {
        auth
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "bastion proxy requires SSH Agent or password authentication for {}; use --password, set BINPORT_BASTION_PASSWORD_{}, or set BINPORT_BASTION_PASSWORD",
                bastion.host,
                bastion_env_suffix(&bastion.host)
            ),
        ));
    };
    Client::connect(
        (bastion.host.as_str(), bastion.port),
        &composite_user,
        auth,
        ServerCheckMethod::DefaultKnownHostsFile,
    )
    .await
    .map_err(io::Error::other)
}

#[cfg(unix)]
fn agent_auth_method() -> Option<AuthMethod> {
    env::var_os("SSH_AUTH_SOCK")
        .is_some()
        .then(AuthMethod::with_agent)
}

#[cfg(not(unix))]
fn agent_auth_method() -> Option<AuthMethod> {
    None
}

fn bastion_env_suffix(host: &str) -> String {
    host.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
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

fn auth_methods(destination: &Destination, password: Option<&str>) -> io::Result<Vec<AuthMethod>> {
    if password.is_some() {
        return Ok(vec![auth_method(destination, password)?]);
    }
    let mut paths = Vec::new();
    if let Some(identity) = &destination.identity {
        paths.push(identity.clone());
    }
    if let Some(home) = user_home() {
        let ssh_dir = home.join(".ssh");
        for name in ["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"] {
            let path = ssh_dir.join(name);
            if path.is_file() && !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    let mut methods = paths
        .into_iter()
        .map(|path| AuthMethod::with_key_file(path, None))
        .collect::<Vec<_>>();
    if let Some(agent) = agent_auth_method() {
        methods.push(agent);
    }
    if methods.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no SSH agent or private key found",
        ));
    }
    Ok(methods)
}

fn apply_ssh_config(source: &str, alias: &str, allow_user: bool, destination: &mut Destination) {
    let mut active = false;
    let mut hostname_set = false;
    let mut user_set = !allow_user;
    let mut port_set = false;
    let mut identity_set = false;
    let mut proxy_jump_set = false;
    let mut bastion_host_set = false;
    let mut bastion_user_set = false;
    let mut bastion_account_set = false;
    let mut bastion_port_set = false;
    let mut bastion_format_set = false;
    let mut bastion_preset_set = false;
    let mut bastion_host = String::new();
    let mut bastion_port: u16 = 22;
    let mut bastion_user = String::new();
    let mut bastion_account = String::new();
    let mut bastion_format = String::new();
    let mut bastion_preset = String::new();
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
            } else if key.eq_ignore_ascii_case("bastionproxy") && !bastion_host_set {
                bastion_host = value.into();
                bastion_host_set = true;
            } else if key.eq_ignore_ascii_case("bastionuser") && !bastion_user_set {
                bastion_user = value.into();
                bastion_user_set = true;
            } else if key.eq_ignore_ascii_case("bastionaccount") && !bastion_account_set {
                bastion_account = value.into();
                bastion_account_set = true;
            } else if key.eq_ignore_ascii_case("bastionport") && !bastion_port_set {
                if let Ok(port) = value.parse() {
                    bastion_port = port;
                    bastion_port_set = true;
                }
            } else if key.eq_ignore_ascii_case("bastionformat") && !bastion_format_set {
                bastion_format = value.into();
                bastion_format_set = true;
            } else if key.eq_ignore_ascii_case("bastionpreset") && !bastion_preset_set {
                bastion_preset = value.into();
                bastion_preset_set = true;
            }
        }
    }
    if bastion_host_set && !bastion_host.is_empty() {
        destination.bastion_proxy = Some(BastionProxy {
            host: bastion_host,
            port: bastion_port,
            user: bastion_user,
            account: bastion_account,
            preset: bastion_preset_set.then_some(bastion_preset),
            format: if bastion_format.is_empty() {
                "{user}/{host}/{account}".into()
            } else {
                bastion_format
            },
        });
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
            bastion_proxy: None,
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
    fn resolves_bastion_proxy_from_ssh_config() {
        let mut dest = destination();
        apply_ssh_config(
            "Host worker\n HostName 10.0.0.5\n User admin\n \
             BastionProxy 10.0.0.1\n BastionUser jumper\n BastionAccount root\n",
            "worker",
            true,
            &mut dest,
        );
        let bastion = dest.bastion_proxy.as_ref().unwrap();
        assert_eq!(bastion.host, "10.0.0.1");
        assert_eq!(bastion.port, 22);
        assert_eq!(bastion.user, "jumper");
        assert_eq!(bastion.account, "root");
        assert_eq!(bastion.format, "{user}/{host}/{account}");
    }

    #[test]
    fn resolves_bastion_proxy_with_custom_format_and_port() {
        let mut dest = destination();
        apply_ssh_config(
            "Host worker\n HostName 10.0.0.5\n \
             BastionProxy 10.0.0.1\n BastionPort 2222\n \
             BastionUser jumper\n BastionAccount admin\n \
             BastionFormat {user}@@{host}@@{account}\n",
            "worker",
            true,
            &mut dest,
        );
        let bastion = dest.bastion_proxy.as_ref().unwrap();
        assert_eq!(bastion.port, 2222);
        assert_eq!(bastion.format, "{user}@@{host}@@{account}");
        assert_eq!(
            bastion.format_username("10.0.0.5"),
            "jumper@@10.0.0.5@@admin"
        );
    }

    #[test]
    fn bastion_format_username_substitutes_all_placeholders() {
        let bastion = BastionProxy {
            host: "10.0.0.1".into(),
            port: 22,
            user: "alice".into(),
            account: "deploy".into(),
            preset: None,
            format: "{user}/{host}/{account}".into(),
        };
        assert_eq!(
            bastion.format_username("192.168.1.5"),
            "alice/192.168.1.5/deploy"
        );
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
