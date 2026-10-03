//! Shared query implementation used by both the baseline CLI and live snapshots.
use crate::signatures::{trigram_signature, ShortSignature};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
pub const BLOCK: usize = 64;
#[derive(Clone, Copy)]
pub struct Record {
    parent: u32,
    offset: u32,
    len: u32,
}
pub struct Index {
    pub parents: Vec<String>,
    pub names: Vec<u8>,
    pub records: Vec<Record>,
    pub dictionary: Vec<(u32, u32, u32)>, // trigram, posting offset, posting count
    pub postings: Vec<u32>,               // block IDs, sorted; deliberately uncompressed
    pub record_signatures: Vec<u128>,
    pub short_signatures: Vec<ShortSignature>,
}
pub fn normalize(s: &str) -> String {
    s.to_lowercase()
}
pub(crate) fn gram(b: &[u8]) -> u32 {
    u32::from(b[0]) | u32::from(b[1]) << 8 | u32::from(b[2]) << 16
}
fn contains(hay: &[u8], needle: &[u8]) -> bool {
    needle.is_empty()
        || (hay.len() >= needle.len() && hay.windows(needle.len()).any(|x| x == needle))
}

impl Index {
    pub fn from_paths(paths: impl Iterator<Item = String>) -> Self {
        let mut index = Self {
            parents: vec![],
            names: vec![],
            records: vec![],
            dictionary: vec![],
            postings: vec![],
            record_signatures: vec![],
            short_signatures: vec![],
        };
        let mut parent_ids = HashMap::new();
        for path in paths {
            let (parent, name) = path.rsplit_once('/').unwrap_or(("", &path));
            let id = *parent_ids.entry(parent.to_owned()).or_insert_with(|| {
                let id = index.parents.len() as u32;
                index.parents.push(parent.to_owned());
                id
            });
            let offset = u32::try_from(index.names.len()).expect("experiment pool exceeds 4 GiB");
            index.names.extend_from_slice(name.as_bytes());
            index.records.push(Record {
                parent: id,
                offset,
                len: name.len() as u32,
            });
        }
        drop(parent_ids);
        let mut lists: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut grams = Vec::new();
        index.record_signatures.reserve_exact(index.records.len());
        index
            .short_signatures
            .reserve_exact(index.records.len().div_ceil(BLOCK));
        for block in 0..index.records.len().div_ceil(BLOCK) {
            grams.clear();
            let mut short = ShortSignature::default();
            for id in block * BLOCK..((block + 1) * BLOCK).min(index.records.len()) {
                let path = normalize(&index.path(id));
                let bytes = path.as_bytes();
                grams.extend(bytes.windows(3).map(gram));
                index.record_signatures.push(trigram_signature(bytes));
                short.insert(bytes);
            }
            index.short_signatures.push(short);
            grams.sort_unstable();
            grams.dedup();
            for key in &grams {
                lists.entry(*key).or_default().push(block as u32);
            }
        }
        let mut keys: Vec<_> = lists.keys().copied().collect();
        keys.sort_unstable();
        for key in keys {
            let list = lists.remove(&key).unwrap();
            index
                .dictionary
                .push((key, index.postings.len() as u32, list.len() as u32));
            index.postings.extend(list);
        }
        index.names.shrink_to_fit();
        index.records.shrink_to_fit();
        index.postings.shrink_to_fit();
        index.dictionary.shrink_to_fit();
        index
    }
    pub fn name(&self, id: usize) -> &str {
        let r = self.records[id];
        std::str::from_utf8(&self.names[r.offset as usize..(r.offset + r.len) as usize]).unwrap()
    }
    pub fn path(&self, id: usize) -> String {
        let r = self.records[id];
        format!("{}/{}", self.parents[r.parent as usize], self.name(id))
    }
    fn list(&self, key: u32) -> Option<&[u32]> {
        self.dictionary
            .binary_search_by_key(&key, |x| x.0)
            .ok()
            .map(|i| {
                let (_, start, len) = self.dictionary[i];
                &self.postings[start as usize..(start + len) as usize]
            })
    }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let mut w = BufWriter::new(File::create(path)?);
        w.write_all(b"LOCIEXP2")?;
        for n in [
            self.parents.len(),
            self.names.len(),
            self.records.len(),
            self.dictionary.len(),
            self.postings.len(),
        ] {
            write_u32(&mut w, u32::try_from(n).unwrap())?;
        }
        for p in &self.parents {
            write_u32(&mut w, p.len() as u32)?;
            w.write_all(p.as_bytes())?;
        }
        w.write_all(&self.names)?;
        for r in &self.records {
            for n in [r.parent, r.offset, r.len] {
                write_u32(&mut w, n)?;
            }
        }
        for (k, s, n) in &self.dictionary {
            for v in [*k, *s, *n] {
                write_u32(&mut w, v)?;
            }
        }
        for p in &self.postings {
            write_u32(&mut w, *p)?;
        }
        for signature in &self.record_signatures {
            w.write_all(&signature.to_le_bytes())?;
        }
        for signature in &self.short_signatures {
            for word in signature.bytes {
                w.write_all(&word.to_le_bytes())?;
            }
            w.write_all(&signature.pairs.to_le_bytes())?;
        }
        w.flush()?;
        w.get_ref().sync_all()
    }
    pub fn load(path: &Path) -> io::Result<Self> {
        // Trusted local experiment files only. Production needs size/checksum validation.
        let mut r = BufReader::new(File::open(path)?);
        let mut magic = [0; 8];
        r.read_exact(&mut magic)?;
        if &magic != b"LOCIEXP2" {
            return Err(io::Error::other("unsupported experiment index"));
        }
        let np = read_u32(&mut r)? as usize;
        let nn = read_u32(&mut r)? as usize;
        let nr = read_u32(&mut r)? as usize;
        let nd = read_u32(&mut r)? as usize;
        let nl = read_u32(&mut r)? as usize;
        let mut parents = Vec::with_capacity(np);
        for _ in 0..np {
            let len = read_u32(&mut r)? as usize;
            let mut b = vec![0; len];
            r.read_exact(&mut b)?;
            parents.push(String::from_utf8(b).map_err(io::Error::other)?);
        }
        let mut names = vec![0; nn];
        r.read_exact(&mut names)?;
        let mut records = Vec::with_capacity(nr);
        for _ in 0..nr {
            records.push(Record {
                parent: read_u32(&mut r)?,
                offset: read_u32(&mut r)?,
                len: read_u32(&mut r)?,
            });
        }
        let mut dictionary = Vec::with_capacity(nd);
        for _ in 0..nd {
            dictionary.push((read_u32(&mut r)?, read_u32(&mut r)?, read_u32(&mut r)?));
        }
        let mut postings = Vec::with_capacity(nl);
        for _ in 0..nl {
            postings.push(read_u32(&mut r)?);
        }
        let mut record_signatures = Vec::with_capacity(nr);
        for _ in 0..nr {
            record_signatures.push(read_u128(&mut r)?);
        }
        let mut short_signatures = Vec::with_capacity(nr.div_ceil(BLOCK));
        for _ in 0..nr.div_ceil(BLOCK) {
            let mut bytes = [0u64; 4];
            for word in &mut bytes {
                let mut b = [0; 8];
                r.read_exact(&mut b)?;
                *word = u64::from_le_bytes(b);
            }
            short_signatures.push(ShortSignature {
                bytes,
                pairs: read_u128(&mut r)?,
            });
        }
        Ok(Self {
            parents,
            names,
            records,
            dictionary,
            postings,
            record_signatures,
            short_signatures,
        })
    }
    pub fn search(
        &self,
        query: &Query,
        indexed: bool,
        limit: usize,
        cancel: &AtomicBool,
        progress: &AtomicUsize,
    ) -> Search {
        if cancel.load(Ordering::Relaxed) {
            return Search {
                cancelled: true,
                ..Search::default()
            };
        }
        let mut lists = Vec::new();
        let mut signature = 0u128;
        let mut short_query = ShortSignature::default();
        for token in &query.tokens {
            signature |= trigram_signature(token.as_bytes());
            if token.len() < 3 {
                short_query.insert(token.as_bytes());
            }
        }
        if let Some(ext) = &query.ext {
            signature |= trigram_signature(format!(".{ext}").as_bytes());
        }
        if indexed {
            for token in &query.tokens {
                for key in token.as_bytes().windows(3).map(gram) {
                    match self.list(key) {
                        Some(l) => lists.push(l),
                        None => return Search::default(),
                    }
                }
            }
            if let Some(ext) = &query.ext {
                let token = format!(".{ext}");
                for key in token.as_bytes().windows(3).map(gram) {
                    match self.list(key) {
                        Some(l) => lists.push(l),
                        None => return Search::default(),
                    }
                }
            }
        }
        lists.sort_unstable_by_key(|l| l.len());
        let all_blocks = self.records.len().div_ceil(BLOCK);
        let mut output = Search::default();
        let blocks = lists.first().map_or(all_blocks, |x| x.len());
        for pos in 0..blocks {
            if cancel.load(Ordering::Relaxed) {
                output.cancelled = true;
                break;
            }
            let block = lists.first().map_or(pos, |x| x[pos] as usize);
            if indexed && !self.short_signatures[block].contains(&short_query) {
                continue;
            }
            if lists
                .iter()
                .skip(1)
                .any(|l| l.binary_search(&(block as u32)).is_err())
            {
                continue;
            }
            for id in block * BLOCK..((block + 1) * BLOCK).min(self.records.len()) {
                // At most 64 record verifications between cooperative cancellation checks.
                output.checked += 1;
                if output.checked.is_multiple_of(1024) {
                    progress.store(output.checked, Ordering::Release);
                }
                if indexed && self.record_signatures[id] & signature != signature {
                    continue;
                }
                output.verified += 1;
                if !query.matches(&self.path(id)) {
                    continue;
                }
                output.matches += 1;
                if output.ids.len() < 50 {
                    output.ids.push(id as u32);
                }
                output.checksum = output.checksum.wrapping_add(id as u64 + 1);
                if output.matches >= limit {
                    return output;
                }
            }
        }
        output
    }
}
#[derive(Default)]
pub struct Search {
    pub ids: Vec<u32>,
    pub matches: usize,
    pub checked: usize,
    pub verified: usize,
    pub cancelled: bool,
    pub checksum: u64,
}
pub struct Query {
    tokens: Vec<String>,
    ext: Option<String>,
}
impl Query {
    pub fn parse(raw: &str) -> Self {
        let mut q = Self {
            tokens: vec![],
            ext: None,
        };
        for t in raw.split_whitespace() {
            if let Some(ext) = t.strip_prefix("ext:") {
                q.ext = Some(normalize(ext));
            } else {
                q.tokens.push(normalize(t));
            }
        }
        q
    }
    /// Match valid UTF-8 runs separately, preserving invalid filesystem bytes.
    /// Each AND term may occur in any run; a term never spans an invalid byte.
    pub fn matches_raw(&self, path: &[u8]) -> bool {
        if let Ok(text) = std::str::from_utf8(path) {
            return self.matches(text);
        }
        let mut remainder = path;
        let mut runs = Vec::new();
        while !remainder.is_empty() {
            match std::str::from_utf8(remainder) {
                Ok(text) => {
                    runs.push(normalize(text));
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    if valid > 0 {
                        runs.push(normalize(std::str::from_utf8(&remainder[..valid]).unwrap()));
                    }
                    let invalid = error.error_len().unwrap_or(remainder.len() - valid);
                    remainder = &remainder[valid + invalid..];
                }
            }
        }
        self.tokens.iter().all(|token| {
            runs.iter()
                .any(|run| contains(run.as_bytes(), token.as_bytes()))
        }) && self.ext.as_ref().is_none_or(|ext| {
            let name = path.rsplit(|byte| *byte == b'/').next().unwrap_or(path);
            let Some(dot) = name.iter().rposition(|byte| *byte == b'.') else {
                return false;
            };
            std::str::from_utf8(&name[dot + 1..]).is_ok_and(|suffix| normalize(suffix) == *ext)
        })
    }
    pub fn matches(&self, path: &str) -> bool {
        let path = normalize(path);
        self.tokens
            .iter()
            .all(|t| contains(path.as_bytes(), t.as_bytes()))
            && self.ext.as_ref().is_none_or(|e| {
                path.rsplit_once('/')
                    .unwrap_or(("", &path))
                    .1
                    .rsplit_once('.')
                    .is_some_and(|(_, ext)| ext == e)
            })
    }
}
fn write_u32(w: &mut impl Write, n: u32) -> io::Result<()> {
    w.write_all(&n.to_le_bytes())
}
fn read_u128(r: &mut impl Read) -> io::Result<u128> {
    let mut b = [0; 16];
    r.read_exact(&mut b)?;
    Ok(u128::from_le_bytes(b))
}
fn read_u32(r: &mut impl Read) -> io::Result<u32> {
    let mut b = [0; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}
