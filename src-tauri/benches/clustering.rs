//! Throughput benchmark for the clustering pipeline.
//!
//! Run with: `cargo bench --bench clustering`
//!
//! Targets: 1k captures clusters in <5ms, 10k in <50ms. The full
//! v1 working set is ~300 captures per cycle, so these are
//! comfortable performance budgets.

use agentscout::analysis::{cluster_captures, ClusterConfig};
use agentscout::storage::CaptureRow;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

fn synth_captures(n: usize) -> Vec<CaptureRow> {
    let apps = ["Code.exe", "chrome.exe", "wezterm-gui.exe", "Slack.exe"];
    let titles = [
        "main.rs - my-cli - Visual Studio Code",
        "Pull requests · GitHub",
        "~/dev/my-cli — wezterm",
        "#engineering - Acme",
    ];
    (0..n)
        .map(|i| CaptureRow {
            id: i as i64,
            timestamp: 1_700_000_000 + (i as i64) * 300,
            cycle_id: "bench".into(),
            // 4-cycle rotation so the benchmark exercises real cluster splits.
            foreground_app: Some(apps[i % apps.len()].into()),
            foreground_window_title: Some(titles[i % titles.len()].into()),
            image_path: format!("/tmp/{i}.enc"),
            ocr_text: None,
            thumbnail_path: None,
            ocr_engine: None,
        })
        .collect()
}

fn bench_clustering(c: &mut Criterion) {
    let mut group = c.benchmark_group("cluster_captures");
    for n in [100, 1_000, 10_000].iter() {
        let captures = synth_captures(*n);
        group.throughput(Throughput::Elements(*n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &captures, |b, caps| {
            b.iter(|| cluster_captures(caps, ClusterConfig::default()));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_clustering);
criterion_main!(benches);
