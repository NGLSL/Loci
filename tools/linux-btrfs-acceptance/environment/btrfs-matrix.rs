use std::os::fd::AsRawFd;
use std::os::unix::{
    ffi::{OsStrExt, OsStringExt},
    fs::{symlink, MetadataExt, PermissionsExt},
};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};
fn cli() -> String {
    std::env::var("LOCI_FINAL_CLI").expect("explicit frozen CLI required")
}
fn command(args: &[&str]) {
    assert!(Command::new("/guest/btrfs")
        .args(args)
        .status()
        .unwrap()
        .success());
}
fn prepare() {
    let base = PathBuf::from(format!(
        "/mnt/data/{}/matrix-{}-{}",
        std::env::var("LOCI_TOKEN").expect("owned token"),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    assert!(!base.exists());
    fs::create_dir(&base).unwrap();
    let root = base.join("tree");
    fs::create_dir(&root).unwrap();
    for n in ["sub-a", "sub-b"] {
        let p = root.join(n);
        command(&["subvolume", "create", p.to_str().unwrap()]);
        fs::create_dir(p.join("common")).unwrap();
        fs::write(p.join("common/report.txt"), n).unwrap();
    }
    let raw = root.join(std::ffi::OsString::from_vec(b"sub-\xff-byte".to_vec()));
    assert!(Command::new("/guest/btrfs")
        .args([
            std::ffi::OsStr::new("subvolume"),
            std::ffi::OsStr::new("create"),
            raw.as_os_str()
        ])
        .status()
        .unwrap()
        .success());
    fs::write(raw.join("raw-report.txt"), "r").unwrap();
    let a = root.join("sub-a");
    fs::hard_link(
        a.join("common/report.txt"),
        a.join("common/alias-report.txt"),
    )
    .unwrap();
    fs::write(
        a.join(std::ffi::OsString::from_vec(
            b"raw-\xff-report.txt".to_vec(),
        )),
        "x",
    )
    .unwrap();
    symlink("../sub-b", a.join("link-b")).unwrap();
    for n in ["mounted-sub", "bind-boundary"] {
        fs::create_dir(root.join(n)).unwrap();
    }
    fs::create_dir(root.join(std::ffi::OsString::from_vec(b"bind-\xff-space x".to_vec()))).unwrap();
    fs::create_dir(base.join("outside")).unwrap();
    fs::write(base.join("outside/hidden.txt"), "h").unwrap();
    fs::write(
        format!(
            "/mnt/data/{}/matrix-path",
            std::env::var("LOCI_TOKEN").expect("owned token")
        ),
        base.as_os_str().as_bytes(),
    )
    .unwrap();
    println!("MATRIX_PREPARED {}", base.display());
}
fn oracle(root: &Path, boundaries: &[PathBuf]) -> Vec<Vec<u8>> {
    fn walk(p: &Path, b: &[PathBuf], o: &mut Vec<Vec<u8>>) {
        for e in fs::read_dir(p).unwrap() {
            let e = e.unwrap();
            let path = e.path();
            let m = fs::symlink_metadata(&path).unwrap();
            if m.is_dir() || m.is_file() || m.file_type().is_symlink() {
                o.push(path.as_os_str().as_bytes().to_vec());
            }
            if m.is_dir() && !b.contains(&path) {
                walk(&path, b, o);
            }
        }
    }
    let mut out = vec![];
    walk(root, boundaries, &mut out);
    out.sort();
    out
}
fn query(root: &Path, db: &Path, text: &str) -> Vec<Vec<u8>> {
    let out = Command::new(cli())
        .args([
            std::ffi::OsStr::new("engine"),
            std::ffi::OsStr::new("query"),
            root.as_os_str(),
            db.as_os_str(),
            std::ffi::OsStr::new(text),
            std::ffi::OsStr::new("--scale"),
            std::ffi::OsStr::new("--fresh"),
            std::ffi::OsStr::new("--all"),
            std::ffi::OsStr::new("--null"),
        ])
        .output()
        .unwrap();
    eprint!("{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success());
    let mut paths: Vec<_> = out
        .stdout
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_vec())
        .collect();
    paths.sort();
    paths
}
struct Watch {
    child: Child,
    input: ChildStdin,
    lines: Receiver<String>,
    paths: Receiver<Vec<u8>>,
}
impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Watch {
    fn start(root: &Path, db: &Path) -> Self {
        let mut c = Command::new(cli())
            .args([
                std::ffi::OsStr::new("engine"),
                std::ffi::OsStr::new("watch"),
                root.as_os_str(),
                db.as_os_str(),
                std::ffi::OsStr::new("--scale"),
                std::ffi::OsStr::new("--null"),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let input = c.stdin.take().unwrap();
        let err = c.stderr.take().unwrap();
        let out = c.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(err).lines() {
                let line = line.unwrap();
                eprintln!("WATCH {line}");
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let (tx, paths) = mpsc::channel();
        thread::spawn(move || {
            let mut r = BufReader::new(out);
            loop {
                let mut p = vec![];
                match r.read_until(0, &mut p) {
                    Ok(0) | Err(_) => break,
                    _ => {
                        assert_eq!(p.pop(), Some(0));
                        if tx.send(p).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let w = Self {
            child: c,
            input,
            lines,
            paths,
        };
        w.wait("engine,watch-ready", Duration::from_secs(15));
        w
    }
    fn wait(&self, needle: &str, limit: Duration) {
        let begin = Instant::now();
        loop {
            assert!(begin.elapsed() < limit, "waiting {needle}");
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(1))
                .expect("CLI diagnostic timeout");
            if line.contains(needle) {
                return;
            }
        }
    }
    fn send(&mut self, s: &str) {
        writeln!(self.input, "{s}").unwrap();
        self.input.flush().unwrap();
    }
    fn export(&mut self) -> Vec<Vec<u8>> {
        while self.paths.try_recv().is_ok() {}
        self.send("export");
        self.wait("engine,command=export", Duration::from_secs(5));
        let mut p = vec![];
        while let Ok(path) = self.paths.recv_timeout(Duration::from_millis(150)) {
            p.push(path);
        }
        p.sort();
        p
    }
    fn eventually(&mut self, root: &Path, b: &[PathBuf], label: &str) {
        let expected = oracle(root, b);
        let t = Instant::now();
        loop {
            if self.export() == expected {
                println!("MATRIX_ORACLE_PASS {label} paths={}", expected.len());
                return;
            }
            assert!(
                t.elapsed() < Duration::from_secs(15),
                "watch full oracle mismatch {label}"
            );
            thread::sleep(Duration::from_millis(100));
        }
    }
}
fn check() {
    let base = PathBuf::from(std::ffi::OsString::from_vec(
        fs::read(format!(
            "/mnt/data/{}/matrix-path",
            std::env::var("LOCI_TOKEN").expect("owned token")
        ))
        .unwrap(),
    ));
    let root = base.join("tree");
    let b = [
        root.join("mounted-sub"),
        root.join("bind-boundary"),
        root.join(std::ffi::OsString::from_vec(b"bind-\xff-space x".to_vec())),
    ];
    let db = base.join("state.db");
    let expected = oracle(&root, &b);
    assert_eq!(query(&root, &db, ""), expected);
    println!("MATRIX_ORACLE_PASS initial paths={}", expected.len());
    for n in ["sub-a", "sub-b"] {
        let p = root.join(n);
        let m = fs::metadata(&p).unwrap();
        let fd = fs::File::open(&p).unwrap();
        let info = fs::read_to_string(format!("/proc/self/fdinfo/{}", fd.as_raw_fd())).unwrap();
        let mid = info.lines().find(|l| l.starts_with("mnt_id:")).unwrap();
        println!(
            "MATRIX_IDENTITY {n} dev={} inode={} {mid}",
            m.dev(),
            m.ino()
        );
        assert_eq!(m.ino(), 256);
        assert_eq!(
            query(&p, &base.join(format!("{n}.db")), ""),
            oracle(&p, &[])
        );
        println!("MATRIX_ORACLE_PASS selected-{n}");
    }
    let raw = root.join(std::ffi::OsString::from_vec(b"sub-\xff-byte".to_vec()));
    assert_eq!(
        query(&raw, &base.join("raw-root.db"), ""),
        oracle(&raw, &[])
    );
    println!("MATRIX_ORACLE_PASS selected-raw-subvolume-root");
    let cross = fs::hard_link(
        root.join("sub-a/common/report.txt"),
        root.join("sub-b/cross-subvol-hardlink"),
    );
    assert_eq!(cross.unwrap_err().raw_os_error(), Some(18));
    println!("MATRIX_CROSS_SUBVOLUME_HARDLINK_EXDEV");
    let a = root.join("sub-a/common/report.txt");
    let alias = root.join("sub-a/common/alias-report.txt");
    assert_eq!(
        fs::metadata(a).unwrap().ino(),
        fs::metadata(alias).unwrap().ino()
    );
    let hidden = query(&root, &db, "hidden");
    assert!(hidden.is_empty());
    let reports = query(&root, &db, "report");
    let mut expected_reports: Vec<_> = expected
        .iter()
        .filter(|p| p[root.as_os_str().as_bytes().len()..].windows(6).any(|s| s == b"report"))
        .cloned()
        .collect();
    expected_reports.sort();
    assert_eq!(reports, expected_reports);
    println!("MATRIX_ORACLE_PASS legal-runs-raw-and-hardlinks");
    let selected = PathBuf::from("/mnt/selected-sub");
    assert_eq!(
        query(&selected, &base.join("mounted-selected.db"), ""),
        oracle(&selected, &[])
    );
    println!("MATRIX_ORACLE_PASS selected-separate-mount");
    let mut w = Watch::start(&root, &db);
    w.eventually(&root, &b, "watch-initial");
    fs::rename(root.join("sub-a/common"), root.join("sub-a/moved")).unwrap();
    w.eventually(&root, &b, "directory-rename");
    fs::write(root.join("sub-a/moved/followup.txt"), "event").unwrap();
    w.eventually(&root, &b, "rename-followup");
    fs::rename(root.join("sub-b"), root.join("sub-c")).unwrap();
    w.eventually(&root, &b, "subvolume-root-rename");
    fs::write(root.join("sub-c/common/next.txt"), "event").unwrap();
    w.eventually(&root, &b, "subvolume-rename-followup");
    fs::set_permissions(root.join("sub-c/common"), fs::Permissions::from_mode(0)).unwrap();
    w.send("status");
    thread::sleep(Duration::from_millis(300));
    w.send("status");
    w.wait("reason=Permission,errno=Some(13)", Duration::from_secs(5));
    println!("MATRIX_PERMISSION_GAP_PASS");
    fs::set_permissions(root.join("sub-c/common"), fs::Permissions::from_mode(0o755)).unwrap();
    w.send("rebuild");
    w.wait("engine,command=rebuild,ok=true", Duration::from_secs(5));
    w.eventually(&root, &b, "permission-restored-rebuild");
    w.send("stop");
    assert!(w.child.wait().unwrap().success());
    println!("BTRFS_MATRIX_NATIVE_FINAL_CLI_PASS");
}
fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("prepare") => prepare(),
        Some("check") => check(),
        _ => panic!("prepare|check"),
    }
}
