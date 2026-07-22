use std::path::{Path, PathBuf};

use rusqlite::OpenFlags;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuntimePlatform {
    Windows,
    MacOs,
    Linux,
}

/// Selects an existing portable/V1 database when one is present. New databases
/// live in an unversioned per-user location so replacing a versioned package
/// cannot silently start a second empty workspace.
pub fn default_db_path() -> PathBuf {
    let platform = if cfg!(target_os = "windows") {
        RuntimePlatform::Windows
    } else if cfg!(target_os = "macos") {
        RuntimePlatform::MacOs
    } else {
        RuntimePlatform::Linux
    };
    let executable = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("BaseSearch"));
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    let local_app_data = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let xdg_data_home = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from);
    resolve_default_db_path(
        platform,
        &executable,
        home.as_deref(),
        local_app_data.as_deref(),
        xdg_data_home.as_deref(),
        confirm_sibling_workspace,
    )
}

fn resolve_default_db_path(
    platform: RuntimePlatform,
    executable: &Path,
    home: Option<&Path>,
    local_app_data: Option<&Path>,
    xdg_data_home: Option<&Path>,
    choose_sibling: impl FnOnce(&[PathBuf]) -> Option<PathBuf>,
) -> PathBuf {
    let home = home.unwrap_or_else(|| Path::new("."));
    let stable = match platform {
        RuntimePlatform::Windows => local_app_data
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.join("AppData").join("Local"))
            .join("Base Search")
            .join("data")
            .join("base_search.db"),
        RuntimePlatform::MacOs => home
            .join("Library")
            .join("Application Support")
            .join("Base Search")
            .join("base_search.db"),
        RuntimePlatform::Linux => xdg_data_home
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.join(".local").join("share"))
            .join("base-search")
            .join("base_search.db"),
    };
    let legacy_home = home.join(".base-search").join("base_search.db");

    if let Some(portable) = portable_db_path(platform, executable)
        && portable.is_file()
    {
        return portable;
    }
    if let Some(selected) = load_workspace_selection(&stable) {
        return selected;
    }
    if stable.is_file() {
        return stable;
    }
    if legacy_home.is_file() {
        return legacy_home;
    }
    let candidates = sibling_portable_databases(platform, executable);
    if let Some(selected) = choose_sibling(&candidates)
        && candidates.contains(&selected)
    {
        if let Err(error) = save_workspace_selection(&stable, &selected) {
            eprintln!(
                "[base-search] Could not remember the selected V1 workspace {}: {error}",
                selected.display()
            );
        }
        return selected;
    }
    stable
}

const WORKSPACE_SELECTION_SCHEMA: u32 = 1;
const WORKSPACE_SELECTION_FILE: &str = "workspace-selection-v1.json";

#[derive(Deserialize, Serialize)]
struct WorkspaceSelection {
    schema: u32,
    database_path: PathBuf,
}

fn workspace_selection_path(stable_database: &Path) -> PathBuf {
    stable_database
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(WORKSPACE_SELECTION_FILE)
}

fn load_workspace_selection(stable_database: &Path) -> Option<PathBuf> {
    let raw = std::fs::read(workspace_selection_path(stable_database)).ok()?;
    let selection: WorkspaceSelection = serde_json::from_slice(&raw).ok()?;
    (selection.schema == WORKSPACE_SELECTION_SCHEMA
        && looks_like_base_search_database(&selection.database_path))
    .then_some(selection.database_path)
}

fn save_workspace_selection(stable_database: &Path, selected: &Path) -> Result<(), String> {
    let path = workspace_selection_path(stable_database);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    let json = serde_json::to_vec_pretty(&WorkspaceSelection {
        schema: WORKSPACE_SELECTION_SCHEMA,
        database_path: selected.to_path_buf(),
    })
    .map_err(|error| error.to_string())?;
    std::fs::write(&path, json).map_err(|error| format!("{}: {error}", path.display()))
}

