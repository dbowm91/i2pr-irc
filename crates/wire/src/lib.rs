//! Bounded byte-oriented IRC/IRCv3 wire primitives.
use std::collections::BTreeMap;

pub const MAX_LINE_BYTES: usize = 512; // Includes CRLF.
pub const MAX_TAGGED_LINE_BYTES: usize = 8191; // Includes CRLF.
pub const MAX_TAG_BYTES: usize = 4094;
pub const MAX_PARAMS: usize = 15;
pub const MAX_TAGS: usize = 64;
pub const MAX_TOKEN_BYTES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    Empty,
    MissingTerminator,
    TooLong,
    InvalidFraming,
    InvalidCommand,
    TooManyParams,
    TooManyTags,
    InvalidTagEscape,
    TokenTooLong,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub tags: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    pub prefix: Option<Vec<u8>>,
    pub command: Vec<u8>,
    pub params: Vec<Vec<u8>>,
}

impl Message {
    pub fn parse(line: &[u8]) -> Result<Self, WireError> {
        let tagged = line.first() == Some(&b'@');
        let limit = if tagged {
            MAX_TAGGED_LINE_BYTES
        } else {
            MAX_LINE_BYTES
        };
        if line.len() > limit {
            return Err(WireError::TooLong);
        }
        if !line.ends_with(b"\r\n") {
            return Err(WireError::MissingTerminator);
        }
        let body = &line[..line.len() - 2];
        if body.is_empty() {
            return Err(WireError::Empty);
        }
        if body.iter().any(|b| *b == 0 || *b == b'\r' || *b == b'\n') {
            return Err(WireError::InvalidFraming);
        }
        let mut rest = body;
        let mut tags = BTreeMap::new();
        if rest.first() == Some(&b'@') {
            let end = rest
                .iter()
                .position(|b| *b == b' ')
                .ok_or(WireError::InvalidFraming)?;
            if end > MAX_TAG_BYTES {
                return Err(WireError::TooLong);
            }
            for field in rest[1..end].split(|b| *b == b';') {
                if field.is_empty() {
                    continue;
                }
                let (k, v) = match field.iter().position(|b| *b == b'=') {
                    Some(i) => (field[..i].to_vec(), Some(unescape_tag(&field[i + 1..])?)),
                    None => (field.to_vec(), None),
                };
                if k.is_empty()
                    || k.iter()
                        .any(|b| b.is_ascii_whitespace() || *b == b';' || *b == b'=')
                {
                    return Err(WireError::InvalidFraming);
                }
                tags.insert(k, v);
                if tags.len() > MAX_TAGS {
                    return Err(WireError::TooManyTags);
                }
            }
            rest = &rest[end + 1..];
        }
        let mut prefix = None;
        if rest.first() == Some(&b':') {
            let end = rest
                .iter()
                .position(|b| *b == b' ')
                .ok_or(WireError::InvalidFraming)?;
            if end < 2 {
                return Err(WireError::InvalidFraming);
            }
            prefix = Some(rest[1..end].to_vec());
            rest = &rest[end + 1..];
        }
        let end = rest.iter().position(|b| *b == b' ').unwrap_or(rest.len());
        let command = rest[..end].to_vec();
        if command.is_empty()
            || command.len() > MAX_TOKEN_BYTES
            || !command.iter().all(|b| b.is_ascii_alphanumeric())
        {
            return Err(WireError::InvalidCommand);
        }
        rest = if end < rest.len() {
            &rest[end + 1..]
        } else {
            &[]
        };
        let mut params = Vec::new();
        while !rest.is_empty() {
            if params.len() == MAX_PARAMS {
                return Err(WireError::TooManyParams);
            }
            if rest[0] == b':' {
                params.push(rest[1..].to_vec());
                break;
            }
            let e = rest.iter().position(|b| *b == b' ').unwrap_or(rest.len());
            let p = rest[..e].to_vec();
            if p.len() > MAX_TOKEN_BYTES {
                return Err(WireError::TokenTooLong);
            }
            params.push(p);
            rest = if e < rest.len() { &rest[e + 1..] } else { &[] };
        }
        Ok(Self {
            tags,
            prefix,
            command,
            params,
        })
    }
    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        let mut out = Vec::new();
        if !self.tags.is_empty() {
            out.push(b'@');
            for (i, (k, v)) in self.tags.iter().enumerate() {
                if i > 0 {
                    out.push(b';')
                }
                out.extend(k);
                if let Some(v) = v {
                    out.push(b'=');
                    out.extend(escape_tag(v));
                }
            }
            out.push(b' ');
        }
        if let Some(p) = &self.prefix {
            out.push(b':');
            out.extend(p);
            out.push(b' ')
        }
        out.extend(&self.command);
        for (i, p) in self.params.iter().enumerate() {
            out.push(b' ');
            if i + 1 == self.params.len() && (p.is_empty() || p.contains(&b' ')) {
                out.push(b':')
            }
            out.extend(p);
        }
        out.extend(b"\r\n");
        let lim = if self.tags.is_empty() {
            MAX_LINE_BYTES
        } else {
            MAX_TAGGED_LINE_BYTES
        };
        if out.len() > lim {
            return Err(WireError::TooLong);
        }
        Ok(out)
    }
}
fn unescape_tag(v: &[u8]) -> Result<Vec<u8>, WireError> {
    let mut o = Vec::new();
    let mut i = 0;
    while i < v.len() {
        if v[i] == b'\\' {
            i += 1;
            if i == v.len() {
                break;
            }
            o.push(match v[i] {
                b':' => b';',
                b's' => b' ',
                b'\\' => b'\\',
                b'r' => b'\r',
                b'n' => b'\n',
                x => x,
            });
        } else {
            o.push(v[i]);
        }
        i += 1;
    }
    Ok(o)
}
fn escape_tag(v: &[u8]) -> Vec<u8> {
    let mut o = Vec::new();
    for b in v {
        o.extend_from_slice(match b {
            b';' => b"\\:",
            b' ' => b"\\s",
            b'\\' => b"\\\\",
            b'\r' => b"\\r",
            b'\n' => b"\\n",
            _ => std::slice::from_ref(b),
        });
    }
    o
}

