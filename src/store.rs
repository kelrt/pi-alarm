//! Saving alarms to disk.
//!
//! The alarms live in one small JSON file, `alarms.json`, in the data
//! directory. Two things make this safe on a Pi that may lose power at any
//! moment and that has two programs (the daemon and the CLI) touching it:
//!
//! * **Atomic writes.** We write a temporary file, flush it to the SD card,
//!   then rename it over the old one. A rename either happens completely or
//!   not at all, so the file is never half-written.
//! * **A lock file.** Every change is "read the list, change it, write it
//!   back". The lock makes sure two processes never do that at the same
//!   time, which would lose one of the changes.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

use crate::alarm::{Alarm, MAX_ALARMS};

/// Version of the file layout. Increase it if the layout ever changes in a
/// way old code cannot read.
const FILE_VERSION: u32 = 1;

/// The JSON document in `alarms.json`.
#[derive(Debug, Serialize, Deserialize)]
struct AlarmFile {
    version: u32,
    alarms: Vec<Alarm>,
}

/// Access to the alarms file. Cheap to create; holds only paths.
#[derive(Debug, Clone)]
pub struct AlarmStore {
    path: PathBuf,
    lock_path: PathBuf,
}

/// Returned by `put` so callers can tell "created" from "replaced".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PutOutcome {
    Created,
    Replaced,
}

impl AlarmStore {
    /// A store that keeps its files in `data_dir`.
    pub fn new(data_dir: &Path) -> AlarmStore {
        AlarmStore {
            path: data_dir.join("alarms.json"),
            lock_path: data_dir.join("alarms.lock"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read all alarms. A missing file simply means "no alarms yet".
    pub fn load(&self) -> anyhow::Result<Vec<Alarm>> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            // `kind()` tells apart "file not found" from real I/O errors.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", self.path.display()))
            }
        };
        let file: AlarmFile = serde_json::from_str(&text)
            .with_context(|| format!("{} is damaged", self.path.display()))?;
        if file.version != FILE_VERSION {
            bail!(
                "{} has version {}, this program understands version {FILE_VERSION}",
                self.path.display(),
                file.version
            );
        }
        Ok(file.alarms)
    }

    /// When the file last changed, or `None` if it does not exist. The
    /// daemon compares this to notice edits made by the CLI.
    pub fn modified(&self) -> Option<SystemTime> {
        fs::metadata(&self.path).ok()?.modified().ok()
    }

    /// Add an alarm, or replace the one with the same id.
    /// The caller is expected to have validated the alarm.
    pub fn put(&self, alarm: Alarm) -> anyhow::Result<PutOutcome> {
        let _lock = self.lock()?;
        let mut alarms = self.load()?;

        // `position` finds the index of the alarm with this id, if any.
        let existing = alarms.iter().position(|other| other.id == alarm.id);
        let outcome = match existing {
            Some(index) => {
                alarms[index] = alarm;
                PutOutcome::Replaced
            }
            None => {
                if alarms.len() >= MAX_ALARMS {
                    bail!("the alarm list is full ({MAX_ALARMS} alarms)");
                }
                alarms.push(alarm);
                PutOutcome::Created
            }
        };
        self.save(&alarms)?;
        Ok(outcome)
    }

    /// Remove an alarm. Returns `false` if there was none with that id,
    /// which is not an error: the result is the same either way.
    pub fn delete(&self, id: &str) -> anyhow::Result<bool> {
        let _lock = self.lock()?;
        let mut alarms = self.load()?;
        let before = alarms.len();
        // `retain` keeps only the elements the closure returns true for.
        alarms.retain(|alarm| alarm.id != id);
        if alarms.len() == before {
            return Ok(false);
        }
        self.save(&alarms)?;
        Ok(true)
    }

    /// Take the lock file. The lock is released when the returned `File` is
    /// dropped, i.e. at the end of the calling function.
    fn lock(&self) -> anyhow::Result<File> {
        if let Some(dir) = self.lock_path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        }
        let file = File::create(&self.lock_path)
            .with_context(|| format!("cannot open {}", self.lock_path.display()))?;
        // Blocks until no other process holds the lock.
        file.lock()
            .with_context(|| format!("cannot lock {}", self.lock_path.display()))?;
        Ok(file)
    }

    fn save(&self, alarms: &[Alarm]) -> anyhow::Result<()> {
        let file = AlarmFile {
            version: FILE_VERSION,
            alarms: alarms.to_vec(),
        };
        let json = serde_json::to_string_pretty(&file)?;
        write_atomically(&self.path, json.as_bytes())
    }
}

