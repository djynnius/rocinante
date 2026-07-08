//! Append-only JSONL session store. One file per session under
//! `.rocinante/sessions/<uuid>.jsonl`; one envelope per line. Resume replays
//! the records into the *compacted* message list (compaction records replace
//! the seq range they summarize) while the full history stays on disk.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use rocinante_providers::{Message, Role};

#[derive(Debug, Serialize, Deserialize)]
pub struct Envelope {
    pub seq: u64,
    pub ts: String,
    #[serde(flatten)]
    pub record: Record,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Record {
    Meta {
        session_id: Uuid,
        cwd: String,
        model: String,
    },
    Message {
        message: Message,
    },
    /// Summary replacing messages in seq range [from_seq, to_seq].
    Compaction {
        from_seq: u64,
        to_seq: u64,
        replacement: Vec<Message>,
    },
    ModeChange {
        mode: String,
    },
    ModelChange {
        model: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("session io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("corrupt session line {line}: {error}")]
    Corrupt { line: usize, error: String },
    #[error("no sessions found in {0}")]
    NoSessions(PathBuf),
}

pub struct SessionStore {
    pub id: Uuid,
    path: PathBuf,
    file: std::fs::File,
    seq: u64,
}

impl SessionStore {
    pub fn sessions_dir(project_dir: &Path) -> PathBuf {
        project_dir.join(".rocinante/sessions")
    }

    /// Start a fresh session file.
    pub fn create(project_dir: &Path, model: &str) -> Result<Self, SessionError> {
        let dir = Self::sessions_dir(project_dir);
        std::fs::create_dir_all(&dir)?;
        let id = Uuid::new_v4();
        let path = dir.join(format!("{id}.jsonl"));
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)?;
        let mut store = Self {
            id,
            path,
            file,
            seq: 0,
        };
        store.append(Record::Meta {
            session_id: id,
            cwd: project_dir.display().to_string(),
            model: model.to_string(),
        })?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&mut self, record: Record) -> Result<u64, SessionError> {
        self.seq += 1;
        let envelope = Envelope {
            seq: self.seq,
            ts: now_iso(),
            record,
        };
        let mut line = serde_json::to_string(&envelope).expect("records serialize");
        line.push('\n');
        self.file.write_all(line.as_bytes())?;
        self.file.sync_data()?;
        Ok(self.seq)
    }

    pub fn append_message(&mut self, message: &Message) -> Result<u64, SessionError> {
        self.append(Record::Message {
            message: message.clone(),
        })
    }

    pub fn last_seq(&self) -> u64 {
        self.seq
    }

    /// Open an existing session file and reconstruct the working context.
    pub fn resume(path: &Path) -> Result<(Self, Vec<Message>), SessionError> {
        let content = std::fs::read_to_string(path)?;
        let mut messages: Vec<(u64, Message)> = Vec::new();
        let mut id = Uuid::new_v4();
        let mut seq = 0;

        for (line_no, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let envelope: Envelope =
                serde_json::from_str(line).map_err(|e| SessionError::Corrupt {
                    line: line_no + 1,
                    error: e.to_string(),
                })?;
            seq = seq.max(envelope.seq);
            match envelope.record {
                Record::Meta { session_id, .. } => id = session_id,
                Record::Message { message } => messages.push((envelope.seq, message)),
                Record::Compaction {
                    from_seq,
                    to_seq,
                    replacement,
                } => {
                    messages.retain(|(s, _)| *s < from_seq || *s > to_seq);
                    // Splice the replacement where the range began.
                    let insert_at = messages.partition_point(|(s, _)| *s < from_seq);
                    for (offset, msg) in replacement.into_iter().enumerate() {
                        messages.insert(insert_at + offset, (from_seq, msg));
                    }
                }
                Record::ModeChange { .. } | Record::ModelChange { .. } => {}
            }
        }

        let file = std::fs::OpenOptions::new().append(true).open(path)?;
        let store = Self {
            id,
            path: path.to_path_buf(),
            file,
            seq,
        };
        Ok((store, messages.into_iter().map(|(_, m)| m).collect()))
    }

    /// Most recently modified session file for a project, if any.
    pub fn latest(project_dir: &Path) -> Result<PathBuf, SessionError> {
        let dir = Self::sessions_dir(project_dir);
        let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
        for entry in std::fs::read_dir(&dir).map_err(|_| SessionError::NoSessions(dir.clone()))? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "jsonl")
                && let Ok(meta) = entry.metadata()
                && let Ok(modified) = meta.modified()
                && newest.as_ref().is_none_or(|(t, _)| modified > *t)
            {
                newest = Some((modified, path));
            }
        }
        newest.map(|(_, p)| p).ok_or(SessionError::NoSessions(dir))
    }
}

fn now_iso() -> String {
    // Seconds since epoch — good enough for ordering; humans read the TUI.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    secs.to_string()
}

/// Convenience used by resume: does this reconstructed context end awaiting
/// an assistant reply (i.e. the session was cut mid-turn)?
pub fn ends_mid_turn(messages: &[Message]) -> bool {
    matches!(
        messages.last().map(|m| m.role),
        Some(Role::User) | Some(Role::Tool)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_resume() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SessionStore::create(dir.path(), "test-model").unwrap();
        store.append_message(&Message::system("sys")).unwrap();
        store.append_message(&Message::user("hello")).unwrap();
        store
            .append_message(&Message::assistant("hi there"))
            .unwrap();
        let path = store.path().to_path_buf();
        let original_id = store.id;
        drop(store);

        let (resumed, messages) = SessionStore::resume(&path).unwrap();
        assert_eq!(resumed.id, original_id);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1].content, "hello");
        assert!(!ends_mid_turn(&messages));
    }

    #[test]
    fn compaction_replaces_range_on_resume() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SessionStore::create(dir.path(), "m").unwrap();
        let sys = store.append_message(&Message::system("sys")).unwrap();
        let first = store.append_message(&Message::user("old work")).unwrap();
        let last_old = store
            .append_message(&Message::assistant("old result"))
            .unwrap();
        store.append_message(&Message::user("recent")).unwrap();
        assert_eq!(sys, 2); // meta is seq 1

        store
            .append(Record::Compaction {
                from_seq: first,
                to_seq: last_old,
                replacement: vec![Message::user("[summary of old work]")],
            })
            .unwrap();
        let path = store.path().to_path_buf();
        drop(store);

        let (_, messages) = SessionStore::resume(&path).unwrap();
        let contents: Vec<&str> = messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, vec!["sys", "[summary of old work]", "recent"]);
    }

    #[test]
    fn latest_finds_newest_session() {
        let dir = tempfile::tempdir().unwrap();
        let s1 = SessionStore::create(dir.path(), "m").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let s2 = SessionStore::create(dir.path(), "m").unwrap();
        let latest = SessionStore::latest(dir.path()).unwrap();
        assert_eq!(latest, s2.path());
        assert_ne!(latest, s1.path());
    }

    #[test]
    fn mid_turn_detection() {
        assert!(ends_mid_turn(&[Message::user("hi")]));
        assert!(!ends_mid_turn(&[Message::assistant("done")]));
    }
}
