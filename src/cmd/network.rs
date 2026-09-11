use super::native_exec::capture_remote;
use std::collections::BTreeMap;
use std::io;
use std::net::Ipv4Addr;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct BandwidthResult {
    pub metrics: BTreeMap<String, String>,
    pub observations: Vec<String>,
}

pub async fn measure(
    source: &str,
    peer: &str,
    peer_address: &str,
    duration: u8,
    password: Option<&str>,
) -> BandwidthResult {
    let mut result = BandwidthResult {
        metrics: BTreeMap::new(),
        observations: Vec::new(),
    };
    if duration == 0 {
        result
            .observations
            .push("Bandwidth duration must be greater than zero".into());
        return result;
    }
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let offset = nonce
        .bytes()
        .fold(0_u16, |value, byte| value.wrapping_add(byte.into()))
        % 10_000;
    let tcp_port = 30_000 + offset;
    match measure_tcp(
        source,
        peer,
        peer_address,
        tcp_port,
        duration,
        &nonce,
        password,
    )
    .await
    {
        Ok(metrics) => result.metrics.extend(metrics),
        Err(error) => {
            result
                .metrics
                .insert("tcp_throughput".into(), "unavailable".into());
            result
                .observations
                .push(format!("TCP throughput test failed: {error}"));
        }
    }

    measure_rdma_fabric(
        &mut result,
        source,
        peer,
        peer_address,
        tcp_port.saturating_add(1),
        duration,
        &nonce,
        password,
    )
    .await;
    result
}

const TCP_SERVER: &str = r#"import socket,sys
token=sys.argv[1].encode(); port=int(sys.argv[2]); timeout=int(sys.argv[3])+10
s=socket.socket(); s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1); s.bind(("",port)); s.listen(1); s.settimeout(timeout)
c,_=s.accept(); c.settimeout(timeout); auth=b""
while not auth.endswith(b"\n") and len(auth)<256: auth+=c.recv(1)
if auth.rstrip()!=token: raise SystemExit(2)
while c.recv(1024*1024): pass
c.close(); s.close()"#;

const TCP_CLIENT: &str = r#"import socket,sys,time
host=sys.argv[1]; port=int(sys.argv[2]); duration=float(sys.argv[3]); token=sys.argv[4].encode()
s=socket.socket(); s.settimeout(5); deadline=time.monotonic()+5
while True:
 try: s.connect((host,port)); break
 except OSError:
  if time.monotonic()>=deadline: raise
  time.sleep(.1)
s.settimeout(duration+5); s.sendall(token+b"\n"); payload=bytes(1024*1024); sent=0; start=time.monotonic(); end=start+duration
while time.monotonic()<end: s.sendall(payload); sent+=len(payload)
s.shutdown(socket.SHUT_WR); elapsed=time.monotonic()-start; s.close()
print("tcp_throughput_gbps\t"+str(round(sent*8/elapsed/1e9,3)))
print("tcp_bytes_sent\t"+str(sent)); print("tcp_duration_seconds\t"+str(round(elapsed,3)))"#;

