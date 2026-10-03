#[cfg(not(windows))]
compile_error!("The independent NTFS performance backend requires Windows.");

mod acceptance;
mod backend;
mod bench;
#[path = "../../windows-ntfs-stage-a/src/checkpoint.rs"]
mod checkpoint;
mod enumerate;
#[path = "../../windows-ntfs-stage-a/src/model.rs"]
mod model;
mod native;
mod query;
mod store;
mod transaction;
#[path = "../../windows-ntfs-stage-a/src/win.rs"]
mod win;

use std::{io, path::Path, sync::atomic::AtomicBool};

fn run() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cancel = AtomicBool::new(false);
    match args.get(1).map(String::as_str) {
        Some("capabilities") if args.len() == 3 => win::capabilities(&args[2]),
        Some("bench") if args.len() == 5 => bench::run(Path::new(&args[2]),Path::new(&args[3]),args[4].parse().map_err(|_|io::Error::new(io::ErrorKind::InvalidInput,"invalid fixture count"))?),
        Some("mft-projection") if args.len()==4 && args[3]=="--allow-volume-enum" => backend::inspect_mft(Path::new(&args[2]),&cancel),
        Some("acceptance-build") if args.len() == 5 => acceptance::build(Path::new(&args[2]), Path::new(&args[3]), args[4].parse().map_err(|_| io::Error::new(io::ErrorKind::InvalidInput,"invalid fixture count"))?),
        Some("acceptance-recover") if args.len() == 5 => acceptance::recover(Path::new(&args[2]),Path::new(&args[3]),args[4].parse().map_err(|_| io::Error::new(io::ErrorKind::InvalidInput,"invalid fixture count"))?),
        Some("query") if args.len() == 5 => {
            let mut backend=backend::Backend::open(Path::new(&args[2]),&Path::new(&args[3]).join("inventory.lcusn"),&cancel)?;
            let query: Vec<u16>=args[4].encode_utf16().collect();
            let result=backend.query(&query,50)?;
            println!("status={:?} total={} returned={} matching=case_sensitive_raw_utf16",result.status,result.total,result.paths.len());
            for path in result.paths { println!("path_utf16={path:04x?}"); }
            backend.stop();
            Ok(())
        },
        Some("acceptance-scan") if args.len()==5 => acceptance::scan_fixture(Path::new(&args[2]),Path::new(&args[3]),args[4].parse().map_err(|_|io::Error::new(io::ErrorKind::InvalidInput,"invalid fixture count"))?),
        _ => Err(io::Error::new(io::ErrorKind::InvalidInput,"usage: loci-ntfs-performance capabilities D: | bench <engineering-fixture> <engineering-storage> <1000|10000> | mft-projection <engineering-fixture> --allow-volume-enum | acceptance-build <engineering-fixture> <engineering-storage> <1000|10000> | acceptance-recover <engineering-fixture> <engineering-storage> <1000|10000> | acceptance-scan <engineering-fixture> <engineering-storage> <1000|10000> | query <engineering-fixture> <engineering-storage> <literal>\nOnly explicit engineering roots are accepted; no journal mutation or automatic elevation. Matching is a case-sensitive UTF-16 literal over relative paths.")),
    }
}
fn main() {
    let before = win::metrics().ok();
    let start = std::time::Instant::now();
    let result = run();
    if let (Some(before), Ok(after)) = (before, win::metrics()) {
        println!("process_resources elapsed_ms={} handles_before={} handles_after={} total_working_set_bytes={} private_commit_bytes={} peak_total_working_set_bytes={} kernel_bytes=unmeasured private_working_set_bytes=unmeasured",start.elapsed().as_millis(),before.handles,after.handles,after.working_set,after.private_usage,after.peak_working_set);
    }
    if let Err(error) = result {
        eprintln!(
            "FAILED kind={:?} os_code={:?} detail={error}",
            error.kind(),
            error.raw_os_error()
        );
        std::process::exit(1);
    }
}
