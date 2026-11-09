//! 种子元数据解析与文件列表规范化（03 §1.1.1）。

use crate::bencode::{self, Node};
use sha1::Digest as _;
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error(transparent)]
    Bencode(#[from] bencode::Error),
    #[error("缺少 info 字典")]
    NoInfo,
    #[error("缺少 name")]
    NoName,
    #[error("文件路径无效")]
    InvalidPath,
    #[error("没有任何文件")]
    Empty,
    #[error("文件长度无效")]
    BadLength,
}

/// 规范化后的文件列表：`(路径, 长度)`，路径不含根目录名、用 `/` 连接、NFC。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entries {
    pub root_name: String,
    pub single_file: bool,
    pub files: Vec<(String, u64)>,
}

impl Entries {
    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.1).sum()
    }
}

#[derive(Debug, Clone)]
pub struct Torrent {
    pub ih1: Option<[u8; 20]>,
    pub ih2: Option<[u8; 32]>,
    pub entries: Entries,
    pub comment: Option<String>,
    pub source: Option<String>,
    pub announces: Vec<String>,
    pub private: bool,
}

fn nfc(s: &str) -> String {
    s.nfc().collect()
}

fn text<'a>(d: &'a Node, key: &str) -> Option<&'a str> {
    d.get(&format!("{key}.utf-8")).and_then(Node::as_str).or_else(|| d.get(key).and_then(Node::as_str))
}

fn check_component(c: &str) -> Result<String, Error> {
    if c.is_empty() || c == "." || c == ".." || c.contains('\0') || c.contains('/') || c.contains('\\') {
        return Err(Error::InvalidPath);
    }
    Ok(nfc(c))
}

fn length(n: Option<&Node>) -> Result<u64, Error> {
    let v = n.and_then(Node::as_int).ok_or(Error::BadLength)?;
    u64::try_from(v).map_err(|_| Error::BadLength)
}

pub fn parse(buf: &[u8]) -> Result<Torrent, Error> {
    let root = bencode::parse(buf)?;
    let info = root.get("info").ok_or(Error::NoInfo)?;
    let raw = &buf[info.span.clone()];
    let v2 = info.get("meta version").and_then(Node::as_int) == Some(2);
    let v1 = info.get("pieces").is_some();
    let ih1 = v1.then(|| sha1::Sha1::digest(raw).into());
    let ih2 = v2.then(|| sha2::Sha256::digest(raw).into());
    let entries = normalized_entries(info)?;

    let mut announces = Vec::new();
    if let Some(a) = root.get("announce").and_then(Node::as_str) {
        announces.push(a.to_string());
    }
    for tier in root.get("announce-list").and_then(Node::as_list).unwrap_or(&[]) {
        for a in tier.as_list().unwrap_or(&[]) {
            if let Some(s) = a.as_str() {
                if !announces.iter().any(|x| x == s) {
                    announces.push(s.to_string());
                }
            }
        }
    }
    Ok(Torrent {
        ih1,
        ih2,
        entries,
        comment: text(&root, "comment").map(str::to_string),
        source: info.get("source").and_then(Node::as_str).map(str::to_string),
        announces,
        private: info.get("private").and_then(Node::as_int) == Some(1),
    })
}

/// 03 §1.1.1 `normalized_entries`。
pub fn normalized_entries(info: &Node) -> Result<Entries, Error> {
    let name = text(info, "name").ok_or(Error::NoName)?;
    let root_name = check_component(name)?;

    if info.get("length").is_some() && info.get("files").is_none() {
        return Ok(Entries { root_name: root_name.clone(), single_file: true, files: vec![(root_name, length(info.get("length"))?)] });
    }

    let mut out = Vec::new();
    if let Some(files) = info.get("files").and_then(Node::as_list) {
        for f in files {
            let pad = f.get("attr").and_then(Node::as_bytes).is_some_and(|a| a.contains(&b'p'));
            let path = f
                .get("path.utf-8")
                .and_then(Node::as_list)
                .or_else(|| f.get("path").and_then(Node::as_list))
                .ok_or(Error::InvalidPath)?;
            let comps: Vec<&str> = path.iter().map(|c| c.as_str().ok_or(Error::InvalidPath)).collect::<Result<_, _>>()?;
            if pad || comps.first() == Some(&".pad") {
                continue;
            }
            if comps.is_empty() {
                return Err(Error::InvalidPath);
            }
            let p = comps.iter().map(|c| check_component(c)).collect::<Result<Vec<_>, _>>()?.join("/");
            out.push((p, length(f.get("length"))?));
        }
    } else if let Some(tree) = info.get("file tree") {
        let top = tree.as_dict().ok_or(Error::InvalidPath)?;
        // v2 单文件：file tree 只有一个顶层键，且它直接是文件
        if top.len() == 1 {
            let (_, n) = top.iter().next().unwrap();
            if let Some(leaf) = n.get("") {
                return Ok(Entries { root_name: root_name.clone(), single_file: true, files: vec![(root_name, length(leaf.get("length"))?)] });
            }
        }
        walk_tree(tree, &mut Vec::new(), &mut out)?;
    } else {
        return Err(Error::Empty);
    }
    if out.is_empty() {
        return Err(Error::Empty);
    }
    Ok(Entries { root_name, single_file: false, files: out })
}

