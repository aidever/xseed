//! 最小 bencode 解析器：只做解析，并记录每个值在原文中的字节范围（用于计算 info_hash）。

use std::collections::BTreeMap;
use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Int(i64),
    Bytes(Vec<u8>),
    List(Vec<Node>),
    Dict(BTreeMap<Vec<u8>, Node>),
}

/// 一个值及其在原文中的位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub value: Value,
    pub span: Range<usize>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("bencode 数据意外结束")]
    Eof,
    #[error("bencode 语法错误，位置 {0}")]
    Syntax(usize),
    #[error("bencode 嵌套过深")]
    TooDeep,
    #[error("bencode 末尾有多余数据")]
    Trailing,
}

const MAX_DEPTH: usize = 64;

pub fn parse(buf: &[u8]) -> Result<Node, Error> {
    let mut p = Parser { buf, pos: 0 };
    let n = p.node(0)?;
    if p.pos != buf.len() {
        return Err(Error::Trailing);
    }
    Ok(n)
}

struct Parser<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Result<u8, Error> {
        self.buf.get(self.pos).copied().ok_or(Error::Eof)
    }

    fn node(&mut self, depth: usize) -> Result<Node, Error> {
        if depth > MAX_DEPTH {
            return Err(Error::TooDeep);
        }
        let start = self.pos;
        let value = match self.peek()? {
            b'i' => {
                self.pos += 1;
                let end = self.find(b'e')?;
                let s = std::str::from_utf8(&self.buf[self.pos..end]).map_err(|_| Error::Syntax(self.pos))?;
                if s.is_empty() || s == "-0" || (s.len() > 1 && s.starts_with('0')) || s.starts_with("-0") {
                    return Err(Error::Syntax(self.pos));
                }
                let v: i64 = s.parse().map_err(|_| Error::Syntax(self.pos))?;
                self.pos = end + 1;
                Value::Int(v)
            }
            b'l' => {
                self.pos += 1;
                let mut items = Vec::new();
                while self.peek()? != b'e' {
                    items.push(self.node(depth + 1)?);
                }
                self.pos += 1;
                Value::List(items)
            }
            b'd' => {
                self.pos += 1;
                let mut map = BTreeMap::new();
                while self.peek()? != b'e' {
                    let key = self.bytes()?;
                    let v = self.node(depth + 1)?;
                    map.insert(key, v);
                }
                self.pos += 1;
                Value::Dict(map)
            }
            b'0'..=b'9' => Value::Bytes(self.bytes()?),
            _ => return Err(Error::Syntax(self.pos)),
        };
        Ok(Node { value, span: start..self.pos })
    }

    fn bytes(&mut self) -> Result<Vec<u8>, Error> {
        let colon = self.find(b':')?;
        let s = std::str::from_utf8(&self.buf[self.pos..colon]).map_err(|_| Error::Syntax(self.pos))?;
        if s.is_empty() || (s.len() > 1 && s.starts_with('0')) {
            return Err(Error::Syntax(self.pos));
        }
        let len: usize = s.parse().map_err(|_| Error::Syntax(self.pos))?;
        let start = colon + 1;
        let end = start.checked_add(len).ok_or(Error::Eof)?;
        if end > self.buf.len() {
            return Err(Error::Eof);
        }
        self.pos = end;
        Ok(self.buf[start..end].to_vec())
    }

    fn find(&self, c: u8) -> Result<usize, Error> {
        self.buf[self.pos..]
            .iter()
            .position(|&b| b == c)
            .map(|i| self.pos + i)
            .ok_or(Error::Eof)
    }
}

impl Node {
    pub fn get(&self, key: &str) -> Option<&Node> {
        match &self.value {
            Value::Dict(m) => m.get(key.as_bytes()),
            _ => None,
        }
    }
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match &self.value {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        self.as_bytes().and_then(|b| std::str::from_utf8(b).ok())
    }
    pub fn as_int(&self) -> Option<i64> {
        match self.value {
            Value::Int(i) => Some(i),
            _ => None,
        }
    }
    pub fn as_list(&self) -> Option<&[Node]> {
        match &self.value {
            Value::List(l) => Some(l),
            _ => None,
        }
    }
    pub fn as_dict(&self) -> Option<&BTreeMap<Vec<u8>, Node>> {
        match &self.value {
            Value::Dict(m) => Some(m),
            _ => None,
        }
    }
}

/// 编码（测试与生成种子用）。
pub fn encode(v: &Value, out: &mut Vec<u8>) {
    match v {
        Value::Int(i) => out.extend_from_slice(format!("i{i}e").as_bytes()),
        Value::Bytes(b) => {
            out.extend_from_slice(format!("{}:", b.len()).as_bytes());
            out.extend_from_slice(b);
        }
        Value::List(l) => {
            out.push(b'l');
            for n in l {
                encode(&n.value, out);
            }
            out.push(b'e');
        }
        Value::Dict(m) => {
            out.push(b'd');
            for (k, n) in m {
                encode(&Value::Bytes(k.clone()), out);
                encode(&n.value, out);
            }
            out.push(b'e');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_and_spans() {
        let src = b"d3:fooi42e4:infod4:name1:xee";
        let n = parse(src).unwrap();
        assert_eq!(n.get("foo").unwrap().as_int(), Some(42));
        let info = n.get("info").unwrap();
        assert_eq!(&src[info.span.clone()], b"d4:name1:xe");
        assert_eq!(info.get("name").unwrap().as_str(), Some("x"));
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(parse(b"i03e"), Err(Error::Syntax(1)));
        assert!(parse(b"i-0e").is_err());
        assert_eq!(parse(b"5:abc"), Err(Error::Eof));
        assert_eq!(parse(b"i1ei2e"), Err(Error::Trailing));
        assert!(parse(&[b'l'; 100]).is_err());
    }

    #[test]
    fn roundtrip() {
        let src = b"d1:ali1ei2ee1:b3:xyze";
        let n = parse(src).unwrap();
        let mut out = Vec::new();
        encode(&n.value, &mut out);
        assert_eq!(out, src);
    }
}
