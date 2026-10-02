#![cfg(target_os = "linux")]

use loci_experiment::engine::{Engine, EngineOptions, Status};
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::{Duration, Instant};

// Opt-in performance regression at the public production-owner seam. The
// foundation controller separately verifies full byte/kind oracles and actual
// worker RSS, so this test does not substitute for its acceptance evidence.
#[test]
#[ignore = "requires LOCI_REAL_MILLION_ROOT; release-mode actual 1M fixture and 20,800 queries"]
fn every_reference_query_meets_its_own_50ms_p95_on_a_real_million_entry_owner() {
    let root = std::env::var_os("LOCI_REAL_MILLION_ROOT").expect("explicit real fixture root");
    let mut owner =
        Engine::open_with_options(std::path::Path::new(&root), None, EngineOptions::scale())
            .unwrap()
            .spawn()
            .unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    while owner.view().status != Status::Validated {
        assert!(Instant::now() < deadline, "{:?}", owner.view());
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(owner.view().resources.inventory_slots, 1_000_001);
    assert_eq!(owner.view().resources.session_watches, 20_001);
    let queries: Vec<_> = include_str!("../tools/linux-million-acceptance/queries.txt")
        .lines()
        .collect();
    assert_eq!(queries.len(), 52);
    let handle = owner.query();
    let query = |raw: &str| {
        let started = Instant::now();
        let lease = handle.lease().unwrap();
        let page = lease
            .page(raw, None, 50, &AtomicBool::new(false), &AtomicUsize::new(0))
            .unwrap();
        let elapsed = started.elapsed();
        assert!(page.validated_at_start_and_finish && !page.cancelled);
        if ["rr", "ia", "ii"].contains(&raw) {
            assert!(page.complete && page.paths.is_empty());
        }
        elapsed
    };
    for raw in &queries {
        for _ in 0..5 {
            query(raw);
        }
    }
    let mut failures = Vec::new();
    for pass in 0..2 {
        let mut timings = vec![Vec::new(); queries.len()];
        for repetition in 0..200 {
            for step in 0..queries.len() {
                let index = (step + repetition * 7 + pass * 11) % queries.len();
                let elapsed = query(queries[index]);
                println!(
                    "sample pass={pass} repetition={repetition} query_id={index} elapsed_ns={}",
                    elapsed.as_nanos()
                );
                timings[index].push(elapsed);
            }
        }
        for (raw, samples) in queries.iter().zip(&mut timings) {
            samples.sort();
            let p95 = samples[189];
            println!(
                "summary pass={pass} query={raw:?} p95_ns={}",
                p95.as_nanos()
            );
            if p95 > Duration::from_millis(50) {
                failures.push((pass, raw.to_string(), p95));
            }
        }
    }
    owner.stop(Duration::from_secs(2)).unwrap();
    assert!(failures.is_empty(), "per-query p95 failures: {failures:?}");
}
