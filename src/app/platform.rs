use std::path::{Path, PathBuf};

use rusqlite::OpenFlags;
use serde::{Deserialize, Serialize};

use crate::i18n::{Lang, tr};

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
    let stable = stable_database_path(platform, home, local_app_data, xdg_data_home);
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
    // A discovered candidate, or any database the person picked themselves —
    // their old workspace is often nowhere near this folder. What is never
    // accepted is a path that is not a Base Search database, so a confused
    // answer cannot silently point the app at some unrelated file.
    if let Some(selected) = choose_sibling(&candidates)
        && (candidates.contains(&selected) || looks_like_base_search_database(&selected))
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

/// Where a new workspace lives when nothing existing is chosen.
fn stable_database_path(
    platform: RuntimePlatform,
    home: &Path,
    local_app_data: Option<&Path>,
    xdg_data_home: Option<&Path>,
) -> PathBuf {
    match platform {
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
    }
}

const UI_LANGUAGE_FILE: &str = "ui-language-v1.txt";

/// The language the prompts that appear *before* a database is open should
/// speak — choosing a workspace, and any failure to open one.
///
/// The chosen language is stored inside the database, which is exactly what
/// these prompts are still deciding, so it is mirrored to a plain file beside
/// the default workspace whenever it changes. On the very first run no such
/// file exists and the operating system's locale is the only hint; English is
/// the last resort. On Windows the locale variables are usually unset, so a
/// person who has never opened the app there does see these two prompts in
/// English — the file makes every later run correct.
pub fn prompt_language() -> Lang {
    stored_prompt_language()
        .or_else(Lang::from_environment)
        .unwrap_or_default()
}

fn ui_language_path() -> Option<PathBuf> {
    let stable = default_stable_database_path();
    Some(stable.parent()?.join(UI_LANGUAGE_FILE))
}

fn stored_prompt_language() -> Option<Lang> {
    let raw = std::fs::read_to_string(ui_language_path()?).ok()?;
    Lang::from_locale_tag(raw.trim())
}

/// Mirrors the chosen language out of the database so the next start can use
/// it before opening one. Best effort: failing to write it costs a prompt in
/// the wrong language, never data.
pub fn remember_prompt_language(lang: Lang) {
    let Some(path) = ui_language_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, lang.code());
}

fn default_stable_database_path() -> PathBuf {
    let platform = if cfg!(target_os = "windows") {
        RuntimePlatform::Windows
    } else if cfg!(target_os = "macos") {
        RuntimePlatform::MacOs
    } else {
        RuntimePlatform::Linux
    };
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    stable_database_path(
        platform,
        &home,
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .as_deref(),
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .as_deref(),
    )
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

/// How many sibling folders are inspected before giving up. The scan only
/// stats a couple of fixed paths per folder, but the executable can sit
/// somewhere crowded, and an unbounded walk at startup is not worth the risk.
const MAX_SIBLING_PACKAGES_SCANNED: usize = 512;

/// Existing Base Search workspaces sitting beside this package.
///
/// A folder qualifies by **containing a real Base Search database**, not by
/// being named a particular way. The previous rule accepted only
/// `BaseSearch-X.Y.Z`, which is what the release scripts produce and almost
/// never what a person ends up with: someone who unzipped v1 into
/// `BaseSearch`, `Base Search 1.3`, or `basesearch_old` was invisible to it,
/// so the new version opened an empty database without a word and looked like
/// it had lost every record.
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
        .take(MAX_SIBLING_PACKAGES_SCANNED)
        .flat_map(|entry| package_database_candidates(&entry.path()))
        .filter(|database| looks_like_base_search_database(database))
        .collect::<Vec<_>>();
    // No deduplication: directory entries are unique and the two layouts below
    // are distinct paths, so a repeat is not reachable. Sorting is what matters
    // — the order decides which database the single-candidate prompt offers.
    candidates.sort();
    candidates
}

