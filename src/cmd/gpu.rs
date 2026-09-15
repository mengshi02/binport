use super::native_exec::capture_remote;
use std::collections::BTreeMap;
use std::io;

const GPU_P2P_BENCHMARK: &str = r#"import concurrent.futures, ctypes, ctypes.util, glob, os, shutil, subprocess, threading, time
hip_library = ctypes.util.find_library("amdhip64")
hygon_smi = shutil.which("hy-smi") or next((p for p in (
    "/opt/hyhal/bin/hy-smi", "/opt/dtk/bin/hy-smi", "/opt/hygondtk/bin/hy-smi"
) if os.access(p, os.X_OK)), None)
if not hip_library:
    hip_library = next((p for pattern in (
        "/opt/dtk*/lib/libamdhip64.so*",
        "/opt/dtk*/lib/*/libamdhip64.so*",
        "/opt/dtk*/hip/lib/libamdhip64.so*",
        "/opt/dtk*/hip/lib/*/libamdhip64.so*",
        "/opt/dtk*/lib64/libamdhip64.so*",
        "/opt/hygondtk*/lib/libamdhip64.so*",
        "/opt/hygondtk*/lib/*/libamdhip64.so*",
        "/opt/hygondtk*/hip/lib/libamdhip64.so*",
        "/opt/hyhal/lib/libamdhip64.so*",
    ) for p in glob.glob(pattern) if os.path.isfile(p)), None)
if shutil.which("nvidia-smi"):
    vendor, prefix, runtime_api = "NVIDIA", "cu", False
    library = ctypes.util.find_library("cuda") or "libcuda.so.1"
    topology_command = ["nvidia-smi", "topo", "-m"]
elif shutil.which("mthreads-gmi"):
    vendor, prefix, runtime_api = "Moore Threads", "mu", False
    library = ctypes.util.find_library("musa") or "libmusa.so.1"
    topology_command = ["mthreads-gmi", "topo", "-mg"]
elif hygon_smi or hip_library:
    vendor, prefix, runtime_api = "Hygon DCU", "hip", True
    if not hip_library:
        raise RuntimeError("Hygon DCU detected, but libamdhip64.so was not found under the DTK installation")
    library = hip_library
    topology_command = []
else:
    raise RuntimeError("no supported NVIDIA, Moore Threads, or Hygon accelerator was detected")
driver = ctypes.CDLL(library)

def call(name, *args):
    code = getattr(driver, prefix + name)(*args)
    if code != 0:
        raise RuntimeError(f"{prefix + name} failed with driver error {code}")

count = ctypes.c_int()
if runtime_api:
    call("Init", 0)
    call("GetDeviceCount", ctypes.byref(count))
else:
    call("Init", 0)
    call("DeviceGetCount", ctypes.byref(count))
if count.value < 2:
    raise RuntimeError("at least two accelerator devices are required")

contexts = []
devices = []
for gpu in range(count.value):
    if runtime_api:
        devices.append(gpu)
        contexts.append(gpu)
    else:
        device = ctypes.c_int()
        context = ctypes.c_void_p()
        call("DeviceGet", ctypes.byref(device), gpu)
        call("DevicePrimaryCtxRetain", ctypes.byref(context), device)
        devices.append(device)
        contexts.append(context)

paths = {}
try:
    topo = subprocess.check_output(topology_command, text=True, stderr=subprocess.DEVNULL) if topology_command else ""
    for line in topo.splitlines():
        columns = line.split()
        if columns and columns[0].startswith("GPU") and columns[0][3:].isdigit():
            left = int(columns[0][3:])
            for right in range(count.value):
                if right + 1 < len(columns) and columns[right + 1] != "X":
                    paths[(left, right)] = columns[right + 1]
except Exception:
    pass

