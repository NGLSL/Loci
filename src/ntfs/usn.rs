//! Bounded decoding of documented USN name records, without Unicode conversion.
use std::io;

pub const MAX_BATCH_BYTES: usize = 64 * 1024;
pub const MAX_RECORDS: usize = 2048;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Record {
    pub object: u64,
    pub parent: u64,
    pub usn: i64,
    pub reason: u32,
    pub attributes: u32,
    pub name: Vec<u16>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Journal {
    pub id: u64,
    pub first: i64,
    pub next: i64,
    pub lowest: i64,
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
            3 => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "USN V3 identities are outside the legacy V2 namespace",
                ));
            }
            4 => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "USN V4 extent records have no filename; rebuild or use a supported source",
                ));
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("unsupported USN record version {major}.{minor}"),
                ));
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
        let object = u64::from_le_bytes(b[8..16].try_into().unwrap());
        let parent = u64::from_le_bytes(b[16..24].try_into().unwrap());
        let name: Vec<u16> = b[offset..offset + name_len]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        if name.iter().any(|&c| c == 0 || c == 47 || c == 92) {
            return Err(invalid("USN name contains a NUL or path separator"));
        }
        records.push(Record {
            object,
            parent,
            usn: i64_at(b, usn_offset),
            reason: u32_at(b, reason_offset),
            attributes: u32_at(b, attr_offset),
            name,
        });
        remaining = &remaining[length..];
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record(name: &[u16]) -> Vec<u8> {
        let len = (60 + name.len() * 2 + 7) & !7;
        let mut b = vec![0u8; len];
        b[..4].copy_from_slice(&(len as u32).to_le_bytes());
        b[4..6].copy_from_slice(&2u16.to_le_bytes());
        b[8..16].copy_from_slice(&7u64.to_le_bytes());
        b[16..24].copy_from_slice(&9u64.to_le_bytes());
        b[56..58].copy_from_slice(&((name.len() * 2) as u16).to_le_bytes());
        b[58..60].copy_from_slice(&60u16.to_le_bytes());
        for (i, c) in name.iter().enumerate() {
            b[60 + i * 2..62 + i * 2].copy_from_slice(&c.to_le_bytes());
        }
        b
    }
    #[test]
    fn preserves_raw_utf16() {
        let r = decode_records(&record(&[0xd800, 65])).unwrap().remove(0);
        assert_eq!(r.name, [0xd800, 65]);
        assert_eq!((r.object, r.parent), (7, 9));
    }
    #[test]
    fn rejects_malformed_and_nonlegacy_batches() {
        let b = record(&[65]);
        assert!(decode_records(&b[..b.len() - 1]).is_err());
        for (offset, value) in [(0, 0u8), (58, 2), (56, 3), (6, 1), (4, 3), (4, 4), (4, 9)] {
            let mut broken = b.clone();
            broken[offset] = value;
            assert!(decode_records(&broken).is_err());
        }
        assert!(decode_records(&record(&[0])).is_err());
        assert!(decode_records(&record(&[92])).is_err());
        assert!(decode_records(&record(&vec![65; 256])).is_err());
        assert!(decode_records(&vec![0; MAX_BATCH_BYTES + 1]).is_err());
        let mut batch = b;
        batch.extend([0; 3]);
        assert!(decode_records(&batch).is_err());
    }
}
