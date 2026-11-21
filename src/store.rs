//! Persistence.
//!
//! Everything lives in ONE human-readable JSON file, so a backup is just
//! `cp solves.json somewhere/`. Design choices:
//!
//!  * `version` field   -> future schema migrations
//!  * `event` per solve -> other puzzles (2x2, 4x4, ...) can share the file;
//!                         solves of events this build doesn't know are
//!                         preserved untouched when saving
//!  * stable unique `id` -> backups from several machines can be merged
//!  * raw `time_ms` + separate `penalty` -> toggling +2 / DNF is lossless
//!  * atomic writes (tmp file + fsync + rename + directory fsync) -> a crash
//!    can't corrupt it
//!  * a `.bak` copy of the previous file is made on every start

use rand::Rng;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const EVENT_333: &str = "333";
const FORMAT: &str = "rcube";
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Penalty {
    None,
    Plus2,
    Dnf,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Solve {
    /// Sortable unique id: 12 hex digits of unix-ms + 8 random hex digits.
    pub id: String,
    pub event: String,
    /// Raw measured time in milliseconds (penalty NOT included).
    pub time_ms: u64,
    pub penalty: Penalty,
    pub scramble: String,
    /// Unix timestamp in milliseconds when the solve finished.
    pub timestamp: u64,
}

impl Solve {
    pub fn new(time_ms: u64, penalty: Penalty, scramble: String) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let id = format!("{:012x}{:08x}", timestamp, rand::thread_rng().gen::<u32>());
        Solve {
            id,
            event: EVENT_333.to_string(),
            time_ms,
            penalty,
            scramble,
            timestamp,
        }
    }

    /// Raw time in centiseconds (truncated, like official timers).
    pub fn raw_cs(&self) -> u64 {
        self.time_ms / 10
    }

    /// Time used for statistics; `None` means DNF.
    pub fn effective_cs(&self) -> Option<u64> {
        match self.penalty {
            Penalty::None => Some(self.raw_cs()),
            Penalty::Plus2 => Some(self.raw_cs() + 200),
            Penalty::Dnf => None,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct FileData {
    format: String,
    version: u32,
    #[serde(default)]
    solves: Vec<Solve>,
}

pub struct Store {
    path: Option<PathBuf>,
    /// Solves of other events: kept verbatim and written back on save.
    foreign: Vec<Solve>,
}

impl Store {
    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share"))
            })
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("rcube").join("solves.json")
    }

    /// A store that never touches the disk (used in tests).
    #[cfg(test)]
    pub fn memory() -> Self {
        Store { path: None, foreign: Vec::new() }
    }

    pub fn open(path: PathBuf) -> Result<(Store, Vec<Solve>), Box<dyn Error>> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut store = Store { path: Some(path.clone()), foreign: Vec::new() };
        if !path.exists() {
            return Ok((store, Vec::new()));
        }

        let text = fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let data: FileData = serde_json::from_str(&text).map_err(|e| {
            format!(
                "{} is not a valid rcube file ({e}).\n\
                 Refusing to start so your data is not overwritten; fix or move the file.",
                path.display()
            )
        })?;
        if data.format != FORMAT || data.version > VERSION {
            return Err(format!(
                "{}: unsupported file (format {:?}, version {}). Please update rcube.",
                path.display(),
                data.format,
                data.version
            )
            .into());
        }

        // Safety net: keep a copy of what we loaded.
        let _ = fs::copy(&path, path.with_extension("json.bak"));

        let (mine, foreign): (Vec<_>, Vec<_>) =
            data.solves.into_iter().partition(|s| s.event == EVENT_333);
        store.foreign = foreign;
        Ok((store, mine))
    }

    pub fn save(&self, solves: &[Solve]) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let mut all: Vec<Solve> = self.foreign.iter().chain(solves.iter()).cloned().collect();
        all.sort_by_key(|s| s.timestamp); // stable: keeps order of equal timestamps
        let data = FileData { format: FORMAT.to_string(), version: VERSION, solves: all };
        let json = serde_json::to_string_pretty(&data)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

        let tmp = path.with_extension("json.tmp");
        let result = (|| {
            let mut f = File::create(&tmp)?;
            f.write_all(json.as_bytes())?;
            f.write_all(b"\n")?;
            f.sync_all()?;
            drop(f);
            fs::rename(&tmp, path)
        })();
        match result {
            Ok(()) => {
                sync_dir(path);
                Ok(())
            }
            Err(e) => {
                // Don't leave a half-written temp file behind.
                let _ = fs::remove_file(&tmp);
                Err(e)
            }
        }
    }
}

/// Best effort: make the rename itself durable. Failure is not an error.
fn sync_dir(path: &Path) {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("rcube-test-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&p);
        p.push("solves.json");
        p
    }

    #[test]
    fn roundtrip_and_foreign_events_survive() {
        let path = tmp_path("roundtrip");
        let (store, solves) = Store::open(path.clone()).unwrap();
        assert!(solves.is_empty());

        let mut a = Solve::new(12_345, Penalty::Plus2, "R U R' U'".into());
        a.timestamp = 1;
        let mut other = Solve::new(999, Penalty::None, "x".into());
        other.event = "222".into();
        other.timestamp = 0;

        // simulate a file that already holds a 2x2 solve
        let mut s2 = Store { path: Some(path.clone()), foreign: vec![other.clone()] };
        s2.save(&[a.clone()]).unwrap();
        drop(store);
        s2.foreign.clear();

        let (reopened, mine) = Store::open(path.clone()).unwrap();
        assert_eq!(mine, vec![a.clone()]);
        assert_eq!(reopened.foreign, vec![other]);
        assert!(path.with_extension("json.bak").exists());
        assert!(!path.with_extension("json.tmp").exists());

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"penalty\": \"plus2\""));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn corrupt_file_is_never_overwritten() {
        let path = tmp_path("corrupt");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{ not json").unwrap();
        assert!(Store::open(path.clone()).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ not json");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn effective_times() {
        let mut s = Solve::new(12_349, Penalty::None, String::new());
        assert_eq!(s.effective_cs(), Some(1234));
        s.penalty = Penalty::Plus2;
        assert_eq!(s.effective_cs(), Some(1434));
        s.penalty = Penalty::Dnf;
        assert_eq!(s.effective_cs(), None);
    }
}
