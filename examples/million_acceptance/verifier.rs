//! Independent raw-filesystem query oracle, outside timed production queries.
use crate::protocol::{invalid, unhex, Json};
use std::collections::HashSet;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

struct RawQuery {
    terms: Vec<String>,
    extension: Option<String>,
}
impl RawQuery {
    fn parse(query: &str) -> Self {
        let mut terms = Vec::new();
        let mut extension = None;
        for term in query.split_whitespace() {
            if let Some(value) = term.strip_prefix("ext:") {
                extension = Some(value.to_lowercase());
            } else {
                terms.push(term.to_lowercase());
            }
        }
        Self { terms, extension }
    }
    fn matches(&self, raw: &[u8]) -> bool {
        let mut runs = Vec::new();
        let mut rest = raw;
        while !rest.is_empty() {
            match std::str::from_utf8(rest) {
                Ok(text) => {
                    runs.push(text.to_lowercase());
                    break;
                }
                Err(error) => {
                    runs.push(
                        std::str::from_utf8(&rest[..error.valid_up_to()])
                            .unwrap()
                            .to_lowercase(),
                    );
                    rest = &rest[error.valid_up_to()
                        + error
                            .error_len()
                            .unwrap_or(rest.len() - error.valid_up_to())..];
                }
            }
        }
        self.terms
            .iter()
            .all(|term| runs.iter().any(|run| run.contains(term)))
            && self.extension.as_ref().is_none_or(|extension| {
                // Whole-valid paths lowercase before extracting the suffix; invalid
                // paths use a separately valid raw suffix. Unicode final sigma can
                // depend on context, so those two existing contracts are distinct.
                if std::str::from_utf8(raw).is_ok() {
                    let normalized = runs.first().map_or("", String::as_str);
                    normalized
                        .rsplit('/')
                        .next()
                        .unwrap_or(normalized)
                        .rsplit_once('.')
                        .is_some_and(|(_, suffix)| suffix == extension)
                } else {
                    let name = raw.rsplit(|byte| *byte == b'/').next().unwrap_or(raw);
                    name.iter()
                        .rposition(|byte| *byte == b'.')
                        .is_some_and(|dot| {
                            std::str::from_utf8(&name[dot + 1..])
                                .is_ok_and(|suffix| suffix.to_lowercase() == *extension)
                        })
                }
            })
    }
}
fn nul_record(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut path = Vec::new();
    loop {
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            return if path.is_empty() {
                Ok(None)
            } else {
                Err(invalid("truncated oracle NUL record"))
            };
        }
        let end = bytes.iter().position(|byte| *byte == 0);
        let count = end.unwrap_or(bytes.len());
        if path.len() + count > 4096 {
            return Err(invalid("oracle path bound"));
        }
        path.extend_from_slice(&bytes[..count]);
        reader.consume(count + usize::from(end.is_some()));
        if end.is_some() {
            if path.is_empty() {
                return Err(invalid("empty oracle path"));
            }
            return Ok(Some(path));
        }
    }
}
pub(super) struct FirstPage {
    version: u64,
    paths: Vec<Vec<u8>>,
    complete: bool,
}
impl FirstPage {
    pub(super) fn capture(reply: &Json, version: u64) -> io::Result<Self> {
        if reply.get("version")?.number()? != version
            || !reply.get("validated")?.boolean()?
            || reply.get("cancelled")?.boolean()?
            || reply.get("status_start")?.text()? != "Validated"
            || reply.get("status_finish")?.text()? != "Validated"
        {
            return Err(invalid("QUERY50 did not retain validated stable cut"));
        }
        let paths = reply
            .get("paths_hex")?
            .array()?
            .iter()
            .map(|path| unhex(path.text()?))
            .collect::<io::Result<Vec<_>>>()?;
        if paths.len() > 50
            || paths
                .iter()
                .any(|path| path.is_empty() || path.contains(&0))
            || paths.iter().collect::<HashSet<_>>().len() != paths.len()
        {
            return Err(invalid("QUERY50 path cardinality/duplicate bound"));
        }
        Ok(Self {
            version,
            paths,
            complete: reply.get("complete")?.boolean()?,
        })
    }
    pub(super) fn check(&self, reply: &Json) -> io::Result<()> {
        let fresh = Self::capture(reply, self.version)?;
        if self.paths != fresh.paths || self.complete != fresh.complete {
            return Err(invalid("QUERY50 changed stable snapshot order/results"));
        }
        Ok(())
    }
}
fn verify_first50(reply: &Json, version: u64, count: u64, oracle: &Path) -> io::Result<()> {
    let page = FirstPage::capture(reply, version)?;
    if page.paths.len() as u64 != count.min(50)
        || (count < 50 && !page.complete)
        || (count > 50 && page.complete)
    {
        return Err(invalid(
            "QUERY50 does not return exact first-page cardinality",
        ));
    }
    let mut wanted = page.paths.into_iter().collect::<HashSet<_>>();
    let mut reader = BufReader::new(File::open(oracle)?);
    let mut oracle_count = 0;
    while let Some(path) = nul_record(&mut reader)? {
        oracle_count += 1;
        wanted.remove(&path);
    }
    if oracle_count != count || !wanted.is_empty() {
        return Err(invalid("QUERY50 is not a subset of complete native oracle"));
    }
    Ok(())
}

