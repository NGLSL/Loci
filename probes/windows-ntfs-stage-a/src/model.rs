//! Bounded decoding of documented USN name records, without Unicode conversion.
use std::io;

pub const MAX_BATCH_BYTES: usize = 64 * 1024;
pub const MAX_RECORDS: usize = 2048;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Record {
    pub object: u128,
    pub parent: u128,
    pub usn: i64,
    pub reason: u32,
    pub attributes: u32,
    pub name: Vec<u16>,
    pub major: u16,
}

/// A name relationship, distinct from the object identity. One USN record is
/// not evidence that every hard-link name for an object has been discovered.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct EntryId {
    pub parent: u128,
    pub object: u128,
    pub name: Vec<u16>,
}

impl Record {
    pub fn entry_id(&self) -> EntryId {
        EntryId {
            parent: self.parent,
            object: self.object,
            name: self.name.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Journal {
    pub id: u64,
    pub first: i64,
    pub next: i64,
    pub lowest: i64,
    pub min_major: u16,
    pub max_major: u16,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn u16_at(b: &[u8], n: usize) -> u16 {
    u16::from_le_bytes(b[n..n + 2].try_into().unwrap())
}
fn u32_at(b: &[u8], n: usize) -> u32 {
    u32::from_le_bytes(b[n..n + 4].try_into().unwrap())
}
fn i64_at(b: &[u8], n: usize) -> i64 {
    i64::from_le_bytes(b[n..n + 8].try_into().unwrap())
}

/// Input consists only of records: strip the API's initial 8-byte cursor first.
/// Reject the whole batch on error; never return a misleading partial result.
pub fn decode_records(bytes: &[u8]) -> io::Result<Vec<Record>> {
    if bytes.len() > MAX_BATCH_BYTES {
        return Err(invalid("USN batch exceeds 64 KiB budget"));
    }
    let mut records = Vec::new();
    let mut remaining = bytes;
    while !remaining.is_empty() {
        if records.len() == MAX_RECORDS {
            return Err(invalid("USN batch exceeds record budget"));
        }
        if remaining.len() < 8 {
            return Err(invalid("truncated USN common header"));
        }
        let length = u32_at(remaining, 0) as usize;
        let major = u16_at(remaining, 4);
        let minor = u16_at(remaining, 6);
        let (header, usn_offset, reason_offset, attr_offset, name_offset) = match major {
            2 => (60, 24, 40, 52, 56),
            3 => (76, 40, 56, 68, 72),
            4 => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "USN V4 extent records have no filename; rebuild or use a supported source",
                ))
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("unsupported USN record version {major}.{minor}"),
                ))
            }
        };
        if minor != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("unsupported USN record version {major}.{minor}"),
            ));
        }
        if length < header || length > remaining.len() || length % 8 != 0 {
            return Err(invalid("invalid or truncated USN record length/alignment"));
        }
        let b = &remaining[..length];
        let name_len = u16_at(b, name_offset) as usize;
        let offset = u16_at(b, name_offset + 2) as usize;
        if name_len == 0
            || name_len % 2 != 0
            || name_len / 2 > 255
            || offset < header
            || offset % 2 != 0
            || offset.checked_add(name_len).is_none_or(|end| end > length)
        {
            return Err(invalid("invalid USN UTF-16 filename length/offset"));
        }
        let (object, parent) = if major == 2 {
            (
                u64::from_le_bytes(b[8..16].try_into().unwrap()) as u128,
                u64::from_le_bytes(b[16..24].try_into().unwrap()) as u128,
            )
        } else {
            (
                u128::from_le_bytes(b[8..24].try_into().unwrap()),
                u128::from_le_bytes(b[24..40].try_into().unwrap()),
            )
        };
        let name = b[offset..offset + name_len]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        records.push(Record {
            object,
            parent,
            usn: i64_at(b, usn_offset),
            reason: u32_at(b, reason_offset),
            attributes: u32_at(b, attr_offset),
            name,
            major,
        });
        remaining = &remaining[length..];
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record(major: u16, name: &[u16]) -> Vec<u8> {
        let header = if major == 2 { 60 } else { 76 };
        let len = (header + name.len() * 2 + 7) & !7;
        let mut b = vec![0; len];
        b[..4].copy_from_slice(&(len as u32).to_le_bytes());
        b[4..6].copy_from_slice(&major.to_le_bytes());
        b[8] = 7;
        let parent = if major == 2 { 16 } else { 24 };
        b[parent] = 9;
        let n = header - 4;
        b[n..n + 2].copy_from_slice(&((name.len() * 2) as u16).to_le_bytes());
        b[n + 2..n + 4].copy_from_slice(&(header as u16).to_le_bytes());
        for (i, c) in name.iter().enumerate() {
            b[header + i * 2..header + i * 2 + 2].copy_from_slice(&c.to_le_bytes());
        }
        b
    }
    #[test]
    fn preserves_raw_utf16_and_v3_identity() {
        for major in [2, 3] {
            let mut b = record(major, &[0xd800, 0x0061]);
            if major == 3 {
                b[23] = 0x80;
            }
            let r = decode_records(&b).unwrap().remove(0);
            assert_eq!(r.name, [0xd800, 0x0061]);
            assert_eq!(r.parent, 9);
            assert_eq!(r.object, if major == 3 { (0x80u128 << 120) | 7 } else { 7 });
        }
    }
    #[test]
    fn rejects_bad_records_and_versions() {
        let b = record(2, &[65]);
        assert!(decode_records(&b[..b.len() - 1]).is_err());
        for (offset, value) in [(0, 0u8), (58, 2), (56, 3), (6, 1), (4, 4), (4, 9)] {
            let mut malformed = b.clone();
            malformed[offset] = value;
            assert!(decode_records(&malformed).is_err());
        }
        assert!(decode_records(&vec![0; MAX_BATCH_BYTES + 1]).is_err());
        assert!(decode_records(&record(2, &vec![65; 256])).is_err());
        let mut batch = b.clone();
        batch.extend([0u8; 3]);
        assert!(decode_records(&batch).is_err());
    }
    #[test]
    fn hardlinks_are_distinct_entries() {
        let a = decode_records(&record(2, &[65])).unwrap().remove(0);
        let b = decode_records(&record(2, &[66])).unwrap().remove(0);
        assert_eq!(a.object, b.object);
        assert_ne!(a.entry_id(), b.entry_id());
        let mut c = a.clone();
        c.parent += 1;
        assert_ne!(a.entry_id(), c.entry_id());
    }
}
