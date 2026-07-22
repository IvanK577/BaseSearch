use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use super::auth::{Identity, Role};
use super::job_store::job_store_path;
use super::jobs::{
    JobAccessError, JobCreateError, JobKind, JobQueueLimits, JobRegistry, JobStatus, JobVisibility,
    spawn_job, spawn_job_for, spawn_job_for_with_input,
};

fn identity(user_id: &str, role: Role) -> Identity {
    Identity {
        user_id: user_id.to_string(),
        username: user_id.to_string(),
        role,
    }
}

fn wait_for_status(registry: &JobRegistry, id: u64, expected: JobStatus) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if registry.snapshot(id).map(|job| job.status) == Some(expected) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!(
        "job {id} did not reach {expected:?}; latest snapshot: {:?}",
        registry.snapshot(id).map(|job| job.status)
    );
}

fn persisted_status(db_path: &std::path::Path, id: u64) -> String {
    Connection::open(job_store_path(db_path))
        .unwrap()
        .query_row(
            "SELECT status FROM jobs WHERE id = ?1",
            [i64::try_from(id).unwrap()],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn second_import_is_persisted_queued_then_starts_after_the_first() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("workspace.db");
    let registry = JobRegistry::open(&db_path).unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let first_started = started_tx.clone();
    let first = spawn_job(&registry, JobKind::Import, "first import", move |handle| {
        first_started.send(handle.id()).unwrap();
        let _ = release_rx.recv();
        handle.succeed(None);
    })
    .unwrap_or_else(|_| panic!("the first import should start"));
    assert_eq!(
        started_rx.recv_timeout(Duration::from_secs(1)),
        Ok(first.id)
    );

    let second_started = started_tx.clone();
    let second_result = spawn_job(&registry, JobKind::Import, "second import", move |handle| {
        second_started.send(handle.id()).unwrap();
        handle.succeed(None);
    });
    assert!(
        second_result.is_ok(),
        "the second write must be queued instead of rejected"
    );
    let second = second_result.unwrap_or_else(|_| unreachable!("checked above"));
    assert_eq!(
        registry.snapshot(second.id).unwrap().status,
        JobStatus::Queued
    );
    assert_eq!(persisted_status(&db_path, second.id), "queued");
    assert!(started_rx.recv_timeout(Duration::from_millis(100)).is_err());

    release_tx.send(()).unwrap();
    assert_eq!(
        started_rx.recv_timeout(Duration::from_secs(1)),
        Ok(second.id)
    );
    wait_for_status(&registry, first.id, JobStatus::Succeeded);
    wait_for_status(&registry, second.id, JobStatus::Succeeded);
    assert_eq!(persisted_status(&db_path, first.id), "succeeded");
    assert_eq!(persisted_status(&db_path, second.id), "succeeded");
}

#[test]
fn read_jobs_have_bounded_concurrency_and_queued_work_resumes() {
    const EXPECTED_READ_LIMIT: usize = 2;

    let registry = JobRegistry::new();
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let (started_tx, started_rx) = mpsc::channel();
    let mut ids = Vec::new();

    for sequence in 0..4 {
        let active = Arc::clone(&active);
        let maximum = Arc::clone(&maximum);
        let release = Arc::clone(&release);
        let started = started_tx.clone();
        let snapshot = spawn_job(
            &registry,
            JobKind::Export,
            format!("export {sequence}"),
            move |handle| {
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(now, Ordering::SeqCst);
                started.send(handle.id()).unwrap();
                let (lock, wake) = &*release;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = wake.wait(released).unwrap();
                }
                active.fetch_sub(1, Ordering::SeqCst);
                handle.succeed(None);
            },
        )
        .unwrap_or_else(|_| panic!("read jobs should enter the bounded queue"));
        ids.push(snapshot.id);
    }

    for _ in 0..EXPECTED_READ_LIMIT {
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    }
    let unexpected = started_rx.recv_timeout(Duration::from_millis(150));
    {
        let (lock, wake) = &*release;
        *lock.lock().unwrap() = true;
        wake.notify_all();
    }
    assert!(
        unexpected.is_err(),
        "more than {EXPECTED_READ_LIMIT} read jobs started concurrently"
    );

    for _ in EXPECTED_READ_LIMIT..ids.len() {
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    }
    for id in ids {
        wait_for_status(&registry, id, JobStatus::Succeeded);
    }
    assert_eq!(maximum.load(Ordering::SeqCst), EXPECTED_READ_LIMIT);
}

