use super::native_exec::capture_remote;
use super::table;
use clap::Args;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io;

const PROBE: &str = r#"
emit() { printf '%s\t%s\n' "$1" "$2"; }
first() { "$@" 2>/dev/null | head -n 1; }
emit system.hostname "$(first hostname)"
emit system.os "$(. /etc/os-release 2>/dev/null; printf '%s' "${PRETTY_NAME:-unknown}")"
emit system.kernel "$(first uname -r)"
emit system.arch "$(first uname -m)"
emit system.glibc "$(first getconf GNU_LIBC_VERSION)"
emit resources.cpu "$(getconf _NPROCESSORS_ONLN 2>/dev/null || true)"
emit resources.memory_kib "$(awk '/^MemTotal:/ {print $2}' /proc/meminfo 2>/dev/null)"
emit resources.disk_free_kib "$(df -Pk / 2>/dev/null | awk 'NR==2 {print $4}')"
emit runtime.shell "${SHELL:-unknown}"
emit runtime.docker "$(first docker --version)"
emit runtime.python "$(first python3 --version)"
emit runtime.java "$(java -version 2>&1 | head -n 1)"
emit configuration.nofile "$(ulimit -n 2>/dev/null || true)"
emit configuration.ip_forward "$(cat /proc/sys/net/ipv4/ip_forward 2>/dev/null || true)"
emit network.dns "$(awk '/^nameserver/ {print $2; exit}' /etc/resolv.conf 2>/dev/null)"
emit accelerator.nvidia_count "$(if command -v nvidia-smi >/dev/null 2>&1; then nvidia-smi -L 2>/dev/null | wc -l | tr -d ' '; else printf 0; fi)"
emit accelerator.nvidia_gpus "$(command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi --query-gpu=name,memory.total,compute_cap --format=csv,noheader 2>/dev/null | paste -sd ';' -)"
emit accelerator.nvidia_driver "$(command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi --query-gpu=driver_version --format=csv,noheader 2>/dev/null | sort -u | paste -sd ',' -)"
emit accelerator.cuda_driver_api "$(command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi 2>/dev/null | sed -n 's/.*CUDA Version: *\([^ ]*\).*/\1/p' | head -n 1)"
emit accelerator.cuda "$(command -v nvcc >/dev/null 2>&1 && nvcc --version 2>/dev/null | awk '/release/ {print $0; exit}')"
emit accelerator.rocm "$(if [ -r /opt/rocm/.info/version ]; then cat /opt/rocm/.info/version; elif command -v hipcc >/dev/null 2>&1; then hipcc --version 2>/dev/null | head -n 1; fi)"
emit accelerator.ascend "$(command -v npu-smi >/dev/null 2>&1 && npu-smi info -l 2>/dev/null | paste -sd ';' -)"
emit accelerator.intel_xpu "$(command -v xpu-smi >/dev/null 2>&1 && xpu-smi discovery 2>/dev/null | paste -sd ';' -)"
if command -v mthreads-gmi >/dev/null 2>&1; then
  moore_list="$(mthreads-gmi -L 2>/dev/null)"
  moore_info="$(mthreads-gmi -cf 2>/dev/null)"