use super::{equal_files, external_sort, file_digest, remove_owned_artifact, Log, Worker};
use crate::budget::Budget;
use crate::protocol::{b, hex, n, s};
use crate::Options;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};
const MAX_FILE: u64 = 512 * 1024 * 1024;

struct ArtifactWriter<'a> {
    file: BufWriter<File>,
    bytes: u64,
    checked: u64,
    budget: &'a Budget,
}
impl<'a> ArtifactWriter<'a> {
    fn new(path: &Path, budget: &'a Budget) -> io::Result<Self> {
        budget.check(2 * 1024 * 1024)?;
        Ok(Self {
            file: BufWriter::new(
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(0x20000)
                    .open(path)?,
            ),
            bytes: 0,
            checked: 0,
            budget,
        })
    }
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| invalid("query artifact overflow"))?;
        if self.bytes > MAX_FILE {
            return Err(invalid("query artifact 512MiB bound"));
        }
        if self.bytes - self.checked >= 1024 * 1024 {
            self.budget.check(2 * 1024 * 1024)?;
            self.checked = self.bytes;
        }
        self.file.write_all(bytes)
    }
    fn finish(mut self) -> io::Result<()> {
        self.file.flush()?;
        self.file.get_ref().sync_all()
    }
}
fn ensure_cut(worker: &mut Worker, version: u64) -> io::Result<()> {
    let status = worker.status()?;
    if status.get("version")?.number()? != version
        || status.get("status")?.text()? != "Validated"
        || !status.get("gaps")?.array()?.is_empty()
    {
        return Err(invalid("filtered oracle lost validated stable cut"));
    }
    Ok(())
}
fn mount_boundaries(root: &Path) -> io::Result<HashSet<PathBuf>> {
    let mut bytes = Vec::new();
    File::open("/proc/self/mountinfo")?
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(invalid("oracle mount metadata bound"));
    }
    let mut points = HashSet::new();
    for line in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let escaped = line
            .split(|byte| *byte == b' ')
            .nth(4)
            .ok_or_else(|| invalid("oracle mount record"))?;
        let mut raw = Vec::new();
        let mut at = 0;
        while at < escaped.len() {
            if escaped[at] == b'\\' {
                let digits = escaped
                    .get(at + 1..at + 4)
                    .ok_or_else(|| invalid("oracle mount escape"))?;
                if !digits.iter().all(|digit| (b'0'..=b'7').contains(digit)) {
                    return Err(invalid("oracle mount escape"));
                }
                let decoded = (digits[0] - b'0') as u16 * 64
                    + (digits[1] - b'0') as u16 * 8
                    + (digits[2] - b'0') as u16;
                raw.push(u8::try_from(decoded).map_err(|_| invalid("oracle mount escape byte"))?);
                at += 4;
            } else {
                raw.push(escaped[at]);
                at += 1;
            }
        }
        let point = PathBuf::from(OsString::from_vec(raw));
        if point != root && point.starts_with(root) {
            points.insert(point);
        }
    }
    Ok(points)
}
// Capture filesystem paths/kinds once, then stream this independent immutable
// traversal for each query. No Engine paths, Query matcher or inventory is used.
fn native_base(root: &Path, output: &Path, budget: &Budget) -> io::Result<(PathBuf, u64)> {
    let unsorted = output.join("filtered-native-base-unsorted.kinds");
    let sorted = output.join("filtered-native-base.kinds");
    let mut writer = ArtifactWriter::new(&unsorted, budget)?;
    let identity = fs::symlink_metadata(root)?;
    let boundaries = mount_boundaries(root)?;
    let mut todo = vec![root.to_path_buf()];
    let mut count = 0u64;
    let mut directories = 0;
    while let Some(directory) = todo.pop() {
        for child in fs::read_dir(directory)? {
            let path = child?.path();
            let meta = fs::symlink_metadata(&path)?;
            if meta.uid() != crate::process::uid() {
                return Err(invalid("filtered oracle foreign fixture owner"));
            }
            let kind = if meta.is_dir() {
                b'D'
            } else if meta.is_file() {
                b'F'
            } else if meta.file_type().is_symlink() {
                b'L'
            } else {
                return Err(invalid("filtered oracle special entry"));
            };
            if path.as_os_str().as_bytes().len() > 4096 {
                return Err(invalid("filtered oracle path bound"));
            }
            count += 1;
            if count > 2_000_000 {
                return Err(invalid("filtered oracle entry bound"));
            }
            writer.write(hex(path.as_os_str().as_bytes()).as_bytes())?;
            writer.write(&[b'\t', kind, b'\n'])?;
            if meta.is_dir() && meta.dev() == identity.dev() && !boundaries.contains(&path) {
                directories += 1;
                if directories > 65536 {
                    return Err(invalid("filtered oracle directory bound"));
                }
                todo.push(path);
            }
        }
    }
    writer.finish()?;
    let final_identity = fs::symlink_metadata(root)?;
    if (identity.dev(), identity.ino()) != (final_identity.dev(), final_identity.ino())
        || boundaries != mount_boundaries(root)?
    {
        return Err(invalid("filtered oracle source/scope changed"));
    }
    // Hex encodes raw bytes monotonically; sorting these sidecars therefore also
    // gives the exact raw-byte lexical order for subsequently filtered NUL paths.
    external_sort(&unsorted, &sorted, output, false, budget)?;
    remove_owned_artifact(output, &unsorted)?;
    Ok((sorted, count))
}
fn filtered_oracle(
    root: &Path,
    base: &Path,
    output: &Path,
    index: usize,
    query: &RawQuery,
    budget: &Budget,
) -> io::Result<(PathBuf, PathBuf, u64)> {
    let paths = output.join(format!("filtered-{index:03}-oracle.nul"));
    let kinds = output.join(format!("filtered-{index:03}-oracle.kinds"));
    let mut path_writer = ArtifactWriter::new(&paths, budget)?;
    let mut kind_writer = ArtifactWriter::new(&kinds, budget)?;
    let mut count = 0;
    let mut reader = BufReader::new(File::open(base)?);
    let mut line = Vec::new();
    loop {
        line.clear();
        // A canonical line is at most two hex bytes per path byte plus kind.
        let bytes = reader.by_ref().take(8196).read_until(b'\n', &mut line)?;
        if bytes == 0 {
            break;
        }
        if !line.ends_with(b"\n") {
            return Err(invalid("filtered native base line bound/truncation"));
        }
        let split = line
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| invalid("filtered native base kind record"))?;
        if line.len() != split + 3 || !matches!(line[split + 1], b'F' | b'D' | b'L') {
            return Err(invalid("filtered native base kind"));
        }
        let raw = unhex(
            std::str::from_utf8(&line[..split]).map_err(|_| invalid("filtered native hex"))?,
        )?;
        let path = Path::new(OsStr::from_bytes(&raw));
        let relative = path
            .strip_prefix(root)
            .map_err(|_| invalid("filtered native path scope"))?;
        let mut searched = vec![b'/'];
        searched.extend_from_slice(relative.as_os_str().as_bytes());
        if query.matches(&searched) {
            count += 1;
            path_writer.write(&raw)?;
            path_writer.write(&[0])?;
            kind_writer.write(&line)?;
        }
    }
    path_writer.finish()?;
    kind_writer.finish()?;
    Ok((paths, kinds, count))
}
fn check_job(reply: &Json, version: u64, count: u64) -> io::Result<()> {
    if reply.get("state")?.text()? != "Complete"
        || reply.get("version")?.number()? != version
        || reply.get("count")?.number()? != count
    {
        return Err(invalid(
            "independent query job version/exact-count mismatch",
        ));
    }
    Ok(())
}
fn representative_sort(
    worker: &mut Worker,
    query: &str,
    version: u64,
    count: u64,
    path: &Path,
    budget: &Budget,
) -> io::Result<Json> {
    let began = Instant::now();
    let admitted = worker.request(
        "SORT_START",
        &[hex(query.as_bytes())],
        Duration::from_secs(10),
    )?;
    let id = admitted.get("job_id")?.number()?;
    let result = (|| -> io::Result<Json> {
        let complete = worker.wait_job(id, Duration::from_secs(3600))?;
        check_job(&complete, version, count)?;
        let confirmed_ns = began.elapsed().as_nanos();
        let mut writer = ArtifactWriter::new(path, budget)?;
        let mut offset = 0u64;
        let mut previous = None::<Vec<u8>>;
        loop {
            let page = worker.request(
                "SORT_PAGE",
                &[id.to_string(), offset.to_string(), "50".into()],
                Duration::from_secs(30),
            )?;
            if page.get("version")?.number()? != version {
                return Err(invalid("sorted page changed snapshot"));
            }
            let rows = page.get("paths_hex")?.array()?;
            if rows.len() > 50 || (rows.is_empty() && !page.get("complete")?.boolean()?) {
                return Err(invalid("sorted page cardinality/progress"));
            }
            for row in rows {
                let raw = unhex(row.text()?)?;
                if raw.is_empty() || previous.as_ref().is_some_and(|old| old >= &raw) {
                    return Err(invalid("sort order/duplicate mismatch"));
                }
                writer.write(&raw)?;
                writer.write(&[0])?;
                previous = Some(raw);
                offset += 1;
                if offset > count {
                    return Err(invalid("sorted result exceeds exact count"));
                }
            }
            if page.get("complete")?.boolean()? {
                break;
            }
        }
        if offset != count {
            return Err(invalid("sorted result truncated"));
        }
        writer.finish()?;
        ensure_cut(worker, version)?;
        Ok(Json::object([
            ("job", complete),
            ("controller_confirmation_ns", n(confirmed_ns)),
            ("paged_export_ns", n(began.elapsed().as_nanos())),
            ("matched_rows", n(offset)),
        ]))
    })();
    if result.is_err() {
        let _ = worker.request("CANCEL", &[id.to_string()], Duration::from_secs(10));
    }
    let dropped = worker.request("OP_DROP", &[id.to_string()], Duration::from_secs(60));
    match result {
        Ok(reply) => {
            dropped?;
            Ok(reply)
        }
        Err(error) => {
            let _ = dropped;
            Err(error)
        }
    }
}
pub(super) fn verify_queries(
    worker: &mut Worker,
    queries: &[String],
    options: &Options,
    log: &mut Log,
    observed: &[FirstPage],
) -> io::Result<()> {
    if queries.len() != observed.len() || queries.is_empty() {
        return Err(invalid("filtered query suite/observations"));
    }
    let version = observed[0].version;
    if observed.iter().any(|page| page.version != version) {
        return Err(invalid("filtered query suite changed cut"));
    }
    ensure_cut(worker, version)?;
    let budget = Budget::with_checkpoint(
        &options.output,
        options
            .output_budget_bytes
            .saturating_sub(320 * 1024 * 1024),
        &options.database,
    )?;
    let (base, baseline_count) = native_base(&options.root, &options.output, &budget)?;
    ensure_cut(worker, version)?;
    for (index, query) in queries.iter().enumerate() {
        let began = Instant::now();
        let mut artifacts = Vec::new();
        let result = (|| -> io::Result<Json> {
            let (oracle, oracle_kinds, count) = filtered_oracle(
                &options.root,
                &base,
                &options.output,
                index,
                &RawQuery::parse(query),
                &budget,
            )?;
            artifacts.extend([oracle.clone(), oracle_kinds.clone()]);
            let page = worker.query(query)?;
            observed[index].check(&page)?;
            verify_first50(&page, version, count, &oracle)?;
            let exported = worker.operation(
                "EXPORT",
                &[
                    hex(format!("filtered-{index:03}-export.nul").as_bytes()),
                    hex(query.as_bytes()),
                ],
            )?;
            if !exported.get("complete")?.boolean()?
                || !exported.get("validated_start_finish")?.boolean()?
                || exported.get("version")?.number()? != version
                || exported.get("count")?.number()? != count
            {
                return Err(invalid("filtered EXPORT version/completeness/cardinality"));
            }
            let export = PathBuf::from(OsString::from_vec(unhex(
                exported.get("path_hex")?.text()?,
            )?));
            let kinds = PathBuf::from(OsString::from_vec(unhex(
                exported.get("kinds_path_hex")?.text()?,
            )?));
            let sorted = options
                .output
                .join(format!("filtered-{index:03}-export-sorted.nul"));
            let sorted_kinds = options
                .output
                .join(format!("filtered-{index:03}-export-sorted.kinds"));
            artifacts.extend([
                export.clone(),
                kinds.clone(),
                sorted.clone(),
                sorted_kinds.clone(),
            ]);
            if export.parent() != Some(options.output.as_path())
                || kinds.parent() != Some(options.output.as_path())
            {
                return Err(invalid("filtered EXPORT escaped artifact scope"));
            }
            external_sort(&export, &sorted, &options.output, true, &budget)?;
            external_sort(&kinds, &sorted_kinds, &options.output, false, &budget)?;
            equal_files(&sorted, &oracle)?;
            equal_files(&sorted_kinds, &oracle_kinds)?;
            let count_started = Instant::now();
            let counted = worker.operation("COUNT_START", &[hex(query.as_bytes())])?;
            let count_confirmed_ns = count_started.elapsed().as_nanos();
            check_job(&counted, version, count)?;
            log.record(
                "count-correctness",
                Json::object([
                    ("query_id", n(index)),
                    ("query", s(query.clone())),
                    ("controller_confirmation_ns", n(count_confirmed_ns)),
                    ("job", counted),
                    ("exact_native_oracle_count", n(count)),
                ]),
            )?;
            let mut sorted_job = Json::Null;
            if matches!(query.as_str(), "" | "report" | "invoice ext:txt") {
                let path = options
                    .output
                    .join(format!("filtered-{index:03}-sort-job.nul"));
                artifacts.push(path.clone());
                sorted_job = representative_sort(worker, query, version, count, &path, &budget)?;
                equal_files(&path, &oracle)?;
            }
            // Give the public monitor its normal cadence before the final cut
            // check; no direct fast-poll or private publication hook is used.
            thread::sleep(Duration::from_millis(40));
            ensure_cut(worker, version)?;
            Ok(Json::object([
                ("query_id", n(index)),
                ("query", s(query.clone())),
                ("version", n(version)),
                ("baseline_native_entries", n(baseline_count)),
                ("exact_matches", n(count)),
                ("first50_native_subset", b(true)),
                ("full_filtered_path_set_equal", b(true)),
                ("full_filtered_kind_set_equal", b(true)),
                ("count_exact", b(true)),
                ("oracle_sha256", s(file_digest(&oracle)?)),
                ("export_sha256", s(file_digest(&sorted)?)),
                ("kind_oracle_sha256", s(file_digest(&oracle_kinds)?)),
                ("sort", sorted_job),
                ("outside_hot_loop_ns", n(began.elapsed().as_nanos())),
            ]))
        })();
        match result {
            Ok(record) => {
                log.record("filtered-query-correctness", record)?;
                for path in artifacts {
                    remove_owned_artifact(&options.output, &path)?;
                }
            }
            Err(error) => {
                log.record(
                    "filtered-query-failure",
                    Json::object([
                        ("query_id", n(index)),
                        ("query", s(query.clone())),
                        ("version", n(version)),
                        ("error", s(error.to_string())),
                        ("failed_artifacts_retained", b(true)),
                    ]),
                )?;
                return Err(error);
            }
        }
    }
    remove_owned_artifact(&options.output, &base)?;
    log.record(
        "filtered-query-suite",
        Json::object([
            ("queries", n(queries.len())),
            ("version", n(version)),
            ("baseline_native_entries", n(baseline_count)),
            ("full_filtered_oracles_equal", b(true)),
            ("representative_sort_oracles_equal", b(true)),
            ("worker_jobs_dropped", b(true)),
        ]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{b, hex, n, s, Json};
    use std::fs;
    #[test]
    fn raw_query_oracle_preserves_and_path_unicode_invalid_runs_and_extensions() {
        for (query, path, expected) in [
            ("pre/ab ext:txt", b"/Pre/AB\xffcd.TXT".as_slice(), true),
            ("abcd", b"/Pre/ab\xffcd.txt", false),
            ("ab cd", b"/Pre/ab\xffcd.txt", true),
            ("ext:txt", b"/Pre/bad.tx\xfft", false),
            ("ext:txt ext:pdf", b"/report.pdf", true),
            ("EXT:txt", b"/report.txt", false),
            ("OR", b"/report.txt", true),
            ("NOT", b"/notes.txt", true),
            ("报告 ext:txt", "/报告/报告.TXT".as_bytes(), true),
            ("i̇", "/İ/file.txt".as_bytes(), true),
            ("ος/σοσ", "/ΟΣ/ΣΟΣ.md".as_bytes(), true),
            ("ος/σος", "/ΟΣ/ΣΟΣ.md".as_bytes(), false),
            ("ext:σ", "/A.Σ".as_bytes(), false),
            ("ext:σ", b"/\xff/A.\xce\xa3".as_slice(), true),
        ] {
            assert_eq!(RawQuery::parse(query).matches(path), expected, "{query}");
        }
    }
    #[test]
    fn partial_wrong_count_and_version_query_jobs_cannot_satisfy_the_oracle() {
        let reply = |state, version, count| {
            Json::object([
                ("state", s(state)),
                ("version", n(version)),
                ("count", n(count)),
            ])
        };
        assert!(check_job(&reply("Complete", 7u64, 2u64), 7, 2).is_ok());
        for bad in [
            reply("Running", 7u64, 2u64),
            reply("Complete", 6u64, 2u64),
            reply("Complete", 7u64, 1u64),
        ] {
            assert!(check_job(&bad, 7, 2).is_err());
        }
    }
    #[test]
    fn first_page_verification_rejects_stale_duplicate_missing_and_foreign_rows() {
        let path =
            std::env::temp_dir().join(format!("loci-verifier-oracle-{}", std::process::id()));
        fs::write(&path, b"/one\0/two\0").unwrap();
        let page = |version, validated, names: &[&[u8]]| {
            Json::object([
                ("version", n(version)),
                ("validated", b(validated)),
                ("cancelled", b(false)),
                ("status_start", s("Validated")),
                ("status_finish", s("Validated")),
                ("complete", b(true)),
                (
                    "paths_hex",
                    Json::Array(names.iter().map(|name| s(hex(name))).collect()),
                ),
            ])
        };
        assert!(verify_first50(&page(7u64, true, &[b"/two", b"/one"]), 7, 2, &path).is_ok());
        for invalid in [
            page(6u64, true, &[b"/one", b"/two"]),
            page(7u64, false, &[b"/one", b"/two"]),
            page(7u64, true, &[b"/one", b"/one"]),
            page(7u64, true, &[b"/one"]),
            page(7u64, true, &[b"/one", b"/foreign"]),
        ] {
            assert!(verify_first50(&invalid, 7, 2, &path).is_err());
        }
        fs::remove_file(path).unwrap();
    }
}
