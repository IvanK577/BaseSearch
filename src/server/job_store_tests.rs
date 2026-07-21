use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::json;

use super::auth::Identity;
use super::job_store::job_store_path;
use super::jobs::{
    ArtifactTtl, JobKind, JobRegistry, JobSnapshot, JobStatus, JobVisibility,
    spawn_job_for_with_input,
};

fn wait_for_status(registry: &JobRegistry, id: u64, expected: JobStatus) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if registry.snapshot(id).map(|job| job.status) == Some(expected) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("job {id} did not reach {expected:?}");
}

#[test]
fn legacy_snapshots_without_ownership_default_to_private() {
    let registry = JobRegistry::new();
    let job = registry.create(JobKind::Export, "Legacy export");
    let mut value = serde_json::to_value(job.snapshot().unwrap()).unwrap();
    let object = value.as_object_mut().unwrap();
    object.remove("owner_user_id");
    object.remove("visibility");

    let restored: JobSnapshot = serde_json::from_value(value).unwrap();

    assert_eq!(restored.owner_user_id, "legacy-system");
    assert_eq!(restored.visibility, JobVisibility::Private);
}

#[test]
fn restart_restores_terminal_jobs_and_marks_inflight_jobs_interrupted() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("workspace.db");

    let registry = JobRegistry::open(&db_path).unwrap();

    let succeeded = registry.create(JobKind::Export, "Exporting report.csv");
    succeeded.set_running();
    succeeded.set_progress("writing", 7, 10);
    succeeded.set_message("Exported 7 rows");
    succeeded.succeed(Some(json!({
        "download_url": "/api/exports/report.csv",
        "rows": 7
    })));
    let succeeded_before = succeeded.snapshot().unwrap();

    let failed = registry.create(JobKind::Optimize, "Optimizing database");
    failed.set_running();
    failed.fail("database rejected optimize");
    let failed_before = failed.snapshot().unwrap();

    let running = registry.create(JobKind::Import, "Importing records.xlsx");
    running.set_running();
    running.set_progress("inserting", 23, 100);
    assert!(registry.cancel(running.id()));
    assert!(running.is_cancelled());
    let running_before = running.snapshot().unwrap();

    let queued = registry.create(JobKind::Reindex, "Reindexing database");
    let queued_before = queued.snapshot().unwrap();
    let highest_id = queued.id();

    drop((succeeded, failed, running, queued, registry));

    let reopened = JobRegistry::open(&db_path).unwrap();

    assert_eq!(
        serde_json::to_value(reopened.snapshot(succeeded_before.id).unwrap()).unwrap(),
        serde_json::to_value(&succeeded_before).unwrap()
    );
    assert_eq!(
        serde_json::to_value(reopened.snapshot(failed_before.id).unwrap()).unwrap(),
        serde_json::to_value(&failed_before).unwrap()
    );

    for before in [running_before, queued_before] {
        let restored = reopened.snapshot(before.id).unwrap();
        assert_eq!(restored.status, JobStatus::Failed);
        assert_eq!(restored.owner_user_id, before.owner_user_id);
        assert_eq!(restored.visibility, before.visibility);
        assert!(!restored.cancellable);
        assert_eq!(restored.created_ms, before.created_ms);
        assert!(restored.updated_ms >= before.updated_ms);
        assert!(
            restored
                .error
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase()
                .contains("interrupted")
        );
    }

    let next = reopened.create(JobKind::Clear, "Clearing database");
    assert!(next.id() > highest_id);
}