fi
emit accelerator.moore_threads_count "$(printf '%s\n' "$moore_list" | grep -c '^GPU ')"
emit accelerator.moore_threads_gpus "$(printf '%s\n' "$moore_list" | sed -n 's/^GPU [0-9][0-9]* : *\(.*\) *(UUID.*/\1/p' | sort | uniq -c | awk '{count=$1; $1=""; sub(/^ /, ""); printf "%s%s x%s", separator, $0, count; separator=", "}')"
emit accelerator.moore_threads_driver "$(printf '%s\n' "$moore_info" | sed -n 's/.*Driver Version: *\([^ ]*\).*/\1/p' | head -n 1)"
emit accelerator.moore_threads_vram "$(printf '%s\n' "$moore_info" | sed -n 's/.*MiB(\([0-9][0-9]*MiB\)).*/\1 per GPU/p' | head -n 1)"
emit accelerator.musa "$(command -v musa_driver_version >/dev/null 2>&1 && musa_driver_version 2>/dev/null | sed -n 's/.*"version":[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
emit accelerator.numa_nodes "$(find /sys/devices/system/node -maxdepth 1 -type d -name 'node[0-9]*' 2>/dev/null | wc -l | tr -d ' ')"
emit accelerator.rdma_devices "$(find /sys/class/infiniband -mindepth 1 -maxdepth 1 2>/dev/null | wc -l | tr -d ' ')"
emit accelerator.cpu_features "$(flags=$(awk -F: '/^(flags|Features)/ {print $2; exit}' /proc/cpuinfo 2>/dev/null); for feature in avx2 avx512f amx_tile sve; do printf '%s' "$flags" | grep -qw "$feature" && printf '%s ' "$feature"; done)"
emit ai_runtime.packages "$(python3 -c 'import sys; m=__import__("importlib.metadata",fromlist=["metadata"]) if sys.version_info >= (3,8) else __import__("importlib_metadata"); n=["torch","tensorflow","jax","vllm","transformers","deepspeed","onnxruntime","sglang","triton"]; d={str(x.metadata.get("Name","")).lower():x.version for x in m.distributions()}; print(", ".join(f"{x}={d[x]}" for x in n if x in d))' 2>/dev/null)"
emit ai_runtime.shm_kib "$(df -Pk /dev/shm 2>/dev/null | awk 'NR==2 {print $2}')"
emit ai_runtime.hugepages_total "$(awk '/^HugePages_Total:/ {print $2}' /proc/meminfo 2>/dev/null)"
emit ai_runtime.cuda_visible_devices "${CUDA_VISIBLE_DEVICES:-}"
emit ai_runtime.nvidia_visible_devices "${NVIDIA_VISIBLE_DEVICES:-}"
emit ai_runtime.rocr_visible_devices "${ROCR_VISIBLE_DEVICES:-}"
emit ai_runtime.ascend_visible_devices "${ASCEND_RT_VISIBLE_DEVICES:-}"
emit ai_runtime.musa_visible_devices "${MUSA_VISIBLE_DEVICES:-}"
emit ai_runtime.omp_num_threads "${OMP_NUM_THREADS:-}"
emit ai_runtime.nccl_debug "${NCCL_DEBUG:-}"
"#;

#[derive(Debug, Args)]
pub struct InspectArgs {
    /// SSH host configured in binport or ~/.ssh/config
    target: String,
    /// Only show these comma-separated sections
    #[arg(long, value_delimiter = ',')]
    section: Vec<String>,
}

#[derive(Debug, Args)]
pub struct DiffArgs {
    /// First SSH host
    left: String,
    /// Second SSH host
    right: String,
    /// Only compare these comma-separated sections
    #[arg(long, value_delimiter = ',')]
    section: Vec<String>,
    /// Include fields whose values are equal
    #[arg(long)]
    all: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EnvironmentSnapshot {
    host: String,
    values: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Debug, Serialize)]
struct Difference {
    section: String,
    field: String,
    left: Option<String>,
    right: Option<String>,
    equal: bool,
}

pub fn inspect(args: InspectArgs, use_password: bool, json: bool) -> io::Result<u8> {
    let password = prompt_password(use_password)?;
    let snapshot = runtime()?.block_on(collect(&args.target, password.as_deref()))?;
    let snapshot = filter(snapshot, &args.section);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&snapshot).map_err(io::Error::other)?
        );
    } else {
        println!("Environment: {}\n", snapshot.host);
        print!("{}", snapshot_table(&snapshot));
    }
    Ok(0)
}