#[test]
fn write_queue_stays_fifo_when_a_worker_exits_without_a_result() {
    let registry = JobRegistry::new();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let first_started = started_tx.clone();
    let first = spawn_job(&registry, JobKind::Import, "first", move |handle| {
        first_started.send(handle.id()).unwrap();
        let _ = release_rx.recv();
    })
    .unwrap();
    assert_eq!(
        started_rx.recv_timeout(Duration::from_secs(1)),
        Ok(first.id)
    );

    let second_started = started_tx.clone();
    let second = spawn_job(&registry, JobKind::Reindex, "second", move |handle| {
        second_started.send(handle.id()).unwrap();
        handle.succeed(None);
    })
    .unwrap();
    let third_started = started_tx;
    let third = spawn_job(&registry, JobKind::Compact, "third", move |handle| {
        third_started.send(handle.id()).unwrap();
        handle.succeed(None);
    })
    .unwrap();
    assert!(started_rx.recv_timeout(Duration::from_millis(100)).is_err());

    release_tx.send(()).unwrap();
    assert_eq!(
        started_rx.recv_timeout(Duration::from_secs(1)),
        Ok(second.id)
    );
    assert_eq!(
        started_rx.recv_timeout(Duration::from_secs(1)),
        Ok(third.id)
    );
    wait_for_status(&registry, first.id, JobStatus::Failed);
    wait_for_status(&registry, second.id, JobStatus::Succeeded);
    wait_for_status(&registry, third.id, JobStatus::Succeeded);
    assert_eq!(
        registry.snapshot(first.id).unwrap().error.as_deref(),
        Some("Job worker stopped without publishing a final result.")
    );
}

#[test]
fn queued_job_is_cancelled_when_current_authorization_is_revoked() {
    let registry = JobRegistry::new();
    let editor = identity("editor-id", Role::Editor);
    let authorized = Arc::new(AtomicBool::new(true));
    let dispatch_authorized = Arc::clone(&authorized);
    registry.set_authorizer(move |user_id, _permission| {
        user_id == "local-owner" || dispatch_authorized.load(Ordering::SeqCst)
    });
    let (release_tx, release_rx) = mpsc::channel();

    let active = spawn_job_for(
        &registry,
        &editor,
        JobKind::Import,
        JobVisibility::Workspace,
        "active import",
        move |handle| {
            let _ = release_rx.recv();
            handle.succeed(None);
        },
    )
    .unwrap();
    wait_for_status(&registry, active.id, JobStatus::Running);

    let ran = Arc::new(AtomicUsize::new(0));
    let queued_ran = Arc::clone(&ran);
    let queued = spawn_job_for(
        &registry,
        &editor,
        JobKind::Import,
        JobVisibility::Workspace,
        "revoked import",
        move |handle| {
            queued_ran.fetch_add(1, Ordering::SeqCst);
            handle.succeed(None);
        },
    )
    .unwrap();
    assert_eq!(
        registry.snapshot(queued.id).unwrap().status,
        JobStatus::Queued
    );

    authorized.store(false, Ordering::SeqCst);
    release_tx.send(()).unwrap();
    wait_for_status(&registry, active.id, JobStatus::Succeeded);
    wait_for_status(&registry, queued.id, JobStatus::Cancelled);

    assert_eq!(ran.load(Ordering::SeqCst), 0);
    assert_eq!(
        registry.snapshot(queued.id).unwrap().message.as_deref(),
        Some("Cancelled because the account no longer permits this operation.")
    );
}

