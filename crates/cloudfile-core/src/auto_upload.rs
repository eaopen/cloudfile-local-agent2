//! A small polling observer over Agent-owned working copies only.
//! Existing intents are never sent automatically; uncertain results pause durably.
use crate::{journal, session, storage, upload};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashMap, path::Path};

const SETTLE_MS: u64 = 3_000;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub pause_reason: Option<String>,
    pub last_synced_at: Option<u64>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            pause_reason: None,
            last_synced_at: None,
        }
    }
}
pub fn settings(root: &Path, id: &str) -> Result<Settings> {
    let path = session::task_dir(root, id)?.join("auto-upload.json");
    if path.exists() {
        storage::read(&path)
    } else {
        Ok(Settings::default())
    }
}
fn update(root: &Path, id: &str, change: impl FnOnce(&mut Settings)) -> Result<()> {
    let dir = session::task_dir(root, id)?;
    let _lock = storage::lock(&dir.join("auto-upload.lock"))?;
    let mut current = settings(root, id)?;
    change(&mut current);
    storage::write(&dir.join("auto-upload.json"), &current)
}
pub fn set_enabled(root: &Path, id: &str, enabled: bool) -> Result<()> {
    let task = session::load(root, id)?;
    if task.mode != "optimistic-edit"
        || (task.device_id.is_none() && !crate::v2_edit::is_v2(root, id))
    {
        anyhow::bail!("此任务不支持自动上传");
    }
    update(root, id, |current| {
        current.enabled = enabled;
        if enabled {
            current.pause_reason = None;
        }
    })
}
pub fn confirmed(root: &Path, id: &str) -> Result<()> {
    update(root, id, |current| {
        current.pause_reason = None;
        current.last_synced_at = Some(crate::now());
    })
}
fn pause(root: &Path, id: &str, error: &str) -> Result<()> {
    update(root, id, |current| {
        current.pause_reason = Some(error.chars().take(400).collect());
    })
}
struct Observation {
    digest: String,
    since: u64,
}
#[derive(Default)]
pub struct Observer {
    observations: HashMap<String, Observation>,
}
impl Observer {
    pub fn scan(&mut self, root: &Path, monotonic_ms: u64) -> Result<()> {
        self.scan_with(root, monotonic_ms, |id, digest| {
            upload::save_observed(root, id, digest)
        })
    }
    pub fn scan_with(
        &mut self,
        root: &Path,
        monotonic_ms: u64,
        mut save: impl FnMut(&str, &str) -> Result<Value>,
    ) -> Result<()> {
        let tasks = session::tasks(root)?;
        self.observations
            .retain(|id, _| tasks.iter().any(|t| &t.task_id == id));
        for task in tasks {
            let id = &task.task_id;
            let current = settings(root, id)?;
            if task.mode != "optimistic-edit"
                || (task.device_id.is_none() && !crate::v2_edit::is_v2(root, id))
                || !current.enabled
                || current.pause_reason.is_some()
                || journal::pending(root, id)?.is_some()
            {
                self.observations.remove(id);
                continue;
            }
            // Office commonly replaces the file atomically. Temporary missing
            // or unreadable copies reset the settling window, not the baseline.
            let path = match session::local_path(root, &task) {
                Ok(path) => path,
                Err(_) => {
                    self.observations.remove(id);
                    continue;
                }
            };
            // Office may replace its work copy between the local_path check
            // and this metadata read. A temporary missing/locked file must not
            // abort the entire observer loop or pause another open document.
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_file() => metadata,
                Ok(_) => {
                    pause(root, id, "工作副本不再是普通文件；自动上传已暂停，请检查本地文件")?;
                    self.observations.remove(id);
                    continue;
                }
                Err(_) => {
                    self.observations.remove(id);
                    continue;
                }
            };
            if metadata.len() > crate::device::MAX_FILE {
                pause(root, id, "文件超过 128 MiB，自动上传已暂停；本地文件已保留")?;
                self.observations.remove(id);
                continue;
            }
            let digest = match session::digest(&path) {
                Ok(digest) => digest,
                Err(_) => {
                    self.observations.remove(id);
                    continue;
                }
            };
            if digest == task.baseline_digest {
                self.observations.remove(id);
                continue;
            }
            let observation = self
                .observations
                .entry(id.clone())
                .or_insert_with(|| Observation {
                    digest: digest.clone(),
                    since: monotonic_ms,
                });
            if observation.digest != digest {
                *observation = Observation {
                    digest,
                    since: monotonic_ms,
                };
                continue;
            }
            if monotonic_ms.saturating_sub(observation.since) < SETTLE_MS {
                continue;
            }
            self.observations.remove(id);
            // upload::save_observed acquires the shared upload lock, rechecks
            // pending state, and binds capture to the exact observed digest.
            match save(id, &digest) {
                Ok(result) if result["state"] == "published" => confirmed(root, id)?,
                Ok(result)
                    if ["unchanged", "changing", "busy", "pending"]
                        .iter()
                        .any(|state| result["state"] == *state) => {}
                Ok(_) => pause(root, id, "上传结果未确认，自动上传已暂停；请查询原上传结果")?,
                Err(error) => pause(root, id, &error.to_string())?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{session::Task, storage};
    use serde_json::json;

    #[test]
    fn office_atomic_replacement_resets_settle_without_pausing_or_uploading() {
        let (_temp, root) = storage::test_root();
        let id = "c".repeat(64);
        let dir = session::task_dir(&root, &id).unwrap();
        storage::private_dir(&dir).unwrap();
        let task = Task {
            task_id: id.clone(),
            device_id: None,
            file_name: "office.docx".into(),
            mode: "optimistic-edit".into(),
            server: "https://example.com".into(),
            repo_id: "11111111-1111-4111-8111-111111111111".into(),
            remote_path: "/office.docx".into(),
            session_id: "22222222-2222-4222-8222-222222222222".into(),
            baseline_file_id: "a".repeat(40),
            baseline_digest: "b".repeat(64),
            size: 3,
        };
        storage::write(&dir.join("task.json"), &task).unwrap();
        // Mark the task as V2 without ever reaching the remote service.
        storage::write(&dir.join("v2-edit.json"), &json!({})).unwrap();
        let working = dir.join("office.docx");
        std::fs::write(&working, b"first edit").unwrap();
        let mut observer = Observer::default();
        let mut calls = 0;
        observer.scan_with(&root, 0, |_, _| {
            calls += 1;
            Ok(json!({"state":"published"}))
        }).unwrap();

        let temporary = dir.join("office.tmp");
        std::fs::rename(&working, &temporary).unwrap();
        observer.scan_with(&root, 1000, |_, _| {
            calls += 1;
            Ok(json!({"state":"published"}))
        }).unwrap();
        std::fs::rename(&temporary, &working).unwrap();

        observer.scan_with(&root, 2500, |_, _| {
            calls += 1;
            Ok(json!({"state":"published"}))
        }).unwrap();
        assert_eq!(calls, 0, "settle period restarts after a replacement");
        observer.scan_with(&root, 5600, |_, _| {
            calls += 1;
            Ok(json!({"state":"changing"}))
        }).unwrap();
        assert_eq!(calls, 1);
        assert!(settings(&root, &id).unwrap().pause_reason.is_none());
    }

    #[test]
    fn view_and_exclusive_tasks_never_offer_automatic_upload() {
        let (_temp, root) = storage::test_root();
        for (suffix, mode) in [('d', "local-view"), ('e', "exclusive-edit")] {
            let id = suffix.to_string().repeat(64);
            let dir = session::task_dir(&root, &id).unwrap();
            storage::private_dir(&dir).unwrap();
            let task = Task {
                task_id: id.clone(),
                device_id: None,
                file_name: "document.txt".into(),
                mode: mode.into(),
                server: "https://example.com".into(),
                repo_id: "11111111-1111-4111-8111-111111111111".into(),
                remote_path: "/document.txt".into(),
                session_id: "22222222-2222-4222-8222-222222222222".into(),
                baseline_file_id: "a".repeat(40),
                baseline_digest: "b".repeat(64),
                size: 3,
            };
            storage::write(&dir.join("task.json"), &task).unwrap();
            storage::write(&dir.join("v2-edit.json"), &json!({})).unwrap();
            std::fs::write(dir.join("document.txt"), b"new content").unwrap();
            assert!(set_enabled(&root, &id, true).is_err());
        }
        let mut calls = 0;
        Observer::default().scan_with(&root, 5000, |_, _| {
            calls += 1;
            Ok(json!({"state":"published"}))
        }).unwrap();
        assert_eq!(calls, 0);
    }
}