pub fn diff(args: DiffArgs, use_password: bool, json: bool) -> io::Result<u8> {
    let password = prompt_password(use_password)?;
    let runtime = runtime()?;
    let (left, right) = runtime.block_on(async {
        tokio::try_join!(
            collect(&args.left, password.as_deref()),
            collect(&args.right, password.as_deref())
        )
    })?;
    let left = filter(left, &args.section);
    let right = filter(right, &args.section);
    let differences = compare(&left, &right, args.all);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "left": left.host, "right": right.host, "differences": differences
            }))
            .map_err(io::Error::other)?
        );
    } else {
        println!("Environment comparison: {} <-> {}\n", left.host, right.host);
        let rows = differences
            .iter()
            .map(|item| {
                vec![
                    item.section.clone(),
                    item.field.clone(),
                    item.left.as_deref().unwrap_or("-").to_owned(),
                    item.right.as_deref().unwrap_or("-").to_owned(),
                ]
            })
            .collect::<Vec<_>>();
        if rows.is_empty() {
            println!("No differences found");
        } else {
            print!(
                "{}",
                table::render(&["SECTION", "FIELD", &left.host, &right.host], &rows)
            );
            let changed = differences.iter().filter(|item| !item.equal).count();
            println!(
                "\nSummary: {changed} difference{}",
                if changed == 1 { "" } else { "s" }
            );
        }
    }
    Ok(0)
}

async fn collect(target: &str, password: Option<&str>) -> io::Result<EnvironmentSnapshot> {
    let (status, stdout, stderr) = capture_remote(target, PROBE.to_owned(), password).await?;
    if status != 0 {
        return Err(io::Error::other(format!(
            "environment probe failed on {target}: {}",
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    parse(target, &String::from_utf8_lossy(&stdout))
}

fn parse(host: &str, output: &str) -> io::Result<EnvironmentSnapshot> {
    let mut values: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for line in output.lines() {
        let Some((key, value)) = line.split_once('\t') else {
            continue;
        };
        let Some((section, field)) = key.split_once('.') else {
            continue;
        };
        values.entry(section.to_owned()).or_default().insert(
            field.to_owned(),
            if value.trim().is_empty() {
                "unavailable".into()
            } else {
                value.trim().into()
            },
        );
    }
    if values.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "environment probe returned no data",
        ));
    }
    Ok(EnvironmentSnapshot {
        host: host.to_owned(),
        values,
    })
}

fn filter(mut snapshot: EnvironmentSnapshot, sections: &[String]) -> EnvironmentSnapshot {
    if !sections.is_empty() {
        snapshot
            .values
            .retain(|key, _| sections.iter().any(|section| section == key));
    }
    snapshot
}

fn compare(left: &EnvironmentSnapshot, right: &EnvironmentSnapshot, all: bool) -> Vec<Difference> {
    let sections = left
        .values
        .keys()
        .chain(right.values.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut output = Vec::new();
    for section in sections {
        let empty = BTreeMap::new();
        let left_fields = left.values.get(&section).unwrap_or(&empty);
        let right_fields = right.values.get(&section).unwrap_or(&empty);
        let fields = left_fields
            .keys()
            .chain(right_fields.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        for field in fields {
            let l = left_fields.get(&field).cloned();
            let r = right_fields.get(&field).cloned();
            let equal = l == r;
            if all || !equal {
                output.push(Difference {
                    section: section.clone(),
                    field,
                    left: l,
                    right: r,
                    equal,
                });
            }
        }
    }
    output
}

fn snapshot_table(snapshot: &EnvironmentSnapshot) -> String {
    let rows = snapshot
        .values
        .iter()
        .flat_map(|(section, fields)| {
            fields
                .iter()
                .map(move |(field, value)| vec![section.clone(), field.clone(), value.clone()])
        })
        .collect::<Vec<_>>();
    table::render(&["SECTION", "FIELD", "VALUE"], &rows)
}

fn prompt_password(enabled: bool) -> io::Result<Option<String>> {
    enabled
        .then(|| rpassword::prompt_password("SSH password: "))
        .transpose()
}

fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Runtime::new().map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_compares_snapshots() {
        let a = parse("a", "system.os\tLinux A\nsystem.arch\tx86_64\n").unwrap();
        let b = parse("b", "system.os\tLinux B\nsystem.arch\tx86_64\n").unwrap();
        let differences = compare(&a, &b, false);
        assert_eq!(differences.len(), 1);
        assert_eq!(differences[0].field, "os");
    }

    #[test]
    fn filters_sections() {
        let snapshot = parse("a", "system.os\tLinux\nruntime.python\t3.12\n").unwrap();
        let snapshot = filter(snapshot, &["runtime".into()]);
        assert_eq!(snapshot.values.keys().collect::<Vec<_>>(), [&"runtime"]);
    }
}
