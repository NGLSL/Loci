#![cfg(target_os = "linux")]
mod common;
use common::Fixture;
use std::fs;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[test]
fn idle_watch_handles_signals_without_a_file_event_and_releases_writer_lock() {
    for signal in ["-INT", "-TERM", "EOF"] {
        let f = Fixture::new();
        fs::write(f.root.join("saved.txt"), "").unwrap();
        let db = f.base.join("state.loci");
        let mut child = Command::new(env!("CARGO_BIN_EXE_loci-experiment"))
            .args(["engine", "watch"])
            .arg(&f.root)
            .arg(&db)
            .arg("--scale")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (tx, rx) = mpsc::channel();
        let stderr = child.stderr.take().unwrap();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let _ = tx.send(line.unwrap());
            }
        });
        loop {
            if rx
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .contains("watch-ready")
            {
                break;
            }
        }
        if signal == "EOF" {
            drop(child.stdin.take());
        } else {
            assert!(Command::new("kill")
                .arg(signal)
                .arg(child.id().to_string())
                .status()
                .unwrap()
                .success());
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "signal shutdown blocked");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(
            status.success(),
            "signal must request orderly shutdown: {status}"
        );
        reader.join().unwrap();
        assert!(rx.try_iter().any(|line| line.contains("state=Stopped")));
        let mut reopened = loci_experiment::engine::Engine::open_with_options(
            &f.root,
            Some(&db),
            loci_experiment::engine::EngineOptions::scale(),
        )
        .unwrap();
        reopened.stop().unwrap();
    }
}

#[test]
fn blocked_export_keeps_monitoring_and_stop_cancels_output_without_a_reader() {
    use std::io::Write;
    for close in ["stop", "EOF", "SIGTERM"] {
        let f = Fixture::new();
        for n in 0..600 {
            fs::write(f.root.join(format!("entry-{n:04}-{}", "x".repeat(100))), "").unwrap();
        }
        let db = f.base.join("state.loci");
        let mut child = Command::new(env!("CARGO_BIN_EXE_loci-experiment"))
            .args(["engine", "watch"])
            .arg(&f.root)
            .arg(&db)
            .args(["--scale", "--null"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (tx, rx) = mpsc::channel();
        let stderr = child.stderr.take().unwrap();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let _ = tx.send(line.unwrap());
            }
        });
        loop {
            if rx
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .contains("watch-ready")
            {
                break;
            }
        }
        let mut input = child.stdin.take().unwrap();
        writeln!(input, "export").unwrap();
        loop {
            if rx
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .contains("command=export,started=true")
            {
                break;
            }
        }
        // Nobody reads stdout: this export exceeds a pipe buffer and must remain cancellable.
        std::thread::sleep(Duration::from_millis(100));
        writeln!(input, "export\nstatus").unwrap();
        loop {
            if rx
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .contains("command=export,ok=false")
            {
                break;
            }
        }
        loop {
            if rx
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .contains("command=status,ok=true")
            {
                break;
            }
        }
        fs::write(f.root.join("new-visible.txt"), "").unwrap();
        loop {
            if rx
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .contains("version=2,state=Validated")
            {
                break;
            }
        }
        match close {
            "stop" => writeln!(input, "stop").unwrap(),
            "EOF" => drop(input),
            _ => {
                assert!(Command::new("kill")
                    .arg("-TERM")
                    .arg(child.id().to_string())
                    .status()
                    .unwrap()
                    .success());
            }
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                reader.join().unwrap();
                panic!("blocked export prevented shutdown");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(status.success());
        reader.join().unwrap();
        assert!(rx.try_iter().any(|line| line.contains("joined=true")));
        let mut reopened = loci_experiment::engine::Engine::open_with_options(
            &f.root,
            Some(&db),
            loci_experiment::engine::EngineOptions::scale(),
        )
        .unwrap();
        assert_eq!(
            reopened
                .query()
                .lease()
                .unwrap()
                .page(
                    "new-visible",
                    None,
                    50,
                    &std::sync::atomic::AtomicBool::new(false),
                    &std::sync::atomic::AtomicUsize::new(0)
                )
                .unwrap()
                .paths,
            [f.root.join("new-visible.txt")]
        );
        reopened.stop().unwrap();
    }
}

#[test]
fn eof_during_stale_correction_reports_unsaved_and_preserves_last_checkpoint() {
    let f = Fixture::new();
    fs::write(f.root.join("saved.txt"), "").unwrap();
    let db = f.base.join("state.loci");
    let mut engine = loci_experiment::engine::Engine::open_with_options(
        &f.root,
        Some(&db),
        loci_experiment::engine::EngineOptions::scale(),
    )
    .unwrap();
    engine.save().unwrap();
    engine.stop().unwrap();
    let prior = fs::read(&db).unwrap();
    for n in 0..100 {
        fs::write(f.root.join(format!("offline-{n}")), "").unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_loci-experiment"))
        .args(["engine", "watch"])
        .arg(&f.root)
        .arg(&db)
        .args(["--scale", "--scan-batch", "1"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    let logs = String::from_utf8(output.stderr).unwrap();
    assert!(logs.contains("shutdown_saved=false,unsaved=true"), "{logs}");
    assert!(logs.contains("state=Stopped,joined=true"), "{logs}");
    assert_eq!(fs::read(&db).unwrap(), prior);
    let mut reopened = loci_experiment::engine::Engine::open_with_options(
        &f.root,
        Some(&db),
        loci_experiment::engine::EngineOptions::scale(),
    )
    .unwrap();
    reopened.stop().unwrap();
}

#[test]
fn independent_count_and_sort_follow_an_immediate_watch_query() {
    use std::io::{Read, Write};
    let f = Fixture::new();
    for name in ["z.txt", "b.txt", "A.txt"] {
        fs::write(f.root.join(name), "").unwrap();
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_loci-experiment"))
        .args(["engine", "watch"])
        .arg(&f.root)
        .arg(f.base.join("db"))
        .args(["--scale", "--null"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (tx, rx) = mpsc::channel();
    let stderr = child.stderr.take().unwrap();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            tx.send(line.unwrap()).unwrap();
        }
    });
    let mut logs = Vec::new();
    let wait = |needle: &str, logs: &mut Vec<String>| loop {
        let line = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let found = line.contains(needle);
        logs.push(line);
        if found {
            break;
        }
    };
    wait("watch-ready", &mut logs);
    let mut input = child.stdin.take().unwrap();
    writeln!(input, "query ext:txt").unwrap();
    wait("command=query,ok=true", &mut logs);
    writeln!(input, "count ext:txt").unwrap();
    wait("command=count,ok=true", &mut logs);
    assert!(logs
        .iter()
        .any(|line| line.contains("job=count,state=Complete") && line.contains("matches=3")));
    writeln!(input, "sort ext:txt").unwrap();
    wait("command=sort,ok=true", &mut logs);
    writeln!(input, "stop").unwrap();
    drop(input);
    let mut bytes = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(child.wait().unwrap().success());
    reader.join().unwrap();
    let mut sorted = Vec::new();
    for name in ["A.txt", "b.txt", "z.txt"] {
        sorted.extend_from_slice(f.root.join(name).as_os_str().as_encoded_bytes());
        sorted.push(0);
    }
    assert!(bytes.ends_with(&sorted));
}