#[test]
fn restart_preserves_the_bounded_import_request_payload() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("workspace.db");
    let registry = JobRegistry::open(&db_path).unwrap();
    let input = json!({
        "files": ["source.csv"],
        "selected_sheets": ["source.csv"],
        "source_profiles": {
            "source.csv": {
                "id": 7,
                "name": "Reusable source",
                "signature": format!("smp1:2:{}", "c".repeat(64))
            }
        },
        "sheet_semantics": {
            "source.csv": { "0": "Recipient", "1": "Value" }
        },
        "sheet_fixed_values": {
            "source.csv": { "Currency": "USD", "WeightUnit": "kg" }
        }
    });
    let snapshot = spawn_job_for_with_input(
        &registry,
        &Identity::local_owner(),
        JobKind::Import,
        JobVisibility::Workspace,
        "Importing source.csv",
        Some(input.clone()),
        |handle| handle.succeed(None),
    )
    .unwrap();
    let id = snapshot.id;
    for _ in 0..100 {
        if registry
            .snapshot(id)
            .is_some_and(|snapshot| snapshot.status == JobStatus::Succeeded)
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(registry.snapshot(id).unwrap().input, Some(input.clone()));
    drop(registry);

    let reopened = JobRegistry::open(&db_path).unwrap();
    let restored = reopened.snapshot(id).unwrap();
    assert_eq!(restored.status, JobStatus::Succeeded);
    assert_eq!(restored.input, Some(input));
}

#[test]
fn terminal_history_is_bounded_in_memory_and_after_reopen() {
    const HISTORY_LIMIT: usize = 40;

    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("workspace.db");
    let registry = JobRegistry::open(&db_path).unwrap();

    for sequence in 0..(HISTORY_LIMIT + 5) {
        let job = registry.create(JobKind::Export, format!("Export {sequence}"));
        job.succeed(Some(json!({ "sequence": sequence })));
    }

    let live = registry.list();
    assert_eq!(live.len(), HISTORY_LIMIT);
    let expected_ids: Vec<u64> = (6..=45).rev().collect();
    assert_eq!(
        live.iter().map(|snapshot| snapshot.id).collect::<Vec<_>>(),
        expected_ids
    );
    drop(registry);

    let reopened = JobRegistry::open(&db_path).unwrap();
    let restored = reopened.list();
    assert_eq!(restored.len(), HISTORY_LIMIT);
    assert_eq!(
        restored
            .iter()
            .map(|snapshot| snapshot.id)
            .collect::<Vec<_>>(),
        expected_ids
    );
}

#[test]
fn export_tokens_are_not_stored_raw_and_resolve_on_restart() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("workspace.db");
    let raw_token = "4242-987654321-secret";
    let export_dir = temp.path().join("exports").join(raw_token);
    std::fs::create_dir_all(&export_dir).unwrap();
    std::fs::write(export_dir.join("report.csv"), b"id,name\n1,Ada\n").unwrap();

    let registry = JobRegistry::open(&db_path).unwrap();
    let job = registry.create(JobKind::Export, "Exporting report.csv");
    job.succeed(Some(json!({
        "file_name": "report.csv",
        "token": raw_token,
        "rows": 1,
        "bytes": 14,
        "download_url": format!("/api/exports/{}/download", job.id())
    })));
    let live = job.snapshot().unwrap();
    assert_eq!(
        live.result.as_ref().unwrap()["token"].as_str(),
        Some(raw_token)
    );

    let persisted_json: String = rusqlite::Connection::open(job_store_path(&db_path))
        .unwrap()
        .query_row(
            "SELECT snapshot_json FROM jobs WHERE id = ?1",
            [i64::try_from(job.id()).unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!persisted_json.contains(raw_token));
    assert!(persisted_json.contains("sha256:"));

    let id = job.id();
    drop((job, registry));

    let reopened = JobRegistry::open(&db_path).unwrap();
    let restored = reopened.snapshot(id).unwrap();
    assert_eq!(
        serde_json::to_value(restored).unwrap(),
        serde_json::to_value(live).unwrap()
    );
}