if runtime_api:
    def properties(path):
        result = {}
        try:
            with open(path) as stream:
                for line in stream:
                    columns = line.split()
                    if len(columns) >= 2:
                        result[columns[0]] = columns[1]
        except OSError:
            pass
        return result

    kfd_nodes = []
    for node_path in glob.glob("/sys/class/kfd/kfd/topology/nodes/[0-9]*"):
        props = properties(os.path.join(node_path, "properties"))
        try:
            with open(os.path.join(node_path, "gpu_id")) as stream:
                gpu_id = int(stream.read().strip())
        except (OSError, ValueError):
            gpu_id = int(props.get("gpu_id", "0"))
        if gpu_id != 0:
            kfd_nodes.append((int(props.get("location_id", "0")), int(os.path.basename(node_path))))
    kfd_nodes.sort()
    node_to_gpu = {node: gpu for gpu, (_, node) in enumerate(kfd_nodes)}
    kfd_routes = {}
    for link_path in glob.glob("/sys/class/kfd/kfd/topology/nodes/*/io_links/*/properties"):
        props = properties(link_path)
        if props.get("type") != "11":
            continue
        source = node_to_gpu.get(int(props.get("node_from", "-1")))
        destination = node_to_gpu.get(int(props.get("node_to", "-1")))
        if source is not None and destination is not None:
            kfd_routes[(source, destination)] = kfd_routes.get((source, destination), 0) + 1
    for source, destination in kfd_routes:
        routes = kfd_routes[(source, destination)]
        reverse = kfd_routes.get((destination, source), 0)
        route_text = str(routes) if routes == reverse else f"{routes}/{reverse} forward/reverse"
        paths[(source, destination)] = f"XGMI · {route_text} KFD route(s) · physical link count unavailable"

size = 256 * 1024 * 1024
iterations = 8

def select(context):
    call("SetDevice" if runtime_api else "CtxSetCurrent", context)

def allocate(context):
    select(context)
    pointer = ctypes.c_void_p() if runtime_api else ctypes.c_uint64()
    call("Malloc" if runtime_api else "MemAlloc_v2", ctypes.byref(pointer), ctypes.c_size_t(size))
    return pointer

def free(context, pointer):
    select(context)
    call("Free" if runtime_api else "MemFree_v2", pointer)

def peer_access(source, destination):
    can_access = ctypes.c_int()
    call("DeviceCanAccessPeer", ctypes.byref(can_access), devices[destination], devices[source])
    return bool(can_access.value)

def bandwidth(source, destination, barrier=None):
    if not peer_access(source, destination):
        return None
    src_ctx, dst_ctx = contexts[source], contexts[destination]
    src = allocate(src_ctx)
    dst = allocate(dst_ctx)
    try:
        select(dst_ctx)
        enable = "DeviceEnablePeerAccess" if runtime_api else "CtxEnablePeerAccess"
        code = getattr(driver, prefix + enable)(src_ctx, 0)
        if code not in (0, 704):
            raise RuntimeError(f"{prefix + enable} failed with driver error {code}")
        for _ in range(2):
            call("MemcpyPeer", dst, destination, src, source, ctypes.c_size_t(size)) if runtime_api else call("MemcpyPeer", dst, dst_ctx, src, src_ctx, ctypes.c_size_t(size))
        call("DeviceSynchronize" if runtime_api else "CtxSynchronize")
        if barrier:
            barrier.wait(timeout=60)
        started = time.perf_counter()
        for _ in range(iterations):
            call("MemcpyPeer", dst, destination, src, source, ctypes.c_size_t(size)) if runtime_api else call("MemcpyPeer", dst, dst_ctx, src, src_ctx, ctypes.c_size_t(size))
        call("DeviceSynchronize" if runtime_api else "CtxSynchronize")
        elapsed = time.perf_counter() - started
        return size * iterations / elapsed / 1e9, elapsed
    finally:
        free(dst_ctx, dst)
        free(src_ctx, src)

values = []
print(f"driver_api\t{vendor} {prefix}* {'Runtime' if runtime_api else 'Driver'} API")
for left in range(count.value):
    for right in range(left + 1, count.value):
        forward = bandwidth(left, right)
        reverse = bandwidth(right, left)
        if forward is None or reverse is None:
            print(f"pair_{left}_{right}\tunavailable (P2P disabled)")
        else:
            forward_rate, _ = forward
            reverse_rate, _ = reverse
            values.extend((forward_rate, reverse_rate))
            path = paths.get((left, right), "HIP P2P (physical route undetermined)" if runtime_api else "P2P path undetermined")
            print(f"pair_{left}_{right}\t{path} · {forward_rate:.2f} / {reverse_rate:.2f} GB/s (forward / reverse)")
if values:
    print(f"summary\t{len(values)//2} pairs · min {min(values):.2f} · avg {sum(values)/len(values):.2f} · max {max(values):.2f} GB/s")