#[test]
fn private_jobs_are_owner_visible_while_workspace_jobs_are_shared() {
    let registry = JobRegistry::new();
    let editor_a = identity("editor-a-id", Role::Editor);
    let editor_b = identity("editor-b-id", Role::Editor);
    let viewer = identity("viewer-id", Role::Viewer);
    let admin = identity("admin-id", Role::Admin);

    let private_a = spawn_job_for(
        &registry,
        &editor_a,
        JobKind::Export,
        JobVisibility::Private,
        "editor A export",
        |handle| handle.succeed(None),
    )
    .unwrap();
    let private_b = spawn_job_for(
        &registry,
        &editor_b,
        JobKind::Export,
        JobVisibility::Private,
        "editor B export",
        |handle| handle.succeed(None),
    )
    .unwrap();
    let workspace = spawn_job_for(
        &registry,
        &editor_a,
        JobKind::Import,
        JobVisibility::Workspace,
        "shared import",
        |handle| handle.succeed(None),
    )
    .unwrap();
    for id in [private_a.id, private_b.id, workspace.id] {
        wait_for_status(&registry, id, JobStatus::Succeeded);
    }

    assert_eq!(private_a.owner_user_id, "editor-a-id");
    assert_eq!(private_a.visibility, JobVisibility::Private);
    let ids_for_a: Vec<u64> = registry
        .list_for(&editor_a)
        .into_iter()
        .map(|job| job.id)
        .collect();
    assert!(ids_for_a.contains(&private_a.id));
    assert!(ids_for_a.contains(&workspace.id));
    assert!(!ids_for_a.contains(&private_b.id));

    let ids_for_viewer: Vec<u64> = registry
        .list_for(&viewer)
        .into_iter()
        .map(|job| job.id)
        .collect();
    assert_eq!(ids_for_viewer, vec![workspace.id]);
    assert_eq!(registry.list_for(&admin).len(), 3);
    assert!(registry.snapshot_for(&editor_b, private_a.id).is_none());
    assert!(registry.snapshot_for(&admin, private_a.id).is_some());
}

#[test]
fn creation_and_cancellation_follow_viewer_editor_admin_permissions() {
    let registry = JobRegistry::new();
    let viewer = identity("viewer-id", Role::Viewer);
    let editor_a = identity("editor-a-id", Role::Editor);
    let editor_b = identity("editor-b-id", Role::Editor);
    let admin = identity("admin-id", Role::Admin);
    let local_owner = Identity::local_owner();

    let denied_ran = Arc::new(AtomicUsize::new(0));
    let denied_counter = Arc::clone(&denied_ran);
    assert!(
        spawn_job_for(
            &registry,
            &viewer,
            JobKind::Import,
            JobVisibility::Workspace,
            "viewer import",
            move |_| {
                denied_counter.fetch_add(1, Ordering::SeqCst);
            },
        )
        .is_err()
    );
    assert_eq!(denied_ran.load(Ordering::SeqCst), 0);

    let editor_maintenance = spawn_job_for(
        &registry,
        &editor_a,
        JobKind::Optimize,
        JobVisibility::Workspace,
        "editor optimize",
        |_| {},
    );
    assert!(editor_maintenance.is_err());

    let (release_tx, release_rx) = mpsc::channel();
    let owned = spawn_job_for(
        &registry,
        &editor_a,
        JobKind::Export,
        JobVisibility::Private,
        "owned export",
        move |handle| {
            let _ = release_rx.recv();
            if handle.is_cancelled() {
                handle.mark_cancelled();
            } else {
                handle.succeed(None);
            }
        },
    )
    .unwrap();
    wait_for_status(&registry, owned.id, JobStatus::Running);

    assert_eq!(
        registry.cancel_for(&viewer, owned.id),
        Err(JobAccessError::Forbidden)
    );
    assert_eq!(
        registry.cancel_for(&editor_b, owned.id),
        Err(JobAccessError::Forbidden)
    );
    assert_eq!(registry.cancel_for(&admin, owned.id), Ok(true));
    release_tx.send(()).unwrap();
    wait_for_status(&registry, owned.id, JobStatus::Cancelled);

    let (write_release_tx, write_release_rx) = mpsc::channel();
    let active_write = spawn_job_for(
        &registry,
        &admin,
        JobKind::Optimize,
        JobVisibility::Workspace,
        "active maintenance",
        move |handle| {
            let _ = write_release_rx.recv();
            handle.succeed(None);
        },
    )
    .unwrap();
    wait_for_status(&registry, active_write.id, JobStatus::Running);
    let queued_ran = Arc::new(AtomicUsize::new(0));
    let queued_counter = Arc::clone(&queued_ran);
    let queued_import = spawn_job_for(
        &registry,
        &editor_a,
        JobKind::Import,
        JobVisibility::Workspace,
        "editor queued import",
        move |_| {
            queued_counter.fetch_add(1, Ordering::SeqCst);
        },
    )
    .unwrap();
    assert_eq!(
        registry.snapshot(queued_import.id).unwrap().status,
        JobStatus::Queued
    );
    assert_eq!(registry.cancel_for(&editor_a, queued_import.id), Ok(true));
    wait_for_status(&registry, queued_import.id, JobStatus::Cancelled);
    write_release_tx.send(()).unwrap();
    wait_for_status(&registry, active_write.id, JobStatus::Succeeded);
    assert_eq!(queued_ran.load(Ordering::SeqCst), 0);

    let editor_import = spawn_job_for(
        &registry,
        &editor_a,
        JobKind::Import,
        JobVisibility::Workspace,
        "editor import",
        |handle| handle.succeed(None),
    )
    .unwrap();
    let admin_maintenance = spawn_job_for(
        &registry,
        &admin,
        JobKind::Optimize,
        JobVisibility::Workspace,
        "admin optimize",
        |handle| handle.succeed(None),
    )
    .unwrap();
    let personal_import = spawn_job_for(
        &registry,
        &local_owner,
        JobKind::Import,
        JobVisibility::Workspace,
        "personal import",
        |handle| handle.succeed(None),
    )
    .unwrap();
    for id in [editor_import.id, admin_maintenance.id, personal_import.id] {
        wait_for_status(&registry, id, JobStatus::Succeeded);
    }
}