#[test]
fn artifact_cleanup_uses_durable_job_metadata_and_excludes_active_jobs() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("workspace.db");
    let uploads = temp.path().join("uploads");
    let exports = temp.path().join("exports");
    std::fs::create_dir_all(&uploads).unwrap();
    std::fs::create_dir_all(&exports).unwrap();

    let active_token = "active-upload";
    let active_dir = uploads.join(format!("{active_token}-0"));
    std::fs::create_dir_all(&active_dir).unwrap();
    std::fs::write(active_dir.join("private.xlsx"), b"active").unwrap();

    let stale_token = "stale-export";
    let stale_dir = exports.join(stale_token);
    std::fs::create_dir_all(&stale_dir).unwrap();
    std::fs::write(stale_dir.join("result.csv"), b"stale").unwrap();

    let orphan_upload = uploads.join("orphan-upload-0");
    let orphan_export = exports.join("orphan-export");
    std::fs::create_dir_all(&orphan_upload).unwrap();
    std::fs::create_dir_all(&orphan_export).unwrap();

    let registry = JobRegistry::open(&db_path).unwrap();
    let (release_tx, release_rx) = mpsc::channel();
    let active = spawn_job_for_with_input(
        &registry,
        &Identity::local_owner(),
        JobKind::Import,
        JobVisibility::Workspace,
        "active import",
        Some(json!({"artifact_token": active_token, "files": ["private.xlsx"]})),
        move |handle| {
            let _ = release_rx.recv();
            handle.succeed(None);
        },
    )
    .unwrap();
    wait_for_status(&registry, active.id, JobStatus::Running);

    let stale = registry.create(JobKind::Export, "stale export");
    stale.succeed(Some(json!({
        "file_name": "result.csv",
        "token": stale_token,
        "download_url": format!("/api/v2/exports/{}/download", stale.id())
    })));

    let report = registry
        .cleanup_artifacts_at(
            &uploads,
            &exports,
            u64::MAX,
            ArtifactTtl {
                uploads_ms: 0,
                exports_ms: 0,
            },
        )
        .unwrap();

    assert!(active_dir.exists(), "active upload must not be removed");
    assert!(!stale_dir.exists(), "terminal export should expire");
    assert!(!orphan_upload.exists(), "orphan upload should expire");
    assert!(!orphan_export.exists(), "orphan export should expire");
    assert_eq!(report.skipped_active, 1);
    assert_eq!(report.removed_exports, 2);
    assert_eq!(report.removed_uploads, 1);

    release_tx.send(()).unwrap();
    wait_for_status(&registry, active.id, JobStatus::Succeeded);
}

#[test]
fn startup_cleanup_uses_the_persisted_terminal_timestamp() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("workspace.db");
    let token = "expired-after-restart";
    let export_dir = temp.path().join("exports").join(token);
    std::fs::create_dir_all(&export_dir).unwrap();
    std::fs::write(export_dir.join("report.csv"), b"expired").unwrap();

    let registry = JobRegistry::open(&db_path).unwrap();
    let job = registry.create(JobKind::Export, "old export");
    job.succeed(Some(json!({
        "file_name": "report.csv",
        "token": token,
        "download_url": format!("/api/v2/exports/{}/download", job.id())
    })));
    let id = job.id();
    drop((job, registry));

    let conn = rusqlite::Connection::open(job_store_path(&db_path)).unwrap();
    let stored: String = conn
        .query_row(
            "SELECT snapshot_json FROM jobs WHERE id = ?1",
            [i64::try_from(id).unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&stored).unwrap();
    value["updated_ms"] = json!(0);
    conn.execute(
        "UPDATE jobs SET updated_ms = 0, snapshot_json = ?1 WHERE id = ?2",
        rusqlite::params![
            serde_json::to_string(&value).unwrap(),
            i64::try_from(id).unwrap()
        ],
    )
    .unwrap();
    drop(conn);

    let _reopened = JobRegistry::open(&db_path).unwrap();
    assert!(
        !export_dir.exists(),
        "startup cleanup should remove an export older than its TTL"
    );
}

#[tokio::test]
async fn periodic_cleanup_removes_new_orphans_without_a_restart() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("workspace.db");
    let registry = JobRegistry::open(&db_path).unwrap();
    let orphan = temp.path().join("uploads").join("periodic-orphan-0");
    std::fs::create_dir_all(&orphan).unwrap();
    std::fs::write(orphan.join("leftover.csv"), b"orphan").unwrap();

    registry.start_artifact_cleanup_task_for_test(
        Duration::from_millis(10),
        ArtifactTtl {
            uploads_ms: 0,
            exports_ms: 0,
        },
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while orphan.exists() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(!orphan.exists(), "periodic cleanup did not remove orphan");
}