def concurrent_measure(pairs):
    pairs = [(source, destination) for source, destination in pairs if peer_access(source, destination)]
    if not pairs:
        return None
    if runtime_api:
        transfers = []
        try:
            for source, destination in pairs:
                src_ctx, dst_ctx = contexts[source], contexts[destination]
                src, dst = allocate(src_ctx), allocate(dst_ctx)
                select(dst_ctx)
                stream = ctypes.c_void_p()
                call("StreamCreate", ctypes.byref(stream))
                transfers.append((source, destination, src_ctx, dst_ctx, src, dst, stream))
            for source, destination, _, _, src, dst, stream in transfers:
                call("MemcpyPeerAsync", dst, destination, src, source, ctypes.c_size_t(size), stream)
            for *_, stream in transfers:
                call("StreamSynchronize", stream)
            started = time.perf_counter()
            for _ in range(iterations):
                for source, destination, _, _, src, dst, stream in transfers:
                    call("MemcpyPeerAsync", dst, destination, src, source, ctypes.c_size_t(size), stream)
            for *_, stream in transfers:
                call("StreamSynchronize", stream)
            elapsed = time.perf_counter() - started
            aggregate = len(transfers) * size * iterations / elapsed / 1e9
            return len(transfers), aggregate, aggregate / len(transfers), None
        finally:
            for _, _, src_ctx, dst_ctx, src, dst, stream in transfers:
                select(dst_ctx)
                call("StreamDestroy", stream)
                free(dst_ctx, dst)
                free(src_ctx, src)
    barrier = threading.Barrier(len(pairs))
    with concurrent.futures.ThreadPoolExecutor(max_workers=len(pairs)) as executor:
        results = list(executor.map(lambda pair: bandwidth(pair[0], pair[1], barrier), pairs))
    rates = [result[0] for result in results if result]
    elapsed = max(result[1] for result in results if result)
    aggregate = len(rates) * size * iterations / elapsed / 1e9
    return len(rates), aggregate, sum(rates) / len(rates), min(rates)

def concurrent_bandwidth(name, pairs):
    result = concurrent_measure(pairs)
    if not result:
        print(f"concurrent_{name}\tunavailable (no supported P2P streams)")
        return
    streams, aggregate, average, minimum = result
    minimum_text = f" · min {minimum:.2f}" if minimum is not None else ""
    print(f"concurrent_{name}\t{streams} streams · aggregate {aggregate:.2f} GB/s · avg {average:.2f} GB/s/stream{minimum_text}")

concurrent_bandwidth("disjoint_pairs", [(gpu, gpu + 1) for gpu in range(0, count.value - 1, 2)])
concurrent_bandwidth("one_to_all", [(0, gpu) for gpu in range(1, count.value)])
rounds = [concurrent_measure([(source, (source + offset) % count.value) for source in range(count.value)]) for offset in range(1, count.value)]
rounds = [result for result in rounds if result]
if rounds:
    aggregates = [result[1] for result in rounds]
    print(f"concurrent_all_to_all\t{count.value * (count.value - 1)} logical streams in {len(rounds)} rounds · {count.value} concurrent/round · avg aggregate {sum(aggregates)/len(aggregates):.2f} GB/s · min/max {min(aggregates):.2f}/{max(aggregates):.2f}")
else:
    print("concurrent_all_to_all\tunavailable (no supported P2P streams)")
"#;

pub async fn measure_gpu_p2p(
    target: &str,
    password: Option<&str>,
) -> io::Result<BTreeMap<String, String>> {
    let command = binport::execute_command("python3", &["-c".into(), GPU_P2P_BENCHMARK.into()])?;
    let (status, stdout, stderr) = tokio::time::timeout(
        std::time::Duration::from_secs(180),
        capture_remote(target, command, password),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "GPU bandwidth test timed out"))??;
    if status != 0 {
        return Err(io::Error::other(format!(
            "GPU bandwidth test failed on {target}: {}",
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    let metrics = parse_metrics(&String::from_utf8_lossy(&stdout));
    if metrics.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "GPU bandwidth test returned no measurements",
        ));
    }
    Ok(metrics)
}

fn parse_metrics(output: &str) -> BTreeMap<String, String> {
    output
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pair_paths_and_summary() {
        let metrics = parse_metrics(
            "pair_0_1\tNV18 · 390.90 / 390.41 GB/s (forward / reverse)\nsummary\t1 pairs · min 390.41 GB/s\n",
        );
        assert!(metrics["pair_0_1"].starts_with("NV18"));
        assert!(metrics["summary"].contains("390.41"));
    }
}