/// Where a workspace can sit inside a package folder: under `data/` for
/// anything the release scripts built, and directly beside the executable for
/// a folder someone simply unzipped and ran in place.
fn package_database_candidates(package: &Path) -> [PathBuf; 2] {
    [
        package.join("data").join("base_search.db"),
        package.join("base_search.db"),
    ]
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

    // These two prompts decide what happens to a person's data, so they are
    // the last place that should be English-only in an app with eleven
    // languages. The database is not open yet, so the language comes from the
    // mirror file written on the last run, or the system locale.
    let t = tr(prompt_language());
    if candidates.len() == 1 {
        let candidate = &candidates[0];
        let result = MessageDialog::new()
            .set_level(MessageLevel::Info)
            .set_title(t.workspace_found_title)
            .set_description(
                t.workspace_found_body
                    .replace("{}", &candidate.display().to_string()),
            )
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
        // Several answers, so none can be picked automatically — but telling a
        // person to relaunch with a command-line flag is not an answer either.
        // Offer the file picker instead, starting where the candidates are.
        let result = MessageDialog::new()
            .set_level(MessageLevel::Info)
            .set_title(t.workspace_several_title)
            .set_description(t.workspace_several_body.replace("{}", &paths))
            .set_buttons(MessageButtons::YesNo)
            .show();
        if result != MessageDialogResult::Yes {
            return None;
        }
        let mut picker = rfd::FileDialog::new().add_filter(t.workspace_database_filter, &["db"]);
        if let Some(directory) = candidates[0].parent() {
            picker = picker.set_directory(directory);
        }
        return picker.pick_file();
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

    /// Writes a file that passes `looks_like_base_search_database`.
    fn write_workspace(path: &std::path::Path, marker: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let connection = rusqlite::Connection::open(path).unwrap();
        connection
            .execute_batch(&format!(
                "CREATE TABLE records(id INTEGER PRIMARY KEY, description TEXT);
                 INSERT INTO records(description) VALUES ('{marker}');"
            ))
            .unwrap();
    }

    /// Discovery used to accept only folders named `BaseSearch-X.Y.Z`, which is
    /// what the release scripts produce and almost never what a person ends up
    /// with. Someone who unzipped v1 into a folder of their own naming was
    /// invisible, and 2.x opened an empty database without a word — which reads
    /// as "the new version lost all my data".
    #[test]
    fn a_v1_workspace_is_found_whatever_its_folder_is_called() {
        for folder in [
            "BaseSearch",
            "Base Search 1.3",
            "basesearch_old",
            "БазаПошук",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().join("home");
            let local = temp.path().join("local-app-data");
            let old = temp.path().join(folder).join("data").join("base_search.db");
            write_workspace(&old, "V1 marker");
            let executable = temp.path().join("BaseSearch-2.1.1").join("BaseSearch.exe");

            let mut offered = Vec::new();
            let selected = resolve_default_db_path(
                RuntimePlatform::Windows,
                &executable,
                Some(&home),
                Some(&local),
                None,
                |candidates| {
                    offered = candidates.to_vec();
                    candidates.first().cloned()
                },
            );

            assert_eq!(
                offered,
                vec![old.clone()],
                "folder {folder:?} was not offered"
            );
            assert_eq!(selected, old, "folder {folder:?} was not selected");
        }
    }

    /// With more than one candidate the person is asked to pick, so the order
    /// they are listed in has to be the same on every start rather than
    /// whatever the filesystem happened to return first.
    #[test]
    fn several_candidates_are_offered_in_a_stable_order() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let local = temp.path().join("local-app-data");
        let zulu = temp.path().join("zulu").join("data").join("base_search.db");
        let alpha = temp
            .path()
            .join("alpha")
            .join("data")
            .join("base_search.db");
        write_workspace(&zulu, "second");
        write_workspace(&alpha, "first");
        let executable = temp.path().join("BaseSearch-2.1.1").join("BaseSearch.exe");

        let mut offered = Vec::new();
        resolve_default_db_path(
            RuntimePlatform::Windows,
            &executable,
            Some(&home),
            Some(&local),
            None,
            |candidates| {
                offered = candidates.to_vec();
                None
            },
        );
        assert_eq!(offered, vec![alpha, zulu]);
    }

    /// A v1 folder that was simply unzipped and run in place keeps its database
    /// beside the executable rather than under `data/`.
    #[test]
    fn a_workspace_beside_the_executable_is_found_too() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let local = temp.path().join("local-app-data");
        let old = temp.path().join("old-copy").join("base_search.db");
        write_workspace(&old, "flat layout");
        let executable = temp.path().join("BaseSearch-2.1.1").join("BaseSearch.exe");

        let selected = resolve_default_db_path(
            RuntimePlatform::Windows,
            &executable,
            Some(&home),
            Some(&local),
            None,
            |candidates| candidates.first().cloned(),
        );
        assert_eq!(selected, old);
    }

    /// The name test is gone, so the content test carries the whole weight: a
    /// folder that merely looks like a package must not be offered.
    #[test]
    fn a_folder_without_a_real_database_is_never_offered() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let local = temp.path().join("local-app-data");
        let decoy = temp.path().join("BaseSearch-1.6.5").join("data");
        std::fs::create_dir_all(&decoy).unwrap();
        std::fs::write(decoy.join("base_search.db"), b"not a database at all").unwrap();
        let unrelated = temp.path().join("notes").join("data");
        std::fs::create_dir_all(&unrelated).unwrap();
        let sqlite_without_records = unrelated.join("base_search.db");
        rusqlite::Connection::open(&sqlite_without_records)
            .unwrap()
            .execute_batch("CREATE TABLE notes(id INTEGER PRIMARY KEY);")
            .unwrap();
        let executable = temp.path().join("BaseSearch-2.1.1").join("BaseSearch.exe");

        let stable = local
            .join("Base Search")
            .join("data")
            .join("base_search.db");
        assert_eq!(
            resolve_default_db_path(
                RuntimePlatform::Windows,
                &executable,
                Some(&home),
                Some(&local),
                None,
                |candidates| {
                    assert!(candidates.is_empty(), "offered {candidates:?}");
                    None
                },
            ),
            stable
        );
    }

    /// Old workspaces are often nowhere near the new folder, so a database the
    /// person picked themselves is accepted — but only a real one.
    #[test]
    fn a_hand_picked_database_is_accepted_and_a_bogus_path_is_not() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let local = temp.path().join("local-app-data");
        let elsewhere = temp.path().join("archive").join("2019").join("base.db");
        write_workspace(&elsewhere, "hand picked");
        let executable = temp.path().join("BaseSearch-2.1.1").join("BaseSearch.exe");
        let stable = local
            .join("Base Search")
            .join("data")
            .join("base_search.db");

        assert_eq!(
            resolve_default_db_path(
                RuntimePlatform::Windows,
                &executable,
                Some(&home),
                Some(&local),
                None,
                |_| Some(elsewhere.clone()),
            ),
            elsewhere,
            "a database outside the scanned folders must still be usable"
        );

        let temp2 = tempfile::tempdir().unwrap();
        let home2 = temp2.path().join("home");
        let local2 = temp2.path().join("local-app-data");
        let junk = temp2.path().join("holiday.jpg");
        std::fs::write(&junk, b"not a database").unwrap();
        let stable2 = local2
            .join("Base Search")
            .join("data")
            .join("base_search.db");
        assert_eq!(
            resolve_default_db_path(
                RuntimePlatform::Windows,
                &temp2.path().join("BaseSearch-2.1.1").join("BaseSearch.exe"),
                Some(&home2),
                Some(&local2),
                None,
                |_| Some(junk.clone()),
            ),
            stable2,
            "a path that is not a Base Search database must be refused"
        );
        let _ = stable;
    }
}