#[test]
fn pending_queues_enforce_per_user_then_workspace_limits() {
    let limits = JobQueueLimits {
        workspace_pending: 2,
        per_user_pending: 1,
        concurrent_reads: 1,
        concurrent_previews: 1,
    };
    let registry = JobRegistry::with_limits(limits);
    let editor_a = identity("editor-a-id", Role::Editor);
    let editor_b = identity("editor-b-id", Role::Editor);
    let editor_c = identity("editor-c-id", Role::Editor);
    let (release_tx, release_rx) = mpsc::channel();

    let active = spawn_job_for(
        &registry,
        &editor_a,
        JobKind::Export,
        JobVisibility::Private,
        "active export",
        move |handle| {
            let _ = release_rx.recv();
            handle.succeed(None);
        },
    )
    .unwrap();
    wait_for_status(&registry, active.id, JobStatus::Running);

    let queued_a = spawn_job_for(
        &registry,
        &editor_a,
        JobKind::Export,
        JobVisibility::Private,
        "queued A",
        |handle| handle.succeed(None),
    )
    .unwrap();
    assert!(matches!(
        spawn_job_for(
            &registry,
            &editor_a,
            JobKind::Export,
            JobVisibility::Private,
            "rejected A",
            |_| {},
        ),
        Err(JobCreateError::UserQueueFull)
    ));

    let queued_b = spawn_job_for(
        &registry,
        &editor_b,
        JobKind::Export,
        JobVisibility::Private,
        "queued B",
        |handle| handle.succeed(None),
    )
    .unwrap();
    assert!(matches!(
        spawn_job_for(
            &registry,
            &editor_c,
            JobKind::Export,
            JobVisibility::Private,
            "rejected workspace",
            |_| {},
        ),
        Err(JobCreateError::WorkspaceQueueFull)
    ));

    release_tx.send(()).unwrap();
    for id in [active.id, queued_a.id, queued_b.id] {
        wait_for_status(&registry, id, JobStatus::Succeeded);
    }
}

#[test]
fn duplicate_heavy_maintenance_is_single_flight() {
    let registry = JobRegistry::new();
    let admin = identity("admin-id", Role::Admin);
    let (release_tx, release_rx) = mpsc::channel();
    let active = spawn_job_for(
        &registry,
        &admin,
        JobKind::OlapBuild,
        JobVisibility::Workspace,
        "first projection rebuild",
        move |handle| {
            let _ = release_rx.recv();
            handle.succeed(None);
        },
    )
    .unwrap();
    wait_for_status(&registry, active.id, JobStatus::Running);

    assert!(matches!(
        spawn_job_for(
            &registry,
            &admin,
            JobKind::OlapBuild,
            JobVisibility::Workspace,
            "duplicate projection rebuild",
            |_| {},
        ),
        Err(JobCreateError::MaintenanceBusy)
    ));

    release_tx.send(()).unwrap();
    wait_for_status(&registry, active.id, JobStatus::Succeeded);
}

#[test]
fn queue_limits_are_restored_from_the_durable_job_store() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("workspace.db");
    let limits = JobQueueLimits {
        workspace_pending: 7,
        per_user_pending: 3,
        concurrent_reads: 1,
        concurrent_previews: 1,
    };

    let registry = JobRegistry::open_with_limits(&db_path, limits).unwrap();
    assert_eq!(registry.queue_limits(), limits);
    drop(registry);

    let reopened = JobRegistry::open(&db_path).unwrap();
    assert_eq!(reopened.queue_limits(), limits);
}