fn sibling_portable_databases(platform: RuntimePlatform, executable: &Path) -> Vec<PathBuf> {
    let current_package = package_root(platform, executable);
    let Some(packages_parent) = current_package.parent() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(packages_parent) else {
        return Vec::new();
    };
    let mut candidates = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path() != current_package)
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .filter(|entry| versioned_package_name(&entry.file_name().to_string_lossy()))
        .map(|entry| entry.path().join("data").join("base_search.db"))
        .filter(|database| looks_like_base_search_database(database))
        .collect::<Vec<_>>();
    candidates.sort();
    candidates
}

fn versioned_package_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let Some(remainder) = lower.strip_prefix("basesearch-") else {
        return false;
    };
    let version = remainder.split('-').next().unwrap_or_default();
    let parts = version.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn looks_like_base_search_database(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    let Ok(connection) = rusqlite::Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return false;
    };
    connection
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'records'
             )",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|exists| exists != 0)
        .unwrap_or(false)
}

fn confirm_sibling_workspace(candidates: &[PathBuf]) -> Option<PathBuf> {
    use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};

    if candidates.len() == 1 {
        let candidate = &candidates[0];
        let result = MessageDialog::new()
            .set_level(MessageLevel::Info)
            .set_title("Base Search: existing workspace found")
            .set_description(format!(
                "An existing Base Search database was found in a sibling version:\n\n{}\n\nUse this database? It will stay at its current location and will not be moved or deleted.",
                candidate.display()
            ))
            .set_buttons(MessageButtons::YesNo)
            .show();
        return (result == MessageDialogResult::Yes).then(|| candidate.clone());
    }
    if candidates.len() > 1 {
        let paths = candidates
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let _ = MessageDialog::new()
            .set_level(MessageLevel::Warning)
            .set_title("Base Search: choose an existing workspace")
            .set_description(format!(
                "Several sibling Base Search databases were found, so none was selected automatically:\n\n{paths}\n\nStart Base Search with --browser --db PATH to choose one explicitly. No database was moved or deleted."
            ))
            .set_buttons(MessageButtons::Ok)
            .show();
    }
    None
}

fn portable_db_path(platform: RuntimePlatform, executable: &Path) -> Option<PathBuf> {
    Some(
        package_root(platform, executable)
            .join("data")
            .join("base_search.db"),
    )
}

fn package_root(platform: RuntimePlatform, executable: &Path) -> PathBuf {
    let executable_dir = executable.parent().unwrap_or_else(|| Path::new("."));
    if platform == RuntimePlatform::MacOs {
        return macos_bundle_root(executable)
            .and_then(|bundle| bundle.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| executable_dir.to_path_buf());
    }
    executable_dir.to_path_buf()
}

fn macos_bundle_root(executable: &Path) -> Option<&Path> {
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app")
        .then_some(bundle)
}

pub(super) fn open_parent_folder(path: &Path) -> Result<(), String> {
    let folder = path.parent().unwrap_or_else(|| Path::new("."));
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("explorer");
        command.arg(folder);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg(folder);
        command
    };
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(folder);
        command
    };
    command
        .spawn()
        .map(|_| ())
        .map_err(|err| format!("{}: {err}", folder.display()))
}

#[cfg(test)]
mod tests {
    use super::{RuntimePlatform, resolve_default_db_path};

    #[test]
    fn versioned_packages_share_a_stable_default_but_existing_portable_data_wins() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let local = temp.path().join("local-app-data");
        let v1_exe = temp.path().join("BaseSearch-1.6.5").join("BaseSearch.exe");
        let v2_exe = temp.path().join("BaseSearch-2.0.0").join("BaseSearch.exe");

        let v1_default = resolve_default_db_path(
            RuntimePlatform::Windows,
            &v1_exe,
            Some(&home),
            Some(&local),
            None,
            |_| None,
        );
        let v2_default = resolve_default_db_path(
            RuntimePlatform::Windows,
            &v2_exe,
            Some(&home),
            Some(&local),
            None,
            |_| None,
        );
        let stable = local
            .join("Base Search")
            .join("data")
            .join("base_search.db");
        assert_eq!(v1_default, stable);
        assert_eq!(v2_default, stable);

