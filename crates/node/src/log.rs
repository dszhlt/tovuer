//! The operation log: every operation of every author, in order, kept in
//! memory and appended to `ops.jsonl` in the room folder.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::ops::{Key, Op};
use crate::util::hex;

#[derive(Debug, Default)]
pub struct OpLog {
    by_author: HashMap<Key, Vec<Op>>,
    file: Option<PathBuf>,
}

/// Result of offering an operation to the log.
#[derive(Debug, PartialEq, Eq)]
pub enum Offer {
    Added,
    Duplicate,
    /// Operations in between are missing — ask the peer for them.
    Gap,
    WriteFailed(String),
}

impl OpLog {
    /// Open the log stored at `path` (missing file — empty log).
    pub fn open(path: &Path) -> Self {
        let mut log = OpLog { by_author: HashMap::new(), file: Some(path.to_path_buf()) };
        if let Ok(text) = std::fs::read_to_string(path) {
            let mut ops: Vec<Op> = text.lines().filter_map(|line| serde_json::from_str::<Op>(line).ok()).collect();
            ops.sort_by_key(|op| op.seq);
            for op in ops {
                log.insert(op);
            }
        }
        log
    }

    /// Everything, oldest first: the order to fold the room in.
    pub fn sorted(&self) -> Vec<&Op> {
        let mut all: Vec<&Op> = self.by_author.values().flatten().collect();
        all.sort_by(|a, b| (a.ts, &a.author, a.seq).cmp(&(b.ts, &b.author, b.seq)));
        all
    }

    pub fn in_memory() -> Self {
        OpLog::default()
    }

    pub fn head(&self, author: &Key) -> u64 {
        self.by_author.get(author).map_or(0, |ops| ops.len() as u64)
    }

    /// Last operation number from every author.
    pub fn heads(&self) -> BTreeMap<String, u64> {
        self.by_author.iter().map(|(k, ops)| (hex(k), ops.len() as u64)).collect()
    }

    fn insert(&mut self, op: Op) -> Offer {
        let Some(author) = op.author_key() else { return Offer::Duplicate };
        let ops = self.by_author.entry(author).or_default();
        let have = ops.len() as u64;
        if op.seq <= have {
            Offer::Duplicate
        } else if op.seq > have + 1 {
            Offer::Gap
        } else {
            ops.push(op);
            Offer::Added
        }
    }

    /// Add a verified operation; written to disk when it is new.
    pub fn offer(&mut self, op: &Op) -> Offer {
        let Some(author) = op.author_key() else { return Offer::Duplicate };
        let head = self.head(&author);
        if op.seq <= head {
            return Offer::Duplicate;
        }
        if op.seq != head + 1 {
            return Offer::Gap;
        }
        if let Some(path) = &self.file {
            let append = || -> std::io::Result<()> {
                let mut line = serde_json::to_vec(op)?;
                line.push(b'\n');
                let mut file = std::fs::OpenOptions::new().create(true).read(true).append(true).open(path)?;
                file.lock()?;
                let previous_len = file.metadata()?.len();
                if let Err(e) = file.write_all(&line).and_then(|()| file.sync_data()) {
                    // Discard a partial record so retrying the same sequence is safe.
                    file.set_len(previous_len)?;
                    return Err(e);
                }
                Ok(())
            };
            if let Err(e) = append() {
                return Offer::WriteFailed(format!("cannot save operation to {}: {e}", path.display()));
            }
        }
        self.by_author.entry(author).or_default().push(op.clone());
        Offer::Added
    }

    /// Operations the other side does not have yet, judging by its heads.
    pub fn missing_for(&self, theirs: &BTreeMap<String, u64>) -> Vec<Op> {
        let mut out = Vec::new();
        for (author, ops) in &self.by_author {
            let have = theirs.get(&hex(author)).copied().unwrap_or(0) as usize;
            if have < ops.len() {
                out.extend_from_slice(&ops[have..]);
            }
        }
        // oldest first, so the other side folds them in a sensible order
        out.sort_by_key(|op| (op.ts, op.seq));
        out
    }

    /// The other side has operations we lack.
    pub fn behind(&self, theirs: &BTreeMap<String, u64>) -> bool {
        theirs.iter().any(|(author, seq)| {
            crate::util::unhex::<32>(author).is_some_and(|key| *seq > self.head(&key))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::Body;
    use crate::room::RoomKeys;
    use iroh::SecretKey;

    fn op(secret: &SecretKey, seq: u64) -> Op {
        Op::new(secret, &RoomKeys::derive("t", ""), seq, seq * 10, "a", &Body::Delete { id: seq })
    }

    #[test]
    fn write_failure_does_not_consume_sequence() {
        let dir = std::env::temp_dir().join(format!("tovuer-log-retry-{}", std::process::id()));
        let path = dir.join("ops.jsonl");
        let mut log = OpLog::open(&path);
        let key = SecretKey::from_bytes(&[1; 32]);
        let first = op(&key, 1);
        assert!(matches!(log.offer(&first), Offer::WriteFailed(_)));
        assert_eq!(log.head(key.public().as_bytes()), 0);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(log.offer(&first), Offer::Added);
        assert_eq!(log.offer(&op(&key, 2)), Offer::Added);
        assert_eq!(OpLog::open(&path).head(key.public().as_bytes()), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn keeps_order_and_finds_gaps() {
        let dir = std::env::temp_dir().join(format!("tovuer-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ops.jsonl");
        let (a, b) = (SecretKey::from_bytes(&[1; 32]), SecretKey::from_bytes(&[2; 32]));
        let mut log = OpLog::open(&path);
        assert!(log.sorted().is_empty());
        assert_eq!(log.offer(&op(&a, 1)), Offer::Added);
        assert_eq!(log.offer(&op(&a, 1)), Offer::Duplicate);
        assert_eq!(log.offer(&op(&a, 3)), Offer::Gap);
        assert_eq!(log.offer(&op(&a, 2)), Offer::Added);
        assert_eq!(log.offer(&op(&b, 1)), Offer::Added);

        let mut theirs = BTreeMap::new();
        theirs.insert(hex(a.public().as_bytes()), 1);
        let missing: Vec<u64> = log.missing_for(&theirs).iter().map(|o| o.seq).collect();
        assert_eq!(missing, vec![1, 2], "b#1 (ts 10) then a#2 (ts 20)");
        assert!(!log.behind(&theirs));
        theirs.insert(hex(a.public().as_bytes()), 5);
        assert!(log.behind(&theirs));

        let reopened = OpLog::open(&path);
        assert_eq!(reopened.sorted().len(), 3);
        assert_eq!(reopened.head(a.public().as_bytes()), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
