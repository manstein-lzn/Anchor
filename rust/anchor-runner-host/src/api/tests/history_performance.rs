//! HTTP behavior and process I/O against snapshot-heavy, isolated history.
use super::*;
use anchor_kernel::{FileStore, Store};
use std::io::Write;
use std::time::Instant;

fn memory_and_reads() -> (u64, u64, u64) {
    fn field(path: &str, name: &str) -> u64 {
        std::fs::read_to_string(path)
            .unwrap()
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
    (
        field("/proc/self/status", "VmRSS:"),
        field("/proc/self/status", "VmHWM:"),
        field("/proc/self/io", "rchar:"),
    )
}

#[tokio::test]
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
    let router = router(state);
    let before = memory_and_reads();
    let started = Instant::now();
    let (status, first) = call(router.clone(), "GET", "/runs", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["runs"].as_array().unwrap().len(), 1);
    let cold = started.elapsed();
    let warm_start = memory_and_reads();
    let mut samples = Vec::new();
    for _ in 0..12 {
        let start = Instant::now();
        let (status, listed) = call(router.clone(), "GET", "/runs", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listed, first);
        let (status, _) = call(router.clone(), "GET", "/timeline?days=30", None).await;
        assert_eq!(status, StatusCode::OK);
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(f64::total_cmp);
    let after = memory_and_reads();
    println!(
        "history_bytes={history_bytes} cold_runs_ms={:.3} pair_p50_ms={:.3} pair_p95_ms={:.3} rss_before_kib={} rss_warm_kib={} rss_after_kib={} hwm_kib={} warm_rchar={}",
        cold.as_secs_f64() * 1000.0,
        samples[6],
        samples[11],
        before.0,
        warm_start.0,
        after.0,
        after.1,
        after.2 - warm_start.2
    );
    // Current state must still be read. Whole snapshot histories must not be.
    assert!(
        after.2 - warm_start.2 < history_bytes,
        "warm polling reread the history"
    );
}
