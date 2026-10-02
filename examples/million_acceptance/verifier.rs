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
            || paths.iter().any(Vec::is_empty)
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
        ] {
            assert_eq!(RawQuery::parse(query).matches(path), expected, "{query}");
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