fn walk_tree(n: &Node, prefix: &mut Vec<String>, out: &mut Vec<(String, u64)>) -> Result<(), Error> {
    let d = n.as_dict().ok_or(Error::InvalidPath)?;
    for (k, child) in d {
        if k.is_empty() {
            continue;
        }
        let c = std::str::from_utf8(k).map_err(|_| Error::InvalidPath)?;
        prefix.push(check_component(c)?);
        if let Some(leaf) = child.get("") {
            if prefix.first().map(String::as_str) != Some(".pad") {
                out.push((prefix.join("/"), length(leaf.get("length"))?));
            }
        } else {
            walk_tree(child, prefix, out)?;
        }
        prefix.pop();
    }
    Ok(())
}

/// 从下载器给出的文件列表构造（无法拿到原始种子时使用）。
/// `files` 中的路径**包含**根目录名（qB `torrents/files` 在 Original 布局下的形式）。
pub fn entries_from_file_list(root_name: &str, files: &[(String, u64)], single_file: bool) -> Result<Entries, Error> {
    let root = check_component(root_name)?;
    if single_file {
        let (p, len) = files.first().ok_or(Error::Empty)?;
        let leaf = p.rsplit(['/', '\\']).next().unwrap_or(p);
        return Ok(Entries { root_name: root, single_file: true, files: vec![(check_component(leaf)?, *len)] });
    }
    let mut out = Vec::new();
    for (p, len) in files {
        let comps: Vec<&str> = p.split(['/', '\\']).collect();
        let rel = match comps.split_first() {
            Some((first, rest)) if nfc(first) == root && !rest.is_empty() => rest,
            _ => return Err(Error::InvalidPath),
        };
        if rel.first() == Some(&".pad") {
            continue;
        }
        let joined = rel.iter().map(|c| check_component(c)).collect::<Result<Vec<_>, _>>()?.join("/");
        out.push((joined, *len));
    }
    if out.is_empty() {
        return Err(Error::Empty);
    }
    Ok(Entries { root_name: root, single_file: false, files: out })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::bencode::{encode, Value};
    use std::collections::BTreeMap;

    fn b(s: &str) -> Node {
        Node { value: Value::Bytes(s.as_bytes().to_vec()), span: 0..0 }
    }
    fn i(v: i64) -> Node {
        Node { value: Value::Int(v), span: 0..0 }
    }
    fn l(v: Vec<Node>) -> Node {
        Node { value: Value::List(v), span: 0..0 }
    }
    fn d(v: Vec<(&str, Node)>) -> Node {
        Node { value: Value::Dict(v.into_iter().map(|(k, n)| (k.as_bytes().to_vec(), n)).collect::<BTreeMap<_, _>>()), span: 0..0 }
    }

    /// 生成一个 v1 多文件测试种子。
    pub fn make_v1(name: &str, files: &[(&[&str], i64)], announce: &str, comment: &str) -> Vec<u8> {
        let fl = files.iter().map(|(p, len)| d(vec![("length", i(*len)), ("path", l(p.iter().map(|c| b(c)).collect()))])).collect();
        let info = d(vec![("files", l(fl)), ("name", b(name)), ("piece length", i(16384)), ("pieces", b("01234567890123456789")), ("private", i(1))]);
        let root = d(vec![("announce", b(announce)), ("comment", b(comment)), ("info", info)]);
        let mut out = Vec::new();
        encode(&root.value, &mut out);
        out
    }

    #[test]
    fn v1_multi() {
        let t = parse(&make_v1("Show", &[(&["S01", "e1.mkv"], 10), (&[".pad", "1"], 5), (&["e2.mkv"], 20)], "https://t.example/a?passkey=abc", "https://x.example/details.php?id=7")).unwrap();
        assert!(t.ih1.is_some() && t.ih2.is_none());
        assert_eq!(t.entries.files, vec![("S01/e1.mkv".into(), 10), ("e2.mkv".into(), 20)]);
        assert_eq!(t.entries.root_name, "Show");
        assert!(!t.entries.single_file && t.private);
        assert_eq!(t.announces, vec!["https://t.example/a?passkey=abc"]);
    }

    #[test]
    fn v1_single_and_nfc() {
        let info = d(vec![("length", i(99)), ("name", b("Cafe\u{301}.flac")), ("pieces", b("x"))]);
        let e = normalized_entries(&info).unwrap();
        assert!(e.single_file);
        assert_eq!(e.files, vec![("Café.flac".into(), 99)]);
    }

    #[test]
    fn rejects_traversal() {
        let info = d(vec![("files", l(vec![d(vec![("length", i(1)), ("path", l(vec![b(".."), b("x")]))])])), ("name", b("r"))]);
        assert_eq!(normalized_entries(&info), Err(Error::InvalidPath));
    }

    #[test]
    fn v2_tree_matches_v1() {
        let leaf = |n| d(vec![("", d(vec![("length", i(n))]))]);
        let tree = d(vec![("S01", d(vec![("e1.mkv", leaf(10))])), ("e2.mkv", leaf(20))]);
        let info = d(vec![("file tree", tree), ("meta version", i(2)), ("name", b("Show"))]);
        let e = normalized_entries(&info).unwrap();
        assert_eq!(e.files, vec![("S01/e1.mkv".into(), 10), ("e2.mkv".into(), 20)]);
    }

    #[test]
    fn from_file_list() {
        let e = entries_from_file_list("Show", &[("Show/S01/e1.mkv".into(), 10), ("Show/e2.mkv".into(), 20)], false).unwrap();
        assert_eq!(e.files, vec![("S01/e1.mkv".into(), 10), ("e2.mkv".into(), 20)]);
        assert!(entries_from_file_list("Show", &[("Other/e.mkv".into(), 1)], false).is_err());
    }
}
