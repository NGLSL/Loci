#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
// Opt-in native acceptance draft. Controller and Engine have separate PIDs.
#[cfg(target_os = "linux")]
#[path = "million_acceptance/budget.rs"]
mod budget;
#[cfg(target_os = "linux")]
#[path = "million_acceptance/controller.rs"]
mod controller;
#[cfg(target_os = "linux")]
#[path = "million_acceptance/process.rs"]
mod process;
#[path = "million_acceptance/protocol.rs"]
mod protocol;
#[cfg(target_os = "linux")]
#[path = "million_acceptance/worker.rs"]
mod worker;
use protocol::invalid;
use std::{collections::BTreeMap, io, path::PathBuf};

#[derive(Clone)]
struct Options {
    root: PathBuf,
    database: PathBuf,
    output: PathBuf,
    sha: String,
    queries: Option<PathBuf>,
    phase: String,
    repetitions: usize,
    idle_seconds: u64,
    smoke: bool,
    fixture_input: Option<PathBuf>,
    output_budget_bytes: u64,
}
impl Options {
    fn parse() -> io::Result<(String, Self)> {
        let mut args = std::env::args().skip(1);
        let mode = args
            .next()
            .ok_or_else(|| invalid("first argument --worker or --controller"))?;
        if mode != "--worker" && mode != "--controller" {
            return Err(invalid("mode --worker or --controller"));
        }
        let mut values = BTreeMap::new();
        let mut smoke = false;
        while let Some(key) = args.next() {
            if key == "--smoke" {
                smoke = true;
                continue;
            }
            if !matches!(
                key.as_str(),
                "--root"
                    | "--database"
                    | "--output"
                    | "--sha"
                    | "--queries"
                    | "--phase"
                    | "--repetitions"
                    | "--idle-seconds"
                    | "--fixture-input"
                    | "--output-budget-mib"
            ) {
                return Err(invalid("unknown argument"));
            }
            let value = args
                .next()
                .ok_or_else(|| invalid("argument value missing"))?;
            if values.insert(key, value).is_some() {
                return Err(invalid("duplicate argument"));
            }
        }
        let path = |key: &str| -> io::Result<PathBuf> {
            let p = PathBuf::from(
                values
                    .get(key)
                    .ok_or_else(|| invalid("required path missing"))?,
            );
            if !p.is_absolute() {
                return Err(invalid("path must be absolute"));
            }
            Ok(p)
        };
        let sha = values
            .get("--sha")
            .ok_or_else(|| invalid("exact SHA required"))?
            .clone();
        if sha.len() != 40
            || !sha
                .bytes()
                .all(|x| x.is_ascii_hexdigit() && !x.is_ascii_uppercase())
        {
            return Err(invalid("SHA must be40lowercasehex"));
        }
        let repetitions = values
            .get("--repetitions")
            .map_or(Ok(200), |x| x.parse())
            .map_err(|_| invalid("repetition integer"))?;
        let idle_seconds = values
            .get("--idle-seconds")
            .map_or(Ok(600), |x| x.parse())
            .map_err(|_| invalid("idle seconds"))?;
        if !(1..=10000).contains(&repetitions) || idle_seconds == 0 || idle_seconds > 86400 {
            return Err(invalid("measurement bound"));
        }
        if !smoke && (repetitions < 200 || idle_seconds < 600) {
            return Err(invalid(
                "reduced repetitions/window require --smoke, cannot accept",
            ));
        }
        let output_budget_mib: u64 = values
            .get("--output-budget-mib")
            .map_or(Ok(4096), |v| v.parse())
            .map_err(|_| invalid("outputbudget integer"))?;
        if !(512..=65536).contains(&output_budget_mib) {
            return Err(invalid("aggregate outputbudget512..65536MiB"));
        }
        let root = path("--root")?;
        let database = path("--database")?;
        let output = path("--output")?;
        if database.starts_with(&root) || output.starts_with(&root) {
            return Err(invalid("database/output must be outside selectedroot"));
        }
        Ok((
            mode,
            Self {
                root,
                database,
                output,
                sha,
                queries: values.get("--queries").map(PathBuf::from),
                phase: values.get("--phase").cloned().unwrap_or("all".into()),
                repetitions,
                idle_seconds,
                smoke,
                fixture_input: values.get("--fixture-input").map(PathBuf::from),
                output_budget_bytes: output_budget_mib * 1024 * 1024,
            },
        ))
    }
}
fn main() {
    if std::env::args().nth(1).as_deref() == Some("--help") {
        println!(
            "Opt-in native Linux acceptance; Engine runs in a separate production-owner PID.\n\
linux_million_acceptance --controller|--worker --root ABS --database ABS --output ABS --sha 40HEX\n\
  [--queries UTF8_LINES] [--phase all|smoke|correctness|queries|events|restart|idle|fixture]\n\
  [--repetitions 200] [--idle-seconds 600] [--output-budget-mib 4096] [--smoke]\n\
Reduced samples/windows require --smoke and cannot establish acceptance."
        );
        return;
    }
    let result = (|| -> io::Result<()> {
        let (mode, options) = Options::parse()?;
        #[cfg(target_os = "linux")]
        {
            if mode == "--worker" {
                worker::run(options)
            } else {
                controller::run(options)
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (mode, options);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native Linux acceptance only",
            ))
        }
    })();
    if let Err(error) = result {
        eprintln!("acceptance failed: {error}");
        std::process::exit(1);
    }
}
