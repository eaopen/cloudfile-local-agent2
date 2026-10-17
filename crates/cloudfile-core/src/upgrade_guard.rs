//! One fail-closed upgrade gate shared by CLI, UI and the future launcher.
//! A quiet desktop does not prove an Office/CAD file was closed. Editable
//! sessions must be explicitly closed before automatic activation.
use crate::{guard, journal, session, storage};
use serde::Serialize;
use std::io::ErrorKind;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Blocker {
    pub task_id: String,
    pub file_name: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Assessment {
    pub safe_to_upgrade: bool,
    pub blockers: Vec<Blocker>,
}

fn block(out: &mut Vec<Blocker>, id: &str, name: &str, reason: &str) {
    out.push(Blocker {
        task_id: id.to_string(),
        file_name: name.to_string(),
        reason: reason.to_string(),
    });
}

pub fn assess(root: &Path) -> Assessment {
    let mut blockers = Vec::new();
    let task_root = root.join("tasks");
    match std::fs::symlink_metadata(&task_root) {
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Assessment {
                safe_to_upgrade: true,
                blockers,
            };
        }
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
        _ => {
            block(&mut blockers, "", "", "task-directory-unreadable");
            return Assessment {
                safe_to_upgrade: false,
                blockers,
            };
        }
    }
    // Avoid a false clean result on an unreadable/journal-corrupt task.
    let tasks = match session::tasks(root) {
        Ok(tasks) => tasks,
        Err(_) => {
            block(&mut blockers, "", "", "task-store-unreadable");
            return Assessment {
                safe_to_upgrade: false,
                blockers,
            };
        }
    };
    let existing = guard::assess(root);
    if !existing.safe_to_exit {
        for risk in existing.risks {
            block(
                &mut blockers,
                &risk.task_id,
                &risk.file_name,
                "guard-or-unconfirmed-intent",
            );
        }
    }
    for task in tasks {
        let id = &task.task_id;
        let name = &task.file_name;
        let journal = match journal::snapshot(root, id) {
            Ok(state) => state,
            Err(_) => {
                block(&mut blockers, id, name, "journal-unreadable");
                continue;
            }
        };
        if journal.pending.is_some() {
            block(&mut blockers, id, name, "pending-publish");
        }
        if !journal.closed {
            block(&mut blockers, id, name, "editing-session-not-finished");
        }
        let path = match session::local_path(root, &task) {
            Ok(path) => path,
            Err(_) => {
                block(&mut blockers, id, name, "working-copy-unreadable");
                continue;
            }
        };
        match session::digest(&path) {
            Ok(digest) if digest == task.baseline_digest => {}
            Ok(_) => block(&mut blockers, id, name, "local-changes-not-published"),
            Err(_) => block(&mut blockers, id, name, "working-copy-unreadable"),
        }
        let dir = match session::task_dir(root, id) {
            Ok(dir) => dir,
            Err(_) => {
                block(&mut blockers, id, name, "invalid-task-id");
                continue;
            }
        };
        // Do not activate while the upload implementation holds its lock.
        if storage::lock(&dir.join("upload.lock")).is_err() {
            block(&mut blockers, id, name, "upload-busy-or-unknown");
        }
    }
    Assessment {
        safe_to_upgrade: blockers.is_empty(),
        blockers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{session::Task, storage};
    use sha2::{Digest, Sha256};

    #[test]
    fn no_tasks_is_safe_and_corrupt_directory_is_not() {
        let (_temp, root) = storage::test_root();
        assert!(assess(&root).safe_to_upgrade);
        std::fs::write(root.join("tasks"), b"broken").unwrap();
        assert!(!assess(&root).safe_to_upgrade);
    }

    #[test]
    fn open_file_and_local_edits_block_activation_even_without_checkout() {
        let (_temp, root) = storage::test_root();
        let id = "a".repeat(64);
        let dir = session::task_dir(&root, &id).unwrap();
        storage::private_dir(&dir).unwrap();
        let content = b"base";
        std::fs::write(dir.join("test.docx"), content).unwrap();
        storage::write(
            &dir.join("task.json"),
            &Task {
                task_id: id.clone(),
                device_id: None,
                file_name: "test.docx".into(),
                mode: "optimistic-edit".into(),
                server: "http://files.example".into(),
                repo_id: "11111111-1111-4111-8111-111111111111".into(),
                remote_path: "/test.docx".into(),
                session_id: "bbb".into(),
                baseline_file_id: "b".repeat(40),
                baseline_digest: format!("{:x}", Sha256::digest(content)),
                size: 4,
            },
        )
        .unwrap();
        let result = assess(&root);
        assert!(!result.safe_to_upgrade);
        assert!(result
            .blockers
            .iter()
            .any(|b| b.reason == "editing-session-not-finished"));
        std::fs::write(dir.join("test.docx"), b"new").unwrap();
        assert!(assess(&root)
            .blockers
            .iter()
            .any(|b| b.reason == "local-changes-not-published"));
    }
}
