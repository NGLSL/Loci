use std::collections::BTreeMap;
use std::io::{self, Read, Write};

pub const MAX_FRAME: usize = 65536;
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}
impl Json {
    pub fn object(fields: impl IntoIterator<Item = (impl Into<String>, Json)>) -> Self {
        Self::Object(fields.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }
    pub fn get(&self, key: &str) -> io::Result<&Self> {
        match self {
            Self::Object(value) => value.get(key).ok_or_else(|| invalid("missing reply field")),
            _ => Err(invalid("reply is not an object")),
        }
    }
    pub fn number(&self) -> io::Result<u64> {
        match self {
            Self::Number(v) => v.parse().map_err(|_| invalid("reply integer")),
            _ => Err(invalid("reply is not integer")),
        }
    }
    pub fn text(&self) -> io::Result<&str> {
        match self {
            Self::String(v) => Ok(v),
            _ => Err(invalid("reply is not string")),
        }
    }
    pub fn boolean(&self) -> io::Result<bool> {
        match self {
            Self::Bool(v) => Ok(*v),
            _ => Err(invalid("reply is not boolean")),
        }
    }
    pub fn array(&self) -> io::Result<&[Json]> {
        match self {
            Self::Array(v) => Ok(v),
            _ => Err(invalid("reply is not array")),
        }
    }
    pub fn encode(&self) -> String {
        match self {
            Self::Null => "null".into(),
            Self::Bool(v) => v.to_string(),
            Self::Number(v) => v.clone(),
            Self::String(v) => quote(v),
            Self::Array(v) => format!(
                "[{}]",
                v.iter().map(Self::encode).collect::<Vec<_>>().join(",")
            ),
            Self::Object(v) => format!(
                "{{{}}}",
                v.iter()
                    .map(|(k, v)| format!("{}:{}", quote(k), v.encode()))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        }
    }
    pub fn parse(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() > MAX_FRAME {
            return Err(invalid("JSON exceeds protocol bound"));
        }
        let mut p = Parser { bytes, at: 0 };
        let value = p.value(0)?;
        p.space();
        if p.at != bytes.len() {
            return Err(invalid("trailing JSON"));
        }
        Ok(value)
    }
}
pub fn n(value: impl TryInto<u64>) -> Json {
    Json::Number(
        value
            .try_into()
            .ok()
            .expect("measurement fits u64")
            .to_string(),
    )
}
pub fn s(value: impl Into<String>) -> Json {
    Json::String(value.into())
}
pub fn b(value: bool) -> Json {
    Json::Bool(value)
}
pub fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
pub fn quote(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < '\u{20}' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
pub fn hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for x in bytes {
        out.push(TABLE[(x >> 4) as usize] as char);
        out.push(TABLE[(x & 15) as usize] as char);
    }
    out
}
pub fn unhex(value: &str) -> io::Result<Vec<u8>> {
    if value.len() % 2 != 0 || value.len() > 8192 {
        return Err(invalid("hex argument bound"));
    }
    fn digit(x: u8) -> io::Result<u8> {
        match x {
            b'0'..=b'9' => Ok(x - b'0'),
            b'a'..=b'f' => Ok(x - b'a' + 10),
            _ => Err(invalid("invalid lowercase hex")),
        }
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|p| Ok(digit(p[0])? * 16 + digit(p[1])?))
        .collect()
}
pub fn frame_read(input: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match input.read(&mut len[..1])? {
        0 => return Ok(None),
        1 => (),
        _ => unreachable!(),
    }
    input.read_exact(&mut len[1..])?;
    let size = u32::from_le_bytes(len) as usize;
    if size == 0 || size > MAX_FRAME {
        return Err(invalid("frame length bound"));
    }
    let mut bytes = vec![0u8; size];
    input.read_exact(&mut bytes)?;
    Ok(Some(bytes))
}
pub fn frame_write(output: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        return Err(invalid("frame length bound"));
    }
    output.write_all(&(bytes.len() as u32).to_le_bytes())?;
    output.write_all(bytes)?;
    output.flush()
}
struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Parser<'_> {
    fn space(&mut self) {
        while self.bytes.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }
    fn take(&mut self, expected: u8) -> io::Result<()> {
        self.space();
        if self.bytes.get(self.at) != Some(&expected) {
            return Err(invalid("JSON syntax"));
        }
        self.at += 1;
        Ok(())
    }
    fn value(&mut self, depth: usize) -> io::Result<Json> {
        if depth > 16 {
            return Err(invalid("JSON nesting bound"));
        }
        self.space();
        match self
            .bytes
            .get(self.at)
            .copied()
            .ok_or_else(|| invalid("truncated JSON"))?
        {
            b'"' => Ok(Json::String(self.string()?)),
            b'{' => {
                self.at += 1;
                self.space();
                let mut fields = BTreeMap::new();
                if self.bytes.get(self.at) == Some(&b'}') {
                    self.at += 1;
                    return Ok(Json::Object(fields));
                }
                loop {
                    self.space();
                    let key = self.string()?;
                    self.take(b':')?;
                    if fields.insert(key, self.value(depth + 1)?).is_some() {
                        return Err(invalid("duplicate JSON key"));
                    }
                    self.space();
                    match self.bytes.get(self.at) {
                        Some(b'}') => {
                            self.at += 1;
                            break;
                        }
                        Some(b',') => self.at += 1,
                        _ => return Err(invalid("JSON object syntax")),
                    }
                }
                Ok(Json::Object(fields))
            }
            b'[' => {
                self.at += 1;
                self.space();
                let mut values = Vec::new();
                if self.bytes.get(self.at) == Some(&b']') {
                    self.at += 1;
                    return Ok(Json::Array(values));
                }
                loop {
                    values.push(self.value(depth + 1)?);
                    self.space();
                    match self.bytes.get(self.at) {
                        Some(b']') => {
                            self.at += 1;
                            break;
                        }
                        Some(b',') => self.at += 1,
                        _ => return Err(invalid("JSON array syntax")),
                    }
                }
                Ok(Json::Array(values))
            }
            b't' => {
                self.word(b"true")?;
                Ok(Json::Bool(true))
            }
            b'f' => {
                self.word(b"false")?;
                Ok(Json::Bool(false))
            }
            b'n' => {
                self.word(b"null")?;
                Ok(Json::Null)
            }
            b'0'..=b'9' => {
                let start = self.at;
                while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
                    self.at += 1;
                }
                let v = std::str::from_utf8(&self.bytes[start..self.at])
                    .map_err(|_| invalid("JSON number"))?;
                if v.len() > 1 && v.starts_with('0') {
                    return Err(invalid("JSON leading zero"));
                };
                v.parse::<u64>()
                    .map_err(|_| invalid("JSON integer overflow"))?;
                Ok(Json::Number(v.into()))
            }
            _ => Err(invalid("unsupported JSON value")),
        }
    }
    fn word(&mut self, value: &[u8]) -> io::Result<()> {
        if self.bytes.get(self.at..self.at + value.len()) != Some(value) {
            return Err(invalid("JSON literal"));
        }
        self.at += value.len();
        Ok(())
    }
    fn string(&mut self) -> io::Result<String> {
        self.take(b'"')?;
        let mut bytes = Vec::new();
        loop {
            let x = *self
                .bytes
                .get(self.at)
                .ok_or_else(|| invalid("unterminated JSON string"))?;
            self.at += 1;
            match x {
                b'"' => return String::from_utf8(bytes).map_err(|_| invalid("JSON UTF8")),
                b'\\' => {
                    let esc = *self
                        .bytes
                        .get(self.at)
                        .ok_or_else(|| invalid("JSON escape"))?;
                    self.at += 1;
                    match esc {
                        b'"' | b'\\' | b'/' => bytes.push(esc),
                        b'n' => bytes.push(b'\n'),
                        b'r' => bytes.push(b'\r'),
                        b't' => bytes.push(b'\t'),
                        b'b' => bytes.push(8),
                        b'f' => bytes.push(12),
                        b'u' => {
                            let mut cp = self.codepoint()?;
                            if (0xd800..=0xdbff).contains(&cp) {
                                if self.bytes.get(self.at..self.at + 2) != Some(b"\\u") {
                                    return Err(invalid("JSON surrogate"));
                                };
                                self.at += 2;
                                let low = self.codepoint()?;
                                if !(0xdc00..=0xdfff).contains(&low) {
                                    return Err(invalid("JSON surrogate"));
                                };
                                cp = 0x10000 + ((cp - 0xd800) << 10) + (low - 0xdc00);
                            }
                            let ch = char::from_u32(cp).ok_or_else(|| invalid("JSON codepoint"))?;
                            let mut tmp = [0; 4];
                            bytes.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
                        }
                        _ => return Err(invalid("JSON escape")),
                    }
                }
                0..=31 => return Err(invalid("JSON control byte")),
                x => bytes.push(x),
            }
        }
    }
    fn codepoint(&mut self) -> io::Result<u32> {
        let part = self
            .bytes
            .get(self.at..self.at + 4)
            .ok_or_else(|| invalid("JSON codepoint"))?;
        let text = std::str::from_utf8(part).map_err(|_| invalid("JSON codepoint"))?;
        let n = u32::from_str_radix(text, 16).map_err(|_| invalid("JSON codepoint"))?;
        self.at += 4;
        Ok(n)
    }
}
