//! Real-directory CLI adapter over the public bounded Engine.
use loci_experiment::engine::{Engine, QueryHandle, Status};
use std::io::{self, BufRead, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

const HELP: &str = "Real directory commands (bounded v0.1 Engine, not synthetic data):
  engine build ROOT DATABASE
  engine query ROOT DATABASE QUERY [--null]
  engine status ROOT DATABASE
  engine rebuild ROOT DATABASE
  engine watch ROOT DATABASE [--null]
Watch reads query QUERY / status / rebuild / save / stop commands from stdin.
Stop or stdin EOF saves the last validated snapshot and releases monitoring.
DATABASE and temporary saves must be outside ROOT. Current limits: 4096 entries,
128 directories, 1 MiB UTF-8 paths; results retain at most 50 paths.
Query terms use case-insensitive AND substrings and ext: filters.
Results go to stdout (escaped Rust strings, or exact NUL-delimited paths with --null).
Status and errors go to stderr. Validated means last reliable observation, not perpetual freshness.
Exit codes: 0 success, 2 invalid arguments, 3 failure, 4 pending/incomplete results.
Existing build/bench/query/scan/live-check commands remain experiments.";

pub fn entry(args: &[String]) {
    if let Err(error) = run(args) {
        eprintln!(
            "engine: {error} (bounded v0.1: 4096 entries, 128 directories, 1 MiB UTF-8 paths)"
        );
        let code = match error.kind() {
            io::ErrorKind::InvalidInput => 2,
            io::ErrorKind::WouldBlock => 4,
            _ => 3,
        };
        std::process::exit(code);
    }
}

fn run(args: &[String]) -> io::Result<()> {
    if args == ["--help"] || args == ["help"] {
        eprintln!("{HELP}");
        return Ok(());
    }
    let action = args.first().map(String::as_str).unwrap_or("");
    let valid = match action {
        "build" | "rebuild" | "status" => args.len() == 3,
        "watch" => args.len() == 3 || (args.len() == 4 && args[3] == "--null"),
        "query" => args.len() == 4 || (args.len() == 5 && args[4] == "--null"),
        _ => false,
    };
    if !valid {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, HELP));
    }
    let root = Path::new(&args[1]);
    let database = Path::new(&args[2]);
    let mut engine = Engine::open(root, Some(database))?;
    if action == "watch" {
        return watch(engine, root, database, args.len() == 4);
    }
    let result = match action {
        "build" | "rebuild" => status(&engine.query()).and_then(|()| engine.save()),
        "query" => query(&engine.query(), &args[3], args.len() == 5),
        "status" => status(&engine.query()),
        _ => unreachable!(),
    };
    let stop = engine.stop();
    eprintln!(
        "engine,version={},state={:?}",
        engine.view().version,
        engine.view().status
    );
    result.and(stop)
}

fn status(handle: &QueryHandle) -> io::Result<()> {
    let result =
        handle
            .lease()?
            .search("", false, &AtomicBool::new(false), &AtomicUsize::new(0))?;
    eprintln!(
        "engine,version={},state={:?},complete={},validated={},matches={},paths_returned={}",
        result.version,
        result.finished.status,
        result.complete,
        result.validated_at_start_and_finish,
        result.matches,
        result.paths.len()
    );
    if result.finished.status != Status::Validated || !result.complete {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "snapshot is pending or incomplete",
        ));
    }
    Ok(())
}

fn query(handle: &QueryHandle, raw: &str, nul: bool) -> io::Result<()> {
    let result =
        handle
            .lease()?
            .search(raw, true, &AtomicBool::new(false), &AtomicUsize::new(0))?;
    eprintln!(
        "engine,version={},state={:?},complete={},validated={},matches={},paths_returned={}",
        result.version,
        result.finished.status,
        result.complete,
        result.validated_at_start_and_finish,
        result.matches,
        result.paths.len()
    );
    let stdout = io::stdout();
    let mut output = stdout.lock();
    for path in result.paths {
        if nul {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                output.write_all(path.as_os_str().as_bytes())?;
            }
            #[cfg(not(unix))]
            output.write_all(
                path.to_str()
                    .ok_or_else(|| io::Error::other("path is not UTF-8"))?
                    .as_bytes(),
            )?;
            output.write_all(&[0])?;
        } else {
            writeln!(output, "{:?}", path.as_os_str())?;
        }
    }
    output.flush()?;
    if !result.validated_at_start_and_finish {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "results belong to a pending snapshot",
        ));
    }
    Ok(())
}

fn watch(mut engine: Engine, root: &Path, database: &Path, nul: bool) -> io::Result<()> {
    // A bounded command queue keeps monitoring independent from terminal input.
    let (sender, receiver) = mpsc::sync_channel(8);
    std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut input = stdin.lock();
        loop {
            let mut line = String::new();
            let read = (&mut input).take(4097).read_line(&mut line);
            let command = match read {
                Ok(0) => break,
                Ok(_) if line.len() <= 4096 => Ok(line.trim_end_matches(['\r', '\n']).to_owned()),
                Ok(_) => Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "watch command exceeds 4096 bytes",
                )),
                Err(error) => Err(error),
            };
            let failed = command.is_err();
            if sender.send(command).is_err() || failed {
                break;
            }
        }
    });
    if let Err(error) = status(&engine.query()) {
        eprintln!(
            "engine,state={:?},complete=false,error={error}",
            engine.view().status
        );
    }
    eprintln!("engine,watch-ready");
    let mut previous = (engine.view().version, engine.view().status);
    let mut failed = false;
    loop {
        if !failed {
            if let Err(error) = engine.poll() {
                eprintln!("engine,failed={error}");
                failed = true;
            }
        }
        let view = engine.view();
        if previous != (view.version, view.status.clone()) {
            eprintln!("engine,version={},state={:?}", view.version, view.status);
            previous = (view.version, view.status);
        }
        let line = match receiver.recv_timeout(Duration::from_millis(20)) {
            Ok(line) => line?,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if line == "stop" {
            break;
        }
        let (name, result) = if let Some(raw) = line.strip_prefix("query ") {
            ("query", query(&engine.query(), raw, nul))
        } else {
            match line.as_str() {
                "query" => ("query", query(&engine.query(), "", nul)),
                "status" => ("status", status(&engine.query())),
                "save" => ("save", engine.save()),
                "rebuild" => {
                    // Opening the public Engine performs a new root reconciliation.
                    engine.stop()?;
                    engine = Engine::open(root, Some(database))?;
                    failed = false;
                    ("rebuild", status(&engine.query()))
                }
                _ => (
                    "invalid",
                    Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "watch commands: query QUERY | status | rebuild | save | stop",
                    )),
                ),
            }
        };
        eprintln!("engine,command={name},ok={}", result.is_ok());
        if let Err(error) = result {
            eprintln!("engine,command={name},error={error}");
        }
    }
    let saved = engine.save();
    let stopped = engine.stop();
    eprintln!(
        "engine,version={},state={:?}",
        engine.view().version,
        engine.view().status
    );
    saved.and(stopped)
}
