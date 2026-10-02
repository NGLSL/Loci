#![cfg_attr(not(windows), allow(dead_code))]
#[cfg(not(windows))]
compile_error!("This standalone probe requires Windows; it is not the shared Loci engine.");

mod checkpoint;
mod fallback;
mod fixture;
mod model;
mod sync;
mod win;

use std::{io, path::Path};

fn run() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("capabilities") if args.len() == 3 => win::capabilities(&args[2]),
        Some("capabilities-repeat") if args.len()==3 => {
            let mut result=Ok(());
            for round in 1..=3{println!("capability_round={round}");result=win::capabilities(&args[2]);}
            result
        },
        Some("fixture") if args.len() == 3 => {
            let base=std::fs::canonicalize(Path::new(&args[2]))?;
            let engineering=std::fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/windows-ntfs-stage-a/run"))?;
            if !base.starts_with(&engineering){return Err(io::Error::new(io::ErrorKind::PermissionDenied,"fixture base must be inside this probe's existing engineering .scratch/windows-ntfs-stage-a/run directory"));}
            fixture::run(&base)
        },
        Some("ntfs") if args.len() == 4 && args[3] == "--allow-volume-enum" => {
            sync::verify(Path::new(&args[2]))
        }
        _ => Err(io::Error::new(io::ErrorKind::InvalidInput,
            "usage: loci-ntfs-stage-a capabilities D: | fixture <existing-project-test-base> | ntfs <existing-fixture-root> --allow-volume-enum\nntfs reads the selected root's ENTIRE VOLUME MFT in bounded batches, never prints other filenames; no elevation or journal mutation")),
    }
}
fn main() {
    if let Err(e) = run() {
        eprintln!(
            "FAILED kind={:?} os_code={:?} detail={e}",
            e.kind(),
            e.raw_os_error()
        );
        std::process::exit(1);
    }
}