#[test]
fn cancellation_before_point_of_no_return_prevents_the_mutation() {
    let registry = JobRegistry::new();
    let admin = identity("admin-id", Role::Admin);
    let mutations = Arc::new(AtomicUsize::new(0));
    let worker_mutations = Arc::clone(&mutations);
    let (ready_tx, ready_rx) = mpsc::channel();
    let (continue_tx, continue_rx) = mpsc::channel();

    let job = spawn_job_for(
        &registry,
        &admin,
        JobKind::Optimize,
        JobVisibility::Workspace,
        "optimize",
        move |handle| {
            ready_tx.send(()).unwrap();
            let _ = continue_rx.recv();
            if !handle.enter_non_cancellable("checkpointing") {
                handle.mark_cancelled();
                return;
            }
            worker_mutations.fetch_add(1, Ordering::SeqCst);
            handle.succeed(None);
        },
    )
    .unwrap();
    ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(registry.cancel_for(&admin, job.id), Ok(true));
    continue_tx.send(()).unwrap();

    wait_for_status(&registry, job.id, JobStatus::Cancelled);
    assert_eq!(mutations.load(Ordering::SeqCst), 0);
    let snapshot = registry.snapshot(job.id).unwrap();
    assert!(snapshot.cancel_requested);
    assert!(!snapshot.cancellable);
}

#[test]
fn cancellation_after_point_of_no_return_is_rejected_and_job_finishes_truthfully() {
    let registry = JobRegistry::new();
    let admin = identity("admin-id", Role::Admin);
    let mutations = Arc::new(AtomicUsize::new(0));
    let worker_mutations = Arc::clone(&mutations);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let job = spawn_job_for(
        &registry,
        &admin,
        JobKind::Clear,
        JobVisibility::Workspace,
        "clear",
        move |handle| {
            assert!(handle.enter_non_cancellable("clearing"));
            entered_tx.send(()).unwrap();
            let _ = release_rx.recv();
            worker_mutations.fetch_add(1, Ordering::SeqCst);
            handle.succeed(None);
        },
    )
    .unwrap();
    entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();

    assert_eq!(registry.cancel_for(&admin, job.id), Ok(false));
    let running = registry.snapshot(job.id).unwrap();
    assert_eq!(running.status, JobStatus::Running);
    assert!(!running.cancellable);
    assert!(!running.cancel_requested);
    assert_eq!(running.progress.phase, "clearing");

    release_tx.send(()).unwrap();
    wait_for_status(&registry, job.id, JobStatus::Succeeded);
    assert_eq!(mutations.load(Ordering::SeqCst), 1);
}

#[test]
fn viewer_sees_shared_import_progress_without_private_import_details() {
    let registry = JobRegistry::new();
    let editor = identity("editor-id", Role::Editor);
    let viewer = identity("viewer-id", Role::Viewer);
    let secret = "customer-secret.xlsx";
    let job = spawn_job_for_with_input(
        &registry,
        &editor,
        JobKind::Import,
        JobVisibility::Workspace,
        format!("Importing {secret}"),
        Some(serde_json::json!({
            "files": [secret],
            "selected_sheets": ["Private sheet"],
            "sheet_semantics": {"Private sheet": {"0": "Recipient"}},
            "artifact_token": "private-upload-token"
        })),
        move |handle| {
            handle.set_progress("inserting", 5, 10);
            handle.set_message(format!("Reading {secret}"));
            handle.succeed(Some(serde_json::json!({
                "files": [{"file_name": secret, "error": "private detail"}],
                "imported": 5
            })));
        },
    )
    .unwrap();
    wait_for_status(&registry, job.id, JobStatus::Succeeded);

    let editor_snapshot = registry.snapshot_for(&editor, job.id).unwrap();
    assert!(
        serde_json::to_string(&editor_snapshot)
            .unwrap()
            .contains(secret)
    );

    let viewer_snapshot = registry.snapshot_for(&viewer, job.id).unwrap();
    let serialized = serde_json::to_string(&viewer_snapshot).unwrap();
    assert_eq!(viewer_snapshot.status, JobStatus::Succeeded);
    assert_eq!(viewer_snapshot.progress.done, 5);
    assert_eq!(viewer_snapshot.progress.total, 10);
    assert!(viewer_snapshot.input.is_none());
    assert!(viewer_snapshot.result.is_none());
    assert!(!serialized.contains(secret));
    assert!(!serialized.contains("Private sheet"));
    assert!(!serialized.contains("private-upload-token"));
    assert!(!serialized.contains("private detail"));
}