async fn measure_tcp(
    source: &str,
    peer: &str,
    address: &str,
    port: u16,
    duration: u8,
    nonce: &str,
    password: Option<&str>,
) -> io::Result<BTreeMap<String, String>> {
    let duration_seconds = u64::from(duration);
    let log = format!("/tmp/binport-tcp-{nonce}.log");
    let port = port.to_string();
    let duration = duration.to_string();
    let server = background_command(
        &log,
        "python3",
        &["-c", TCP_SERVER, nonce, &port, &duration],
    )?;
    let (status, stdout, stderr) = bounded_capture(peer, server, password, 20).await?;
    if status != 0 {
        return Err(io::Error::other(format!(
            "could not start receiver: {}",
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    let pid = parse_pid(&stdout)?;
    let client = binport::execute_command(
        "python3",
        &[
            "-c".into(),
            TCP_CLIENT.into(),
            address.into(),
            port.into(),
            duration.into(),
            nonce.into(),
        ],
    )?;
    let measured = bounded_capture(source, client, password, duration_seconds + 20).await;
    cleanup(peer, pid, &log, password).await;
    let (status, stdout, stderr) = measured?;
    if status != 0 {
        return Err(io::Error::other(
            String::from_utf8_lossy(&stderr).trim().to_owned(),
        ));
    }
    Ok(parse_metrics(&stdout))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RdmaEndpoint {
    device: String,
    interface: String,
    address: Ipv4Addr,
    prefix: u8,
    rate_gbps: u32,
    numa_node: i32,
    cpu_list: String,
    binder: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RdmaPair {
    source: RdmaEndpoint,
    peer: RdmaEndpoint,
}

struct RdmaMeasurement {
    pair: RdmaPair,
    throughput_gbps: f64,
    message_rate_mpps: String,
    mtu_bytes: Option<u32>,
}

const RDMA_DISCOVERY: &str = r#"command -v ib_write_bw >/dev/null 2>&1 || exit 0
command -v ibdev2netdev >/dev/null 2>&1 || exit 0
ibdev2netdev 2>/dev/null | while read -r dev _ _ _ iface state; do
  [ "$state" = "(Up)" ] || continue
  cidr=$(ip -o -4 addr show dev "$iface" 2>/dev/null | awk 'NR==1 {print $4}')
  [ -n "$cidr" ] || continue
  rate=$(cat "/sys/class/infiniband/$dev/ports/1/rate" 2>/dev/null | awk '{print int($1)}')
  numa=$(cat "/sys/class/net/$iface/device/numa_node" 2>/dev/null || printf '%s' -1)
  cpus=$([ "${numa:--1}" -ge 0 ] 2>/dev/null && cat "/sys/devices/system/node/node$numa/cpulist" 2>/dev/null)
  if command -v numactl >/dev/null 2>&1; then binder=numactl; elif command -v taskset >/dev/null 2>&1; then binder=taskset; else binder=none; fi
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$dev" "$iface" "$cidr" "${rate:-0}" "${numa:--1}" "${cpus:--}" "$binder"
done"#;

#[allow(clippy::too_many_arguments)]
async fn measure_rdma_fabric(
    result: &mut BandwidthResult,
    source: &str,
    peer: &str,
    peer_address: &str,
    first_port: u16,
    duration: u8,
    nonce: &str,
    password: Option<&str>,
) {
    let all_pairs = match discover_pairs(source, peer, password).await {
        Ok(pairs) if !pairs.is_empty() => pairs,
        Ok(_) => {
            result.observations.push(
                "RDMA throughput test skipped: no active same-subnet RDMA fabric was discovered"
                    .into(),
            );
            return;
        }
        Err(error) => {
            result
                .observations
                .push(format!("RDMA fabric discovery failed: {error}"));
            return;
        }
    };
    let fabrics = summarize_fabrics(&all_pairs);
    result.metrics.insert("rdma_fabrics".into(), fabrics);
    let pairs = fastest_fabric(all_pairs);
    let advertised = pairs
        .iter()
        .map(|pair| pair.source.rate_gbps.min(pair.peer.rate_gbps))
        .max()
        .unwrap_or(0);
    result.metrics.insert(
        "selected_rdma_fabric".into(),
        format!(
            "{} x {} Gbps (fastest same-subnet fabric)",
            pairs.len(),
            advertised
        ),
    );
    if let Some(pair) = pairs.first() {
        result.metrics.insert(
            "rdma_numa_binding".into(),
            format!("source={}, peer={}", pair.source.binder, pair.peer.binder),
        );
    }

    let mut tests = tokio::task::JoinSet::new();
    for (index, pair) in pairs.into_iter().enumerate() {
        let link_nonce = format!("{nonce}-{index}");
        let source = source.to_owned();
        let peer = peer.to_owned();
        let peer_address = peer_address.to_owned();
        let password = password.map(str::to_owned);
        tests.spawn(async move {
            let measured = measure_rdma_link(
                &source,
                &peer,
                &peer_address,
                first_port.saturating_add(index as u16),
                duration,
                link_nonce,
                pair,
                password.as_deref(),
            )
            .await;
            (index, measured)
        });
    }
    let mut measured = Vec::new();
    while let Some(joined) = tests.join_next().await {
        match joined {
            Ok(item) => measured.push(item),
            Err(error) => result
                .observations
                .push(format!("RDMA test task failed: {error}")),
        }
    }
    measured.sort_by_key(|(index, _)| *index);
    let mut successes = Vec::new();
    for (index, measurement) in measured {
        match measurement {
            Ok(item) => {
                result.metrics.insert(
                    format!("rdma_link_{:02}", index + 1),
                    format!(
                        "{}/{} (NUMA {}) -> {}/{} (NUMA {}) · {:.2} Gbps",
                        item.pair.source.device,
                        item.pair.source.address,
                        item.pair.source.numa_node,
                        item.pair.peer.device,
                        item.pair.peer.address,
                        item.pair.peer.numa_node,
                        item.throughput_gbps
                    ),
                );
                successes.push(item);
            }
            Err(error) => result
                .observations
                .push(format!("RDMA link {} failed: {error}", index + 1)),
        }
    }
    if successes.is_empty() {
        result.metrics.insert(
            "rdma_aggregate_throughput_gbps".into(),
            "unavailable".into(),
        );
        return;
    }
    let total = successes
        .iter()
        .map(|item| item.throughput_gbps)
        .sum::<f64>();
    let minimum = successes
        .iter()
        .map(|item| item.throughput_gbps)
        .fold(f64::INFINITY, f64::min);
    let maximum = successes
        .iter()
        .map(|item| item.throughput_gbps)
        .fold(0.0_f64, f64::max);
    result
        .metrics
        .insert("rdma_links_measured".into(), successes.len().to_string());
    result.metrics.insert(
        "rdma_aggregate_throughput_gbps".into(),
        format!("{total:.2}"),
    );
    result.metrics.insert(
        "rdma_link_throughput_range_gbps".into(),
        format!("{minimum:.2} - {maximum:.2}"),
    );
    let total_rate = successes
        .iter()
        .filter_map(|item| item.message_rate_mpps.parse::<f64>().ok())
        .sum::<f64>();
    result.metrics.insert(
        "rdma_aggregate_message_rate_mpps".into(),
        format!("{total_rate:.6}"),
    );
    let mut mtus = successes
        .iter()
        .filter_map(|item| item.mtu_bytes)
        .collect::<Vec<_>>();
    mtus.sort_unstable();
    mtus.dedup();
    if !mtus.is_empty() {
        result.metrics.insert(
            "rdma_active_mtu".into(),
            mtus.iter()
                .map(|mtu| format!("{mtu} B"))
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    if minimum < maximum * 0.9 {
        result.observations.push(format!(
            "RDMA link imbalance detected: slowest link is {:.1}% of the fastest",
            minimum / maximum * 100.0
        ));
    }
}

async fn discover_pairs(
    source: &str,
    peer: &str,
    password: Option<&str>,
) -> io::Result<Vec<RdmaPair>> {
    let command = || {
        binport::execute_command(
            "sh",
            &[
                "-c".into(),
                RDMA_DISCOVERY.into(),
                "binport-rdma-discovery".into(),
            ],
        )
    };
    let source_command = command()?;
    let peer_command = command()?;
    let (source_output, peer_output) = tokio::try_join!(
        bounded_capture(source, source_command, password, 20),
        bounded_capture(peer, peer_command, password, 20)
    )?;
    let source_endpoints = parse_endpoints(source_output.0, &source_output.1, &source_output.2)?;
    let peer_endpoints = parse_endpoints(peer_output.0, &peer_output.1, &peer_output.2)?;
    Ok(pair_fabrics(&source_endpoints, &peer_endpoints))
}

fn parse_endpoints(status: u32, stdout: &[u8], stderr: &[u8]) -> io::Result<Vec<RdmaEndpoint>> {
    if status != 0 {
        return Err(io::Error::other(
            String::from_utf8_lossy(stderr).trim().to_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(parse_endpoint)
        .collect())
}

fn parse_endpoint(line: &str) -> Option<RdmaEndpoint> {
    let mut fields = line.split('\t');
    let device = fields.next()?.to_owned();
    let interface = fields.next()?.to_owned();
    let (address, prefix) = fields.next()?.split_once('/')?;
    Some(RdmaEndpoint {
        device,
        interface,
        address: address.parse().ok()?,
        prefix: prefix.parse().ok()?,
        rate_gbps: fields.next()?.parse().ok()?,
        numa_node: fields.next()?.parse().ok()?,
        cpu_list: fields.next()?.to_owned(),
        binder: fields.next()?.to_owned(),
    })
}

fn pair_fabrics(source: &[RdmaEndpoint], peer: &[RdmaEndpoint]) -> Vec<RdmaPair> {
    let mut pairs = source
        .iter()
        .flat_map(|left| {
            peer.iter()
                .filter(move |right| same_subnet(left, right))
                .map(move |right| RdmaPair {
                    source: left.clone(),
                    peer: right.clone(),
                })
        })
        .collect::<Vec<_>>();
    pairs.sort_by(|left, right| left.source.interface.cmp(&right.source.interface));
    pairs
}

fn fastest_fabric(mut pairs: Vec<RdmaPair>) -> Vec<RdmaPair> {
    let fastest = pairs
        .iter()
        .map(|pair| pair.source.rate_gbps.min(pair.peer.rate_gbps))
        .max()
        .unwrap_or(0);
    pairs.retain(|pair| pair.source.rate_gbps.min(pair.peer.rate_gbps) == fastest);
    pairs.sort_by(|left, right| left.source.interface.cmp(&right.source.interface));
    pairs
}

fn summarize_fabrics(pairs: &[RdmaPair]) -> String {
    let mut rates = BTreeMap::<u32, usize>::new();
    for pair in pairs {
        *rates
            .entry(pair.source.rate_gbps.min(pair.peer.rate_gbps))
            .or_default() += 1;
    }
    rates
        .iter()
        .rev()
        .map(|(rate, count)| format!("{count} x {rate} Gbps"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn same_subnet(left: &RdmaEndpoint, right: &RdmaEndpoint) -> bool {
    if left.prefix != right.prefix || left.prefix > 32 {
        return false;
    }
    let mask = if left.prefix == 0 {
        0
    } else {
        u32::MAX << (32 - left.prefix)
    };
    u32::from(left.address) & mask == u32::from(right.address) & mask
}

#[allow(clippy::too_many_arguments)]
async fn measure_rdma_link(
    source: &str,
    peer: &str,
    peer_address: &str,
    port: u16,
    duration: u8,
    nonce: String,
    pair: RdmaPair,
    password: Option<&str>,
) -> io::Result<RdmaMeasurement> {
    let duration_seconds = u64::from(duration);
    let log = format!("/tmp/binport-rdma-{nonce}.log");
    let port = port.to_string();
    let duration = duration.to_string();
    let server_args = vec![
        "-d".into(),
        pair.peer.device.clone(),
        "-F".into(),
        "--report_gbits".into(),
        "-D".into(),
        duration.clone(),
        "-p".into(),
        port.clone(),
    ];
    let (server_executable, server_args) = numa_bound_command(&pair.peer, server_args);
    let (server_executable, server_args) =
        remote_timeout_command(server_executable, server_args, duration_seconds + 15);
    let server = background_command(&log, &server_executable, &server_args)?;
    let (status, stdout, stderr) = bounded_capture(peer, server, password, 20).await?;
    if status != 0 {
        return Err(io::Error::other(
            String::from_utf8_lossy(&stderr).trim().to_owned(),
        ));
    }
    let pid = parse_pid(&stdout)?;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let client_args = vec![
        "-d".into(),
        pair.source.device.clone(),
        "-F".into(),
        "--report_gbits".into(),
        "-D".into(),
        duration,
        "-p".into(),
        port,
        peer_address.into(),
    ];
    let (client_executable, client_args) = numa_bound_command(&pair.source, client_args);
    let (client_executable, client_args) =
        remote_timeout_command(client_executable, client_args, duration_seconds + 15);
    let client = binport::execute_command(
        &client_executable,
        &client_args.into_iter().map(Into::into).collect::<Vec<_>>(),
    )?;
    let measured = bounded_capture(source, client, password, duration_seconds + 20).await;
    cleanup(peer, pid, &log, password).await;
    let (status, stdout, stderr) = measured?;
    if status != 0 {
        return Err(io::Error::other(
            String::from_utf8_lossy(&stderr).trim().to_owned(),
        ));
    }
    let text = String::from_utf8_lossy(&stdout);
    let mtu_bytes = text.lines().find_map(parse_rdma_mtu);
    let (throughput, message_rate_mpps) = text
        .lines()
        .filter_map(parse_rdma_row)
        .next_back()
        .ok_or_else(|| io::Error::other("ib_write_bw returned no result row"))?;
    Ok(RdmaMeasurement {
        pair,
        throughput_gbps: throughput
            .parse()
            .map_err(|_| io::Error::other("invalid ib_write_bw throughput"))?,
        message_rate_mpps,
        mtu_bytes,
    })
}

fn numa_bound_command(endpoint: &RdmaEndpoint, arguments: Vec<String>) -> (String, Vec<String>) {
    let mut output = Vec::new();
    let executable = match endpoint.binder.as_str() {
        "numactl" if endpoint.numa_node >= 0 => {
            output.push(format!("--cpunodebind={}", endpoint.numa_node));
            output.push(format!("--membind={}", endpoint.numa_node));
            output.push("ib_write_bw".into());
            "numactl"
        }
        "taskset" if endpoint.numa_node >= 0 && endpoint.cpu_list != "-" => {
            output.push("-c".into());
            output.push(endpoint.cpu_list.clone());
            output.push("ib_write_bw".into());
            "taskset"
        }
        _ => "ib_write_bw",
    };
    output.extend(arguments);
    (executable.into(), output)
}

fn remote_timeout_command(
    executable: String,
    arguments: Vec<String>,
    seconds: u64,
) -> (String, Vec<String>) {
    let mut output = vec![
        "-c".into(),
        "limit=$1; shift; if command -v timeout >/dev/null 2>&1; then exec timeout -k 2 \"$limit\" \"$@\"; else exec \"$@\"; fi".into(),
        "binport-timeout".into(),
        seconds.to_string(),
        executable,
    ];
    output.extend(arguments);
    ("sh".into(), output)
}

async fn bounded_capture(
    host: &str,
    command: String,
    password: Option<&str>,
    seconds: u64,
) -> io::Result<(u32, Vec<u8>, Vec<u8>)> {
    tokio::time::timeout(
        std::time::Duration::from_secs(seconds),
        capture_remote(host, command, password),
    )
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!("operation timed out after {seconds}s"),
        )
    })?
}

fn background_command<S: AsRef<str>>(
    log: &str,
    executable: &str,
    args: &[S],
) -> io::Result<String> {
    let mut values = vec![
        "-c".into(),
        "log=$1; shift; nohup \"$@\" >\"$log\" 2>&1 </dev/null & echo $!".into(),
        "binport-background".into(),
        log.into(),
        executable.into(),
    ];
    values.extend(args.iter().map(|value| value.as_ref().into()));
    binport::execute_command("sh", &values)
}

fn parse_pid(output: &[u8]) -> io::Result<u32> {
    String::from_utf8_lossy(output)
        .trim()
        .parse::<u32>()
        .map_err(|_| io::Error::other("remote receiver did not return a process id"))
}

async fn cleanup(host: &str, pid: u32, log: &str, password: Option<&str>) {
    if let Ok(command) = binport::execute_command(
        "sh",
        &[
            "-c".into(),
            "kill \"$1\" 2>/dev/null || true; rm -f -- \"$2\"".into(),
            "binport-cleanup".into(),
            pid.to_string().into(),
            log.into(),
        ],
    ) {
        let _ = bounded_capture(host, command, password, 10).await;
    }
}

fn parse_metrics(output: &[u8]) -> BTreeMap<String, String> {
    String::from_utf8_lossy(output)
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(key, value)| (key.into(), value.into()))
        .collect()
}

fn parse_rdma_row(line: &str) -> Option<(String, String)> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    (fields.len() >= 5
        && fields[0].parse::<u64>().is_ok()
        && fields[1].parse::<u64>().is_ok()
        && fields[3].parse::<f64>().is_ok())
    .then(|| (fields[3].into(), fields[4].into()))
}

fn parse_rdma_mtu(line: &str) -> Option<u32> {
    let value = line.split_once("Mtu")?.1.split_once(':')?.1.trim();
    value.strip_suffix("[B]")?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rdma_bandwidth_rows() {
        let row = parse_rdma_row("65536 5000 23.14 22.85 0.04358").unwrap();
        assert_eq!(row, ("22.85".into(), "0.04358".into()));
        assert!(parse_rdma_row("#bytes #iterations BW").is_none());
        assert_eq!(parse_rdma_mtu(" Mtu             : 4096[B]"), Some(4096));
    }

    #[test]
    fn parses_tab_separated_tcp_metrics() {
        let metrics = parse_metrics(b"tcp_throughput_gbps\t8.2\ntcp_bytes_sent\t42\n");
        assert_eq!(metrics["tcp_throughput_gbps"], "8.2");
    }

    #[test]
    fn discovers_only_the_fastest_same_subnet_fabric() {
        let source = [
            parse_endpoint("mlx5_0\tens1\t172.11.0.20/23\t400\t0\t0-63\ttaskset").unwrap(),
            parse_endpoint("mlx5_bond_0\tbond0\t10.0.0.20/24\t25\t-1\t-\tnone").unwrap(),
        ];
        let peer = [
            parse_endpoint("mlx5_2\tens1\t172.11.0.15/23\t400\t0\t0-63\tnumactl").unwrap(),
            parse_endpoint("mlx5_bond_0\tbond0\t10.0.0.15/24\t25\t-1\t-\tnone").unwrap(),
        ];
        let pairs = fastest_fabric(pair_fabrics(&source, &peer));
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].source.device, "mlx5_0");
        assert_eq!(pairs[0].peer.device, "mlx5_2");
    }

    #[test]
    fn does_not_pair_different_subnets() {
        let source =
            [parse_endpoint("mlx5_0\tens1\t172.11.0.20/24\t400\t0\t0-63\ttaskset").unwrap()];
        let peer = [parse_endpoint("mlx5_2\tens1\t172.11.1.15/24\t400\t0\t0-63\tnumactl").unwrap()];
        assert!(pair_fabrics(&source, &peer).is_empty());
    }

    #[test]
    fn summarizes_rdma_fabrics_without_guessing_their_purpose() {
        let mut source = Vec::new();
        let mut peer = Vec::new();
        for (index, rate) in [400, 400, 100, 25].into_iter().enumerate() {
            source.push(
                parse_endpoint(&format!(
                    "mlx5_{index}\tens{index}\t172.{}.0.20/24\t{rate}\t0\t0-63\ttaskset",
                    index + 10
                ))
                .unwrap(),
            );
            peer.push(
                parse_endpoint(&format!(
                    "mlx5_{}\tens{index}\t172.{}.0.15/24\t{rate}\t0\t0-63\tnumactl",
                    index + 5,
                    index + 10
                ))
                .unwrap(),
            );
        }
        assert_eq!(
            summarize_fabrics(&pair_fabrics(&source, &peer)),
            "2 x 400 Gbps, 1 x 100 Gbps, 1 x 25 Gbps"
        );
    }

    #[test]
    fn selects_available_numa_binding_tool() {
        let taskset =
            parse_endpoint("mlx5_0\tens1\t172.11.0.20/24\t400\t1\t64-127\ttaskset").unwrap();
        let (executable, args) = numa_bound_command(&taskset, vec!["-d".into(), "mlx5_0".into()]);
        assert_eq!(executable, "taskset");
        assert_eq!(args[..3], ["-c", "64-127", "ib_write_bw"]);

        let numactl =
            parse_endpoint("mlx5_2\tens1\t172.11.0.15/24\t400\t0\t0-63\tnumactl").unwrap();
        let (executable, args) = numa_bound_command(&numactl, Vec::new());
        assert_eq!(executable, "numactl");
        assert_eq!(args, ["--cpunodebind=0", "--membind=0", "ib_write_bw"]);
    }

    #[test]
    fn wraps_remote_benchmark_with_a_hard_deadline() {
        let (executable, args) =
            remote_timeout_command("ib_write_bw".into(), vec!["-d".into(), "mlx5_0".into()], 20);
        assert_eq!(executable, "sh");
        assert_eq!(args[3..], ["20", "ib_write_bw", "-d", "mlx5_0"]);
        assert!(args[1].contains("timeout -k 2"));
    }
}