/// Replace `path` with `contents` so that a crash never leaves a partial file.
pub fn write_atomically(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;

    // Same directory as the target, because a rename is only atomic within
    // one filesystem.
    let mut temp_name = path.file_name().unwrap_or_default().to_os_string();
    temp_name.push(".tmp");
    let temp_path = dir.join(temp_name);

    let mut temp = File::create(&temp_path)
        .with_context(|| format!("cannot write {}", temp_path.display()))?;
    temp.write_all(contents)?;
    // `sync_all` waits until the data is really on the SD card.
    temp.sync_all()?;
    drop(temp);

    fs::rename(&temp_path, path).with_context(|| format!("cannot replace {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alarm::tests::sample_alarm;

    #[test]
    fn a_new_store_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = AlarmStore::new(dir.path());
        assert!(store.load().unwrap().is_empty());
        assert!(store.modified().is_none());
    }

    #[test]
    fn alarms_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = AlarmStore::new(dir.path());
            assert_eq!(store.put(sample_alarm("a")).unwrap(), PutOutcome::Created);
            assert_eq!(store.put(sample_alarm("b")).unwrap(), PutOutcome::Created);
        }
        // A brand new store object, as after a reboot.
        let store = AlarmStore::new(dir.path());
        let alarms = store.load().unwrap();
        assert_eq!(alarms, vec![sample_alarm("a"), sample_alarm("b")]);
        assert!(store.modified().is_some());
    }

    #[test]
    fn put_with_an_existing_id_replaces_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let store = AlarmStore::new(dir.path());
        store.put(sample_alarm("a")).unwrap();
        store.put(sample_alarm("b")).unwrap();

        let mut changed = sample_alarm("a");
        changed.hour = 9;
        changed.enabled = false;
        assert_eq!(store.put(changed.clone()).unwrap(), PutOutcome::Replaced);

        let alarms = store.load().unwrap();
        assert_eq!(alarms.len(), 2);
        assert_eq!(alarms[0], changed, "keeps its place in the list");
        assert_eq!(alarms[1], sample_alarm("b"));
    }

    #[test]
    fn delete_removes_and_reports_whether_it_existed() {
        let dir = tempfile::tempdir().unwrap();
        let store = AlarmStore::new(dir.path());
        store.put(sample_alarm("a")).unwrap();
        assert!(store.delete("a").unwrap());
        assert!(!store.delete("a").unwrap());
        assert!(!store.delete("never-existed").unwrap());
        assert!(store.load().unwrap().is_empty());
    }

    #[test]
    fn the_list_has_a_maximum_size() {
        let dir = tempfile::tempdir().unwrap();
        let store = AlarmStore::new(dir.path());
        for index in 0..MAX_ALARMS {
            store.put(sample_alarm(&format!("a{index}"))).unwrap();
        }
        let error = store.put(sample_alarm("one-too-many")).unwrap_err();
        assert!(error.to_string().contains("full"), "{error}");
        // Replacing an existing alarm still works when full.
        assert_eq!(store.put(sample_alarm("a0")).unwrap(), PutOutcome::Replaced);
    }

    #[test]
    fn the_data_directory_is_created_on_first_write() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("does/not/exist/yet");
        let store = AlarmStore::new(&nested);
        store.put(sample_alarm("a")).unwrap();
        assert_eq!(store.load().unwrap().len(), 1);
    }

    #[test]
    fn a_damaged_file_is_an_error_not_an_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        let store = AlarmStore::new(dir.path());
        fs::write(store.path(), "{ this is not json").unwrap();
        let error = store.load().unwrap_err();
        assert!(error.to_string().contains("damaged"), "{error}");
        // And we refuse to overwrite it.
        assert!(store.put(sample_alarm("a")).is_err());
        assert_eq!(
            fs::read_to_string(store.path()).unwrap(),
            "{ this is not json"
        );
    }

    #[test]
    fn a_file_from_a_newer_version_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = AlarmStore::new(dir.path());
        fs::write(store.path(), r#"{"version":2,"alarms":[]}"#).unwrap();
        let error = store.load().unwrap_err();
        assert!(error.to_string().contains("version 2"), "{error}");
    }

    #[test]
    fn no_temporary_file_is_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let store = AlarmStore::new(dir.path());
        store.put(sample_alarm("a")).unwrap();
        let mut names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["alarms.json", "alarms.lock"]);
    }

    #[test]
    fn writers_in_several_threads_do_not_lose_updates() {
        let dir = tempfile::tempdir().unwrap();
        let store = AlarmStore::new(dir.path());
        let mut handles = Vec::new();
        for index in 0..8 {
            // Each thread gets its own copy of the store (just two paths).
            let store = store.clone();
            handles.push(std::thread::spawn(move || {
                store.put(sample_alarm(&format!("t{index}"))).unwrap();
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(store.load().unwrap().len(), 8);
    }
}