        let portable = v2_exe.parent().unwrap().join("data").join("base_search.db");
        std::fs::create_dir_all(portable.parent().unwrap()).unwrap();
        std::fs::write(&portable, b"existing V1 database").unwrap();
        assert_eq!(
            resolve_default_db_path(
                RuntimePlatform::Windows,
                &v2_exe,
                Some(&home),
                Some(&local),
                None,
                |_| None,
            ),
            portable
        );
    }

    #[test]
    fn legacy_home_database_is_selected_instead_of_opening_an_empty_new_database() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let executable = temp.path().join("bin").join("BaseSearch");
        let legacy = home.join(".base-search").join("base_search.db");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, b"existing fallback database").unwrap();

        assert_eq!(
            resolve_default_db_path(
                RuntimePlatform::Linux,
                &executable,
                Some(&home),
                None,
                None,
                |_| None,
            ),
            legacy
        );
    }

    #[test]
    fn macos_app_uses_external_portable_data_or_application_support_never_bundle_data() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let package = temp.path().join("BaseSearch-2.0.0-macos-aarch64");
        let executable = package
            .join("BaseSearch.app")
            .join("Contents")
            .join("MacOS")
            .join("BaseSearch");
        let inside_bundle = executable
            .parent()
            .unwrap()
            .join("data")
            .join("base_search.db");
        std::fs::create_dir_all(inside_bundle.parent().unwrap()).unwrap();
        std::fs::write(&inside_bundle, b"must not be selected").unwrap();

        let stable = home
            .join("Library")
            .join("Application Support")
            .join("Base Search")
            .join("base_search.db");
        assert_eq!(
            resolve_default_db_path(
                RuntimePlatform::MacOs,
                &executable,
                Some(&home),
                None,
                None,
                |_| None,
            ),
            stable
        );

        let external_portable = package.join("data").join("base_search.db");
        std::fs::create_dir_all(external_portable.parent().unwrap()).unwrap();
        std::fs::write(&external_portable, b"portable database").unwrap();
        assert_eq!(
            resolve_default_db_path(
                RuntimePlatform::MacOs,
                &executable,
                Some(&home),
                None,
                None,
                |_| None,
            ),
            external_portable
        );

        assert_ne!(inside_bundle, external_portable);
    }

    #[test]
    fn confirmed_sibling_v1_workspace_is_reused_without_moving_or_deleting_it() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let local = temp.path().join("local-app-data");
        let old_database = temp
            .path()
            .join("BaseSearch-1.6.5")
            .join("data")
            .join("base_search.db");
        std::fs::create_dir_all(old_database.parent().unwrap()).unwrap();
        let old_connection = rusqlite::Connection::open(&old_database).unwrap();
        old_connection
            .execute_batch(
                "CREATE TABLE records(id INTEGER PRIMARY KEY, description TEXT);
                 INSERT INTO records(description) VALUES ('V1 marker');",
            )
            .unwrap();
        drop(old_connection);
        let bytes_before = std::fs::read(&old_database).unwrap();
        let v2_executable = temp.path().join("BaseSearch-2.0.0").join("BaseSearch.exe");

        let selected = resolve_default_db_path(
            RuntimePlatform::Windows,
            &v2_executable,
            Some(&home),
            Some(&local),
            None,
            |candidates| {
                assert_eq!(candidates, std::slice::from_ref(&old_database));
                Some(candidates[0].clone())
            },
        );

        assert_eq!(selected, old_database);
        assert!(old_database.is_file());
        assert_eq!(std::fs::read(&old_database).unwrap(), bytes_before);
        let stable_database = local
            .join("Base Search")
            .join("data")
            .join("base_search.db");
        assert!(
            !stable_database.exists(),
            "the V1 database must not be copied"
        );
        assert!(
            stable_database
                .parent()
                .unwrap()
                .join("workspace-selection-v1.json")
                .is_file(),
            "only an explicit workspace pointer should be persisted"
        );

        assert_eq!(
            resolve_default_db_path(
                RuntimePlatform::Windows,
                &v2_executable,
                Some(&home),
                Some(&local),
                None,
                |_| panic!("a persisted explicit selection must not prompt again"),
            ),
            old_database
        );
    }
}
