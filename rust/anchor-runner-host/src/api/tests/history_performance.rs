//! HTTP behavior and process I/O against snapshot-heavy, isolated history.
use super::*;
use anchor_kernel::{FileStore, Store};
use std::collections::HashSet;
use std::io::Write;
use std::thread::{self, ThreadId};
use std::time::Instant;

#[derive(serde::Serialize)]
struct ProcessSample {
    rss_kib: u64,
    hwm_kib: u64,
    rchar: u64,
    glibc: Value,
}

fn allocation_stats() -> Value {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // glibc >= 2.33. These counters include allocator metadata and tcache;
        // allocated bytes are not a measurement of live Rust object sizes.
        #[repr(C)]
        struct Mallinfo2 {
            arena: usize,
            ordblks: usize,
            smblks: usize,
            hblks: usize,
            hblkhd: usize,
            usmblks: usize,
            fsmblks: usize,
            uordblks: usize,
            fordblks: usize,
            keepcost: usize,
        }
        unsafe extern "C" {
            fn mallinfo2() -> Mallinfo2;
        }
        // SAFETY: mallinfo2 has no arguments; the structure matches glibc's ABI.
        let stats = unsafe { mallinfo2() };
        json!({
            "arena_bytes": stats.arena,
            "allocated_bytes": stats.uordblks,
            "free_bytes": stats.fordblks,
            "mmap_bytes": stats.hblkhd,
            "mmap_regions": stats.hblks,
            "free_chunks": stats.ordblks,
            "main_arena_releasable_top_bytes": stats.keepcost
        })
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    {
        Value::Null
    }
}

fn memory_and_reads() -> ProcessSample {
    fn field(contents: &str, name: &str) -> u64 {
        contents
            .lines()
            .find_map(|line| {
                line.strip_prefix(name)?
                    .split_whitespace()
                    .next()?
                    .parse()
                    .ok()
            })
            .unwrap()
    }
    let glibc = allocation_stats();
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let io = std::fs::read_to_string("/proc/self/io").unwrap();
    ProcessSample {
        rss_kib: field(&status, "VmRSS:"),
        hwm_kib: field(&status, "VmHWM:"),
        rchar: field(&io, "rchar:"),
        glibc,
    }
}

async fn query_batch(
    router: &Router,
    baseline: &Arc<(Value, Value)>,
) -> (Vec<f64>, HashSet<ThreadId>) {
    let ready = Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let router = router.clone();
        let baseline = Arc::clone(baseline);
        let ready = Arc::clone(&ready);
        tasks.spawn(async move {
            ready.wait().await;
            let started = Instant::now();
            let list_worker = thread::current().id();
            let (status, listed) = call(router.clone(), "GET", "/runs", None).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(listed, baseline.0);
            let timeline_worker = thread::current().id();
            let (status, timeline) = call(router, "GET", "/timeline?days=30", None).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(timeline, baseline.1);
            (
                started.elapsed().as_secs_f64() * 1000.0,
                [list_worker, timeline_worker, thread::current().id()],
            )
        });
    }
    let mut samples = Vec::with_capacity(8);
    let mut workers = HashSet::new();
    while let Some(result) = tasks.join_next().await {
        let (elapsed_ms, observed) = result.unwrap();
        samples.push(elapsed_ms);
        workers.extend(observed);
    }
    (samples, workers)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 32)]
