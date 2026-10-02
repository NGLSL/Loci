#![cfg(target_os = "linux")]
mod common;
use common::Fixture;
use std::fs;
use std::process::{Command, Output};

fn cli(f: &Fixture, action: &str, query: Option<&str>) -> Output {
    let db = f.base.join("state.loci");
    let mut command = Command::new(env!("CARGO_BIN_EXE_loci-experiment"));
    command.args(["engine", action]).arg(&f.root).arg(db);
    if let Some(query) = query {
        command.arg(query).arg("--null");
    }
    command.output().unwrap()
}

#[test]
fn real_cli_builds_queries_and_reopens_an_explicit_root() {
    let f = Fixture::new();
    fs::write(f.root.join("中文 Report One.RS"), "x").unwrap();
    fs::write(f.root.join("中文 Report Two.txt"), "x").unwrap();
    let build = cli(&f, "build", None);
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(build.stdout.is_empty());
    assert!(f.base.join("state.loci").is_file());
    let query = cli(&f, "query", Some("中文 report ext:rs"));
    assert!(
        query.status.success(),
        "{}",
        String::from_utf8_lossy(&query.stderr)
    );
    let mut expected = f
        .root
        .join("中文 Report One.RS")
        .as_os_str()
        .as_encoded_bytes()
        .to_vec();
    expected.push(0);
    assert_eq!(query.stdout, expected);
    let status = cli(&f, "status", None);
    assert!(status.status.success());
    assert!(status.stdout.is_empty());
    let diagnostic = String::from_utf8(status.stderr).unwrap();
    assert!(diagnostic.contains("version="));
    assert!(diagnostic.contains("Validated"));
    assert!(diagnostic.contains("complete=true"));
    assert!(diagnostic.contains("Stopped"));
}

#[test]
fn watch_observes_real_changes_and_accepts_rebuild_before_saving() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::Duration;
    let f = Fixture::new();
    fs::write(f.root.join("old.rs"), "x").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_loci-experiment"))
        .args(["engine", "watch"])
        .arg(&f.root)
        .arg(f.base.join("state.loci"))
        .arg("--null")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (logs_tx, logs) = mpsc::channel();
    let stderr = child.stderr.take().unwrap();
    let log_reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            if logs_tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let (paths_tx, paths) = mpsc::channel();
    let stdout = child.stdout.take().unwrap();
    let path_reader = std::thread::spawn(move || {
        let mut stdout = BufReader::new(stdout);
        loop {
            let mut path = Vec::new();
            if stdout.read_until(0, &mut path).unwrap() == 0 {
                break;
            }
            path.pop();
            if paths_tx.send(path).is_err() {
                break;
            }
        }
    });
    let wait = |text: &str| loop {
        let line = logs
            .recv_timeout(Duration::from_secs(8))
            .expect("CLI diagnostic timeout");
        if line.contains(text) {
            break;
        }
    };
    let mut input = child.stdin.take().unwrap();
    wait("watch-ready");
    fs::rename(f.root.join("old.rs"), f.root.join("中文 new.rs")).unwrap();
    fs::write(f.root.join("added.txt"), "x").unwrap();
    // Wait for the owner to publish a new real inotify observation.
    wait("version=2,state=Validated");
    writeln!(input, "query 中文 ext:rs").unwrap();
    wait("command=query,ok=true");
    assert_eq!(
        paths.recv_timeout(Duration::from_secs(3)).unwrap(),
        f.root.join("中文 new.rs").as_os_str().as_encoded_bytes()
    );
    fs::remove_file(f.root.join("added.txt")).unwrap();
    wait("version=3,state=Validated");
    writeln!(input, "rebuild").unwrap();
    wait("command=rebuild,ok=true");
    writeln!(input, "save").unwrap();
    wait("command=save,ok=true");
    writeln!(input, "stop").unwrap();
    drop(input);
    assert!(child.wait().unwrap().success());
    log_reader.join().unwrap();
    path_reader.join().unwrap();
    let reopened = cli(&f, "query", Some("ext:txt"));
    assert!(reopened.status.success());
    assert!(reopened.stdout.is_empty());
}

#[test]
fn cli_rejects_invalid_arguments_internal_database_and_bounded_overflow() {
    let f = Fixture::new();
    let invalid = Command::new(env!("CARGO_BIN_EXE_loci-experiment"))
        .args(["engine", "query"])
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("4096"));
    let internal = Command::new(env!("CARGO_BIN_EXE_loci-experiment"))
        .args(["engine", "build"])
        .arg(&f.root)
        .arg(f.root.join("state.loci"))
        .output()
        .unwrap();
    assert_eq!(internal.status.code(), Some(2));
    assert!(!f.root.join("state.loci").exists());
    for n in 0..4097 {
        fs::write(f.root.join(format!("entry-{n:04}.txt")), "").unwrap();
    }
    let overflow = cli(&f, "build", None);
    assert!(!overflow.status.success());
    assert!(String::from_utf8_lossy(&overflow.stderr).contains("4096"));
    assert!(!f.base.join("state.loci").exists());
}

#[test]
fn rebuilt_cli_reconciles_offline_add_delete_and_escaped_output() {
    let f = Fixture::new();
    fs::write(f.root.join("deleted.rs"), "x").unwrap();
    assert!(cli(&f, "build", None).status.success());
    fs::remove_file(f.root.join("deleted.rs")).unwrap();
    fs::write(f.root.join("new\nname.rs"), "x").unwrap();
    assert!(cli(&f, "rebuild", None).status.success());
    let out = Command::new(env!("CARGO_BIN_EXE_loci-experiment"))
        .args(["engine", "query"])
        .arg(&f.root)
        .arg(f.base.join("state.loci"))
        .arg("ext:rs")
        .output()
        .unwrap();
    assert!(out.status.success());
    let paths = String::from_utf8(out.stdout).unwrap();
    assert!(paths.contains("new\\nname.rs"));
    assert_eq!(paths.lines().count(), 1);
    assert!(!paths.contains("deleted"));
}
