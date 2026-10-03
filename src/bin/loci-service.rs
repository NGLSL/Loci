use loci_experiment::service::{self, index_owner::IndexOwner};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
fn main() {
    let stopped = Arc::new(AtomicBool::new(false));
    let backend = IndexOwner::new();
    let result = service::run(backend, stopped.clone());
    stopped.store(true, Ordering::Release);
    if let Err(error) = result {
        eprintln!("Loci service: {error}");
        std::process::exit(1);
    }
}
