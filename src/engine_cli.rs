//! Real-directory CLI adapter over the public Engine.
use loci_experiment::engine::{Engine, QueryHandle, Status, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};
use std::ffi::OsString;
use std::io::{self, Write};
use std::io::{BufRead, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

const HELP: &str = "Real directory commands (public Engine, not synthetic data):
  engine build ROOT DATABASE
  engine query ROOT DATABASE QUERY [--fresh] [--null] [--all [--page-size N]]
  engine status ROOT DATABASE [--fresh]
  engine rebuild ROOT DATABASE
  engine watch ROOT DATABASE [--null]
Watch reads query QUERY / export QUERY / status / rebuild / save / stop commands from stdin.
Stop or stdin EOF saves the last validated snapshot and releases monitoring.
DATABASE and temporary saves must be outside ROOT. Current limits: 4096 entries,
128 directories, 1 MiB UTF-8 paths. Default query retains at most 50 paths;
--all or watch export enumerates one pinned snapshot in bounded pages.
Query terms use case-insensitive AND substrings and ext: filters.
Results go to stdout (escaped Rust strings, or exact NUL-delimited paths with --null).
Status and errors go to stderr. Validated means last reliable observation, not perpetual freshness.
Exit codes: 0 success, 2 invalid arguments, 3 failure, 4 pending/incomplete results.
Existing build/bench/query/scan commands remain experiments.";

pub fn entry(args: &[OsString]) {
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

fn run(args: &[OsString]) -> io::Result<()> {
    if args.len() == 1 && (args[0] == "--help" || args[0] == "help") {
        eprintln!("{HELP}");
        return Ok(());
    }
    let action = args.first().and_then(|arg| arg.to_str()).unwrap_or("");
    let base = match action {
        "build" | "rebuild" | "status" | "watch" => 3,
        "query" => 4,
        _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, HELP)),
    };
    if args.len() < base {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, HELP));
    }
    let (nul, all, fresh, page_size) = query_options(&args[base..], action)?;
    let root = Path::new(&args[1]);
    let database = Path::new(&args[2]);
    let mut startup = StartupTiming::new();
    let mut engine = Engine::open(root, Some(database))?;
    if action == "watch" {
        if engine.view().version > 0 {
            startup.searchable(&engine);
        }
        return watch(engine, root, database, nul, startup);
    }
    wait_for_snapshot(&mut engine, false, &mut startup)?;
    startup.searchable(&engine);
    if fresh || all || action == "build" || action == "rebuild" {
        wait_for_snapshot(&mut engine, true, &mut startup)?;
    }
    let raw = if action == "query" {
        args[3].to_str().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "query text must be UTF-8")
        })?
    } else {
        ""
    };
    let result = match action {
        "build" | "rebuild" => status(&engine.query()).and_then(|()| engine.save()),
        "query" if all => export(&engine.query(), raw, nul, page_size),
        "query" => query(&engine.query(), raw, nul),
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

fn query_options(args: &[OsString], action: &str) -> io::Result<(bool, bool, bool, usize)> {
    let mut nul = false;
    let mut all = false;
    let mut fresh = false;
    let mut page_size = DEFAULT_PAGE_SIZE;
    let mut size_given = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].to_str().unwrap_or("") {
            "--null" if !nul && (action == "query" || action == "watch") => nul = true,
            "--all" if !all && action == "query" => all = true,
            "--fresh" if !fresh && (action == "query" || action == "status") => fresh = true,
            "--page-size" if !size_given && action == "query" => {
                i += 1;
                page_size = args
                    .get(i)
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.parse().ok())
                    .filter(|n| (1..=MAX_PAGE_SIZE).contains(n))
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "page size must be 1..=1024")
                    })?;
                size_given = true;
            }
            _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, HELP)),
        }
        i += 1;
    }
    if size_given && !all {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--page-size requires --all",
        ));
    }
    Ok((nul, all, fresh, page_size))
}