#[ignore = "process-wide I/O benchmark; run alone with --ignored --exact --nocapture"]
async fn repeated_run_and_timeline_queries_do_not_reread_snapshot_history() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let source = std::env::var_os("ANCHOR_PERF_HISTORY_COPY").map(PathBuf::from);
    let copied_resource: Option<anchor_kernel::Resource> = source.as_ref().map(|path| {
        serde_json::from_slice(&std::fs::read(path.join("resource.json")).unwrap()).unwrap()
    });
    let id = copied_resource
        .as_ref()
        .map(|r| r.id.strip_prefix("run/").unwrap())
        .unwrap_or("history-perf");
    fixtures::create(&state.application, &fixtures::work_record(id));
    let metadata = RunMetadata::new(
        id.into(),
        "fixture".into(),
        fixtures::record(&state.application, id).graph_digest,
        &state.bundle_root.join("graph.json"),
    )
    .unwrap();
    crate::run_metadata::save(&state.data_root, &metadata).unwrap();
    let kernel_root = state.data_root.join("kernel-runs");
    let resource_dir = kernel_root.join(format!("run%2F{id}"));
    let path = resource_dir.join("history.jsonl");
    // Explicit local measurement may substitute a private copy. Otherwise
    // generate the same growing-snapshot shape without any conversation data.
    if let Some(source) = &source {
        std::fs::copy(source.join("history.jsonl"), &path).unwrap();
        std::fs::copy(
            source.join("resource.json"),
            resource_dir.join("resource.json"),
        )
        .unwrap();
    } else {
        let mut resource = FileStore::new(&kernel_root)
            .unwrap()
            .load(&format!("run/{id}"))
            .unwrap()
            .unwrap();
        let mut file = std::fs::File::create(&path).unwrap();
        for version in 1..=256 {
            resource.version = version;
            resource.state["input"] = json!({"padding":"x".repeat(version as usize * 2048)});
            let record = json!({"stamp":{"principal":{"id":"anchor/executor","kind":"agent"},"cost":0},
                "submission":{"target":resource.id,"seen":version-1,"action":"write"},
                "verdict":{"effect":"accepted","verification":"asserted","reason":"ok"},"after":resource});
            serde_json::to_writer(&mut file, &record).unwrap();
            file.write_all(b"\n").unwrap();
        }
        std::fs::write(
            resource_dir.join("resource.json"),
            serde_json::to_vec(&resource).unwrap(),
        )
        .unwrap();
    }
    let history_bytes = std::fs::metadata(&path).unwrap().len();
    let resource_bytes = std::fs::metadata(resource_dir.join("resource.json"))
        .unwrap()
        .len();
    let router = router(state);
    let before = memory_and_reads();
    let started = Instant::now();
    let (status, first) = call(router.clone(), "GET", "/runs", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["runs"].as_array().unwrap().len(), 1);
    let cold = started.elapsed();
    let (status, timeline) = call(router.clone(), "GET", "/timeline?days=30", None).await;
    assert_eq!(status, StatusCode::OK);
    // Responses are compared within this process. Cross-process baseline_json
    // comparisons must normalize started/updated timestamps first.
    let baseline = Arc::new((first, timeline));
    let cold_end = memory_and_reads();
    let mut workers = HashSet::new();
    let mut warm_samples = Vec::with_capacity(16);
    let mut batches = Vec::with_capacity(8);
    for batch in 1..=2 {
        let (samples, observed) = query_batch(&router, &baseline).await;
        let worker_count = observed.len();
        workers.extend(observed);
        warm_samples.extend(samples);
        batches.push(json!({
            "stage": "warmup", "pairs_completed": batch * 8,
            "observed_worker_threads": worker_count, "process": memory_and_reads()
        }));
    }
    let warm_start = memory_and_reads();
    let mut samples = Vec::with_capacity(48);
    // Batch-boundary sampling observes retained memory after responses drop.
    // The timer excludes fixture construction, cold requests and warmup.
    let started = Instant::now();
    for batch in 3..=8 {
        let (batch_samples, observed) = query_batch(&router, &baseline).await;
        let worker_count = observed.len();
        workers.extend(observed);
        samples.extend(batch_samples);
        batches.push(json!({
            "stage": "measured", "pairs_completed": batch * 8,
            "observed_worker_threads": worker_count, "process": memory_and_reads()
        }));
    }
    let elapsed = started.elapsed().as_secs_f64();
    let after = memory_and_reads();
    let warm_rchar = after.rchar - warm_start.rchar;
    let mut ranked = samples.clone();
    ranked.sort_by(f64::total_cmp);
    // Nearest-rank percentiles over individual pairs, not batch averages.
    let percentile = |percent: usize| ranked[(ranked.len() * percent).div_ceil(100) - 1];
    println!(
        "{}",
        json!({
            "benchmark": "repeated_run_and_timeline_queries_do_not_reread_snapshot_history",
            "fixture": if source.is_some() { "history_copy" } else { "synthetic" },
            "malloc_arena_max": std::env::var("MALLOC_ARENA_MAX").ok(),
            "glibc_tunables": std::env::var("GLIBC_TUNABLES").ok(),
            "runtime_worker_threads": 32,
            "observed_worker_threads": workers.len(),
            "concurrency": 8,
            "total_pairs": 64,
            "warmup_pairs": 16,
            "measured_pairs": samples.len(),
            "history_bytes": history_bytes,
            "resource_bytes": resource_bytes,
            "cold_runs_ms": cold.as_secs_f64() * 1000.0,
            "pair_p50_ms": percentile(50),
            "pair_p95_ms": percentile(95),
            "pair_latency_ms": samples,
            "warmup_pair_latency_ms": warm_samples,
            "elapsed_seconds": elapsed,
            "pairs_per_second": ranked.len() as f64 / elapsed,
            "rss_before_kib": before.rss_kib,
            "rss_warm_kib": warm_start.rss_kib,
            "rss_after_kib": after.rss_kib,
            "hwm_kib": after.hwm_kib,
            "warm_rchar": warm_rchar,
            "process_before": before,
            "process_cold": cold_end,
            "process_warm": warm_start,
            "process_after": after,
            "batches": batches,
            "baseline_json": {"runs": baseline.0, "timeline": baseline.1}
        })
    );
    // Current state must still be read. Whole snapshot histories must not be.
    assert!(
        warm_rchar < history_bytes,
        "warm polling reread the history"
    );
}