#[derive(Debug, Default)]
pub struct LineDecoder {
    buf: Vec<u8>,
    dropping: bool,
}
impl LineDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Result<Vec<u8>, WireError>> {
        let mut out = Vec::new();
        for b in bytes {
            if self.dropping {
                if *b == b'\n' {
                    self.dropping = false;
                }
                continue;
            }
            self.buf.push(*b);
            let cap = if self.buf.first() == Some(&b'@') {
                MAX_TAGGED_LINE_BYTES
            } else {
                MAX_LINE_BYTES
            };
            if self.buf.len() > cap {
                self.buf.clear();
                self.dropping = true;
                out.push(Err(WireError::TooLong));
                continue;
            }
            if *b == b'\n' {
                let line = std::mem::take(&mut self.buf);
                out.push(Message::parse(&line).map(|_| line));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_unknown_command_and_tag() {
        let m = Message::parse(b"@x=one\\stwo :nick PRIVMSG #c :hello world\r\n").unwrap();
        assert_eq!(Message::parse(&m.encode().unwrap()).unwrap(), m);
    }
    #[test]
    fn incremental_and_oversize_discard() {
        let mut d = LineDecoder::default();
        assert!(d.push(b"PING :x\r").is_empty());
        assert_eq!(d.push(b"\n").len(), 1);
        let mut x = vec![b'A'; MAX_LINE_BYTES + 1];
        x.extend_from_slice(b"\r\n");
        assert!(d.push(&x).iter().any(Result::is_err));
        assert_eq!(d.push(b"PING :ok\r\n").len(), 1);
    }
    #[test]
    fn tag_escape_vectors() {
        assert_eq!(unescape_tag(b"a\\:b\\sc\\r\\n").unwrap(), b"a;b c\r\n");
    }
}