struct StartupTiming {
    started: Instant,
    correcting: bool,
    validated: bool,
    last_progress: Instant,
}
impl StartupTiming {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            correcting: false,
            validated: false,
            last_progress: Instant::now(),
        }
    }
    fn searchable(&mut self, engine: &Engine) {
        let phase = if engine.view().status == Status::Validated {
            "validated"
        } else {
            "stale"
        };
        eprintln!(
            "engine,phase={phase},first_searchable_ms={},version={},state={:?}",
            self.started.elapsed().as_millis(),
            engine.view().version,
            engine.view().status
        );
        self.observe(engine);
    }
    fn before_poll(&mut self, engine: &Engine) {
        if !self.validated && !self.correcting && engine.view().status != Status::Validated {
            eprintln!(
                "engine,phase=correcting,scanned_entries={}",
                engine.metrics().scanned_entries
            );
            self.correcting = true;
        }
    }
    fn observe(&mut self, engine: &Engine) {
        self.observe_progress(engine.view().status, engine.metrics().scanned_entries);
    }
    fn observe_progress(&mut self, state: Status, scanned_entries: usize) {
        if !self.validated && state == Status::Validated {
            eprintln!(
                "engine,phase=validated,full_correction_ms={},scanned_entries={}",
                self.started.elapsed().as_millis(),
                scanned_entries
            );
            self.validated = true;
        } else if !self.validated
            && self.correcting
            && self.last_progress.elapsed() >= Duration::from_millis(100)
        {
            eprintln!(
                "engine,phase=correcting,scanned_entries={},state={:?}",
                scanned_entries, state
            );
            self.last_progress = Instant::now();
        }
    }
}

fn wait_for_snapshot(
    engine: &mut Engine,
    require_validated: bool,
    startup: &mut StartupTiming,
) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(90);
    while engine.view().version == 0
        || (require_validated && engine.view().status != Status::Validated)
    {
        if let Status::Failed(error) = engine.view().status {
            return Err(io::Error::other(error));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "initial build is still pending",
            ));
        }
        startup.before_poll(engine);
        engine.poll()?;
        if require_validated {
            startup.observe(engine);
        }
        if engine.view().version == 0
            || (require_validated && engine.view().status != Status::Validated)
        {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    Ok(())
}

fn export(handle: &QueryHandle, raw: &str, nul: bool, page_size: usize) -> io::Result<()> {
    let lease = handle.lease()?;
    let mut cursor = None;
    let mut page_number = 0;
    let stdout = io::stdout();
    let mut output = stdout.lock();
    loop {
        let page = lease.page(
            raw,
            cursor.as_ref(),
            page_size,
            &AtomicBool::new(false),
            &AtomicUsize::new(0),
        )?;
        page_number += 1;
        eprintln!("engine,page={page_number},version={},state={:?},complete={},validated={},paths_returned={}", page.version, page.finished.status, page.complete, page.validated_at_start_and_finish, page.paths.len());
        write_paths(&mut output, page.paths, nul)?;
        if !page.validated_at_start_and_finish || page.cancelled {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "export snapshot pending or cancelled",
            ));
        }
        if page.complete {
            break;
        }
        cursor = page.next;
    }
    output.flush()
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
    write_paths(&mut output, result.paths, nul)?;
    output.flush()?;
    if !result.validated_at_start_and_finish {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "results belong to a pending snapshot",
        ));
    }
    Ok(())
}

fn write_paths(
    output: &mut impl Write,
    paths: Vec<std::path::PathBuf>,
    nul: bool,
) -> io::Result<()> {
    for path in paths {
        if nul {
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
    Ok(())
}

fn watch(
    mut engine: Engine,
    root: &Path,
    database: &Path,
    nul: bool,
    mut startup: StartupTiming,
) -> io::Result<()> {
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
            startup.before_poll(&engine);
            if let Err(error) = engine.poll() {
                eprintln!("engine,failed={error}");
                failed = true;
            }
            startup.observe(&engine);
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
        let (name, result) = if let Some(raw) = line.strip_prefix("export ") {
            (
                "export",
                export(&engine.query(), raw, nul, DEFAULT_PAGE_SIZE),
            )
        } else if let Some(raw) = line.strip_prefix("query ") {
            ("query", query(&engine.query(), raw, nul))
        } else {
            match line.as_str() {
                "export" => ("export", export(&engine.query(), "", nul, DEFAULT_PAGE_SIZE)),
                "query" => ("query", query(&engine.query(), "", nul)),
                "status" => ("status", status(&engine.query())),
                "save" => ("save", engine.save()),
                "rebuild" => {
                    // Opening the public Engine performs a new root reconciliation.
                    engine.stop()?;
                    startup = StartupTiming::new();
                    engine = Engine::open(root, Some(database))?;
                    wait_for_snapshot(&mut engine, true, &mut startup)?;
                    failed = false;
                    ("rebuild", status(&engine.query()))
                }
                _ => (
                    "invalid",
                    Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "watch commands: query QUERY | export QUERY | status | rebuild | save | stop",
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
