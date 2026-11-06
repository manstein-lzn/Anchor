//! Measure a private copy, never a live store. Appends one synthetic Write.
use anchor_kernel::*;
use std::{path::Path, sync::Arc, time::Instant};

struct Allow;
impl Authority for Allow {
    fn permits(&self, _: &Principal, _: Action, _: &str) -> bool {
        true
    }
}
struct KeepState;
impl Validator for KeepState {
    fn judge(&self, ctx: &JudgeCtx, _: &Submission) -> Judgement {
        Judgement::accept(ctx.resource.state.clone(), Verification::Asserted)
    }
}
fn counter(path: &str, key: &str) -> u64 {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .find_map(|line| {
            line.strip_prefix(key)?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
        .unwrap()
}
fn main() {
    let root = std::env::args().nth(1).expect("private store root");
    let id = std::env::args().nth(2).expect("resource id");
    assert!(Path::new(&root).is_dir());
    let store = FileStore::new(root).unwrap();
    let start = Instant::now();
    let initial = store.load(&id).unwrap().expect("resource");
    let cold_ms = start.elapsed().as_secs_f64() * 1000.0;
    let mut kernel = Kernel::new(store, Box::new(Allow)).unwrap();
    kernel
        .install(&initial.validator, Arc::new(KeepState))
        .unwrap();
    let start_bytes = counter("/proc/self/io", "rchar:");
    let start_rss = counter("/proc/self/status", "VmRSS:");
    let mut samples = Vec::new();
    for _ in 0..12 {
        let start = Instant::now();
        std::hint::black_box(kernel.load(&id).unwrap().unwrap());
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let warm_bytes = counter("/proc/self/io", "rchar:") - start_bytes;
    samples.sort_by(f64::total_cmp);
    let start_bytes = counter("/proc/self/io", "rchar:");
    let start = Instant::now();
    let verdict = kernel
        .submit(
            &Principal::new("perf-fixture", PrincipalKind::Code),
            0,
            Submission::new(id, initial.version, Action::Write),
        )
        .unwrap();
    assert!(verdict.is_accepted());
    println!(
        "cold_load_ms={cold_ms:.3} warm_load_p50_ms={:.3} warm_load_rchar={warm_bytes} submit_ms={:.3} submit_rchar={} rss_start_kib={start_rss} rss_end_kib={} hwm_kib={}",
        samples[6],
        start.elapsed().as_secs_f64() * 1000.0,
        counter("/proc/self/io", "rchar:") - start_bytes,
        counter("/proc/self/status", "VmRSS:"),
        counter("/proc/self/status", "VmHWM:")
    );
}
