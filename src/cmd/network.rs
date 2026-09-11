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
    let log = format!("/tmp/binport-tcp-{nonce}.log");
    let port = port.to_string();
    let duration = duration.to_string();
    let server = background_command(
        &log,
        "python3",
        &["-c", TCP_SERVER, nonce, &port, &duration],
    )?;
    let (status, stdout, stderr) = capture_remote(peer, server, password).await?;
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
    let measured = capture_remote(source, client, password).await;
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
  printf '%s\t%s\t%s\t%s\n' "$dev" "$iface" "$cidr" "${rate:-0}"
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
                        "{}/{} -> {}/{} · {:.2} Gbps",
                        item.pair.source.device,
                        item.pair.source.address,
                        item.pair.peer.device,
                        item.pair.peer.address,
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
        capture_remote(source, source_command, password),
        capture_remote(peer, peer_command, password)
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
    let log = format!("/tmp/binport-rdma-{nonce}.log");
    let port = port.to_string();
    let duration = duration.to_string();
    let server = background_command(
        &log,
        "ib_write_bw",
        &[
            "-d",
            &pair.peer.device,
            "-F",
            "--report_gbits",
            "-D",
            &duration,
            "-p",
            &port,
        ],
    )?;
    let (status, stdout, stderr) = capture_remote(peer, server, password).await?;
    if status != 0 {
        return Err(io::Error::other(
            String::from_utf8_lossy(&stderr).trim().to_owned(),
        ));
    }
    let pid = parse_pid(&stdout)?;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let client = binport::execute_command(
        "ib_write_bw",
        &[
            "-d".into(),
            pair.source.device.clone().into(),
            "-F".into(),
            "--report_gbits".into(),
            "-D".into(),
            duration.into(),
            "-p".into(),
            port.into(),
            peer_address.into(),
        ],
    )?;
    let measured = capture_remote(source, client, password).await;
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

fn background_command(log: &str, executable: &str, args: &[&str]) -> io::Result<String> {
    let mut values = vec![
        "-c".into(),
        "log=$1; shift; nohup \"$@\" >\"$log\" 2>&1 </dev/null & echo $!".into(),
        "binport-background".into(),
        log.into(),
        executable.into(),
    ];
    values.extend(args.iter().map(|value| (*value).into()));
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
        let _ = capture_remote(host, command, password).await;
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
            parse_endpoint("mlx5_0\tens1\t172.11.0.20/23\t400").unwrap(),
            parse_endpoint("mlx5_bond_0\tbond0\t10.0.0.20/24\t25").unwrap(),
        ];
        let peer = [
            parse_endpoint("mlx5_2\tens1\t172.11.0.15/23\t400").unwrap(),
            parse_endpoint("mlx5_bond_0\tbond0\t10.0.0.15/24\t25").unwrap(),
        ];
        let pairs = fastest_fabric(pair_fabrics(&source, &peer));
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].source.device, "mlx5_0");
        assert_eq!(pairs[0].peer.device, "mlx5_2");
    }

    #[test]
    fn does_not_pair_different_subnets() {
        let source = [parse_endpoint("mlx5_0\tens1\t172.11.0.20/24\t400").unwrap()];
        let peer = [parse_endpoint("mlx5_2\tens1\t172.11.1.15/24\t400").unwrap()];
        assert!(pair_fabrics(&source, &peer).is_empty());
    }

    #[test]
    fn summarizes_rdma_fabrics_without_guessing_their_purpose() {
        let mut source = Vec::new();
        let mut peer = Vec::new();
        for (index, rate) in [400, 400, 100, 25].into_iter().enumerate() {
            source.push(
                parse_endpoint(&format!(
                    "mlx5_{index}\tens{index}\t172.{}.0.20/24\t{rate}",
                    index + 10
                ))
                .unwrap(),
            );
            peer.push(
                parse_endpoint(&format!(
                    "mlx5_{}\tens{index}\t172.{}.0.15/24\t{rate}",
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
}
