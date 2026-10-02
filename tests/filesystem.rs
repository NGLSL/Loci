//! Real filesystem operations in a small disposable fixture; no OS watcher.
use std::fs;
use std::path::Path;
use std::process::Command;

fn scan(root: &Path, expected_files: usize, expected_rust_files: usize) {
    let output = Command::new(env!("CARGO_BIN_EXE_loci-experiment"))
        .arg("scan")
        .arg(root)
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    let row = text.lines().find(|l| l.starts_with("scan,")).unwrap();
    let parts: Vec<_> = row.split(',').collect();
    assert_eq!(parts[1].parse::<usize>().unwrap(), expected_files);
    assert_eq!(parts[2], "0");
    let row = text
        .lines()
        .find(|l| l.starts_with("real_query,ext:rs,"))
        .unwrap();
    assert_eq!(
        row.split(',').nth(2).unwrap().parse::<usize>().unwrap(),
        expected_rust_files
    );
}

#[test]
fn actual_add_delete_rename_are_found_by_bounded_rescan() {
    let root = std::env::current_dir()
        .unwrap()
        .join("work")
        .join(format!("fs-fixture-{}", std::process::id()));
    assert!(!root.exists(), "refuse to reuse existing fixture");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir(root.join("target")).unwrap();
    fs::create_dir(root.join(".git")).unwrap();
    fs::write(root.join("src/a.rs"), "fixture").unwrap();
    fs::write(root.join("target/ignored.rs"), "fixture").unwrap();
    fs::write(root.join(".git/ignored.rs"), "fixture").unwrap();
    scan(&root, 1, 1);
    fs::write(root.join("src/报告.rs"), "fixture").unwrap();
    scan(&root, 2, 2);
    fs::rename(root.join("src/报告.rs"), root.join("src/renamed.md")).unwrap();
    scan(&root, 2, 1);
    fs::remove_file(root.join("src/a.rs")).unwrap();
    scan(&root, 1, 0);
    // Delete only explicitly created files and directories, no recursive delete.
    for file in ["src/renamed.md", "target/ignored.rs", ".git/ignored.rs"] {
        fs::remove_file(root.join(file)).unwrap();
    }
    for dir in ["src", "target", ".git"] {
        fs::remove_dir(root.join(dir)).unwrap();
    }
    fs::remove_dir(root).unwrap();
}
