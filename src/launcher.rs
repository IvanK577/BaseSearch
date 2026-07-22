use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use serde::{Deserialize, Serialize};

use base_search::server::network::{
    TrustedIpv4Interface, discover_trusted_ipv4_interfaces, is_trusted_lan_ipv4,
    local_workspace_url, trusted_lan_workspace_url,
};

const PORT_SEARCH_WIDTH: u16 = 128;
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(200);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(12 * 60 * 60);
const MAX_LOG_LINES: usize = 80;
const LAUNCHER_PREFERENCES_VERSION: u8 = 1;
static PREFERENCE_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum WorkspaceMode {
    #[default]
    Personal,
    TrustedLan,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceUrls {
    local: String,
    lan: Option<String>,
}

fn workspace_urls(mode: WorkspaceMode, port: u16, lan_address: Option<Ipv4Addr>) -> WorkspaceUrls {
    WorkspaceUrls {
        local: local_workspace_url(port),
        lan: if mode == WorkspaceMode::TrustedLan {
            lan_address.and_then(|address| trusted_lan_workspace_url(address, port))
        } else {
            None
        },
    }
}

fn selected_bind_host(
    mode: WorkspaceMode,
    lan_address: Option<Ipv4Addr>,
) -> Result<Ipv4Addr, String> {
    match mode {
        WorkspaceMode::Personal => Ok(Ipv4Addr::LOCALHOST),
        WorkspaceMode::TrustedLan => match lan_address {
            Some(address) if is_trusted_lan_ipv4(address) => Ok(address),
            Some(address) => Err(format!(
                "The selected address {address} is not a private LAN or CGNAT VPN address."
            )),
            None => Err(
                "No usable private LAN or VPN IPv4 interface is available. Connect this computer to a trusted network and refresh interfaces."
                    .to_string(),
            ),
        },
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct LauncherPreferences {
    version: u8,
    mode: WorkspaceMode,
    preferred_port: u16,
}

impl LauncherPreferences {
    fn defaults(preferred_port: u16) -> Self {
        Self {
            version: LAUNCHER_PREFERENCES_VERSION,
            mode: WorkspaceMode::Personal,
            preferred_port,
        }
    }

    fn is_valid(&self) -> bool {
        self.version == LAUNCHER_PREFERENCES_VERSION && self.preferred_port != 0
    }
}

struct LoadedLauncherPreferences {
    preferences: LauncherPreferences,
    warning: Option<String>,
}

fn launcher_preferences_path(db_path: &Path) -> PathBuf {
    let file_name = db_path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("base_search.db");
    db_path.with_file_name(format!("{file_name}.launcher.json"))
}

fn load_launcher_preferences(path: &Path, default_port: u16) -> LoadedLauncherPreferences {
    let defaults = LauncherPreferences::defaults(default_port);
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return LoadedLauncherPreferences {
                preferences: defaults,
                warning: None,
            };
        }
        Err(error) => {
            return LoadedLauncherPreferences {
                preferences: defaults,
                warning: Some(format!(
                    "Launcher preferences could not be read and were ignored: {error}"
                )),
            };
        }
    };

    match serde_json::from_str::<LauncherPreferences>(&text) {
        Ok(preferences) if preferences.is_valid() => LoadedLauncherPreferences {
            preferences,
            warning: None,
        },
        Ok(_) | Err(_) => LoadedLauncherPreferences {
            preferences: defaults,
            warning: Some(
                "Launcher preferences are invalid and Personal defaults were restored.".to_string(),
            ),
        },
    }
}

fn save_launcher_preferences(path: &Path, preferences: &LauncherPreferences) -> Result<(), String> {
    if !preferences.is_valid() {
        return Err("launcher preferences are invalid".to_string());
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create launcher preferences folder: {error}"))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("launcher.json");
    let sequence = PREFERENCE_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp_path = parent.join(format!(
        ".{file_name}.tmp-{}-{sequence}",
        std::process::id()
    ));
    let bytes = serde_json::to_vec_pretty(preferences)
        .map_err(|error| format!("cannot encode launcher preferences: {error}"))?;

    let write_result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        replace_file_atomically(&temp_path, path)?;
        sync_parent_directory(parent)?;
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp_path);
        return Err(format!("cannot save launcher preferences: {error}"));
    }
    Ok(())
}

type AccountSummary = (String, String, String);

fn bootstrap_first_owner_with(
    db_path: &Path,
    username: &str,
    password: &str,
    confirmation: &str,
    list_accounts: impl FnOnce(&Path) -> Result<Vec<AccountSummary>, String>,
    add_account: impl FnOnce(&Path, &str, &str, &str) -> Result<(), String>,
) -> Result<(), String> {
    let username = username.trim();
    if username.is_empty() {
        return Err("Username cannot be empty.".to_string());
    }
    if username.chars().count() > 128 {
        return Err("Username cannot exceed 128 characters.".to_string());
    }
    if password.len() < 8 {
        return Err("Password must be at least 8 characters.".to_string());
    }
    if password != confirmation {
        return Err("Password and confirmation must match.".to_string());
    }
    if !list_accounts(db_path)?.is_empty() {
        return Err("Workspace accounts already exist.".to_string());
    }
    add_account(db_path, username, password, "owner")
}

fn bootstrap_first_owner(
    db_path: &Path,
    username: &str,
    password: &str,
    confirmation: &str,
) -> Result<(), String> {
    if let Some(parent) = db_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Cannot create the database folder: {error}"))?;
    }
    bootstrap_first_owner_with(
        db_path,
        username,
        password,
        confirmation,
        base_search::server::list_accounts,
        base_search::server::add_account,
    )
}

fn validate_start_requirements(
    mode: WorkspaceMode,
    lan_confirmed: bool,
    account_ready: bool,
    lan_address: Option<Ipv4Addr>,
) -> Result<(), String> {
    if mode == WorkspaceMode::Personal {
        return Ok(());
    }
    if !account_ready {
        return Err("Create the first owner account before starting LAN mode.".to_string());
    }
    if !lan_confirmed {
        return Err("Please confirm that this is a trusted LAN or VPN.".to_string());
    }
    selected_bind_host(mode, lan_address).map(|_| ())
}

#[cfg(target_os = "windows")]
fn replace_file_atomically(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
    }

    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: both pointers reference NUL-terminated UTF-16 buffers that remain
    // alive for the duration of the Windows API call.
    let moved = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(target_os = "windows"))]
fn replace_file_atomically(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> io::Result<()> {
    fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> io::Result<()> {
    Ok(())
}

#[derive(Clone, Debug)]
pub struct LauncherConfig {
    pub db_path: PathBuf,
    pub preferred_port: u16,
    pub open_browser: bool,
}

impl LauncherConfig {
    pub fn local(db_path: PathBuf) -> Self {
        Self {
            db_path,
            preferred_port: base_search::server::DEFAULT_PORT,
            open_browser: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LaunchStatus {
    Starting,
    Ready,
    Error(String),
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LaunchEvent {
    Prepared { urls: WorkspaceUrls },
    Progress(String),
    Output(String),
    Ready,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LaunchAction {
    OpenBrowser(String),
}

struct LauncherModel {
    db_path: PathBuf,
    mode: WorkspaceMode,
    status: LaunchStatus,
    stage: String,
    open_on_ready: bool,
    generation: u64,
    local_url: Option<String>,
    lan_url: Option<String>,
    browser_opened: bool,
}

#[derive(Clone, Default)]
struct ProcessSlot {
    state: Arc<Mutex<ManagedProcess>>,
}

#[derive(Default)]
struct ManagedProcess {
    generation: u64,
    child: Option<Child>,
}

impl ProcessSlot {
    fn activate(&self, generation: u64) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "local server process lock is poisoned".to_string())?;
        stop_child(&mut state.child)?;
        state.generation = generation;
        Ok(())
    }

    fn replace_for_generation(&self, generation: u64, child: Child) -> Result<bool, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "local server process lock is poisoned".to_string())?;
        if state.generation != generation {
            drop(state);
            terminate_child(child)?;
            return Ok(false);
        }
        stop_child(&mut state.child)?;
        state.child = Some(child);
        Ok(true)
    }

    fn stop_for_generation(&self, generation: u64) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "local server process lock is poisoned".to_string())?;
        if state.generation == generation {
            stop_child(&mut state.child)?;
        }
        Ok(())
    }

    fn stop_all(&self) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "local server process lock is poisoned".to_string())?;
        stop_child(&mut state.child)
    }

    fn is_generation_active(&self, generation: u64) -> bool {
        self.state
            .lock()
            .is_ok_and(|state| state.generation == generation)
    }

    fn is_running_for_generation(&self, generation: u64) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.generation != generation {
            return false;
        }
        let Some(child) = state.child.as_mut() else {
            return false;
        };
        match child.try_wait() {
            Ok(None) => true,
            Ok(Some(_)) | Err(_) => {
                state.child = None;
                false
            }
        }
    }
}

fn stop_child(child: &mut Option<Child>) -> Result<(), String> {
    let Some(child) = child.take() else {
        return Ok(());
    };
    terminate_child(child)
}

fn terminate_child(mut child: Child) -> Result<(), String> {
    if child
        .try_wait()
        .map_err(|err| format!("cannot inspect local server process: {err}"))?
        .is_none()
    {
        child
            .kill()
            .map_err(|err| format!("cannot stop local server process: {err}"))?;
    }
    child
        .wait()
        .map_err(|err| format!("cannot finish stopping local server process: {err}"))?;
    Ok(())
}

impl LauncherModel {
    fn new(db_path: PathBuf, open_on_ready: bool) -> Self {
        Self {
            db_path,
            mode: WorkspaceMode::Personal,
            status: LaunchStatus::Stopped,
            stage: String::new(),
            open_on_ready,
            generation: 0,
            local_url: None,
            lan_url: None,
            browser_opened: false,
        }
    }

    fn begin_start(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1).max(1);
        self.status = LaunchStatus::Starting;
        self.stage = "Preparing the local workspace".to_string();
        self.local_url = None;
        self.lan_url = None;
        self.browser_opened = false;
        self.generation
    }

    fn set_mode(&mut self, mode: WorkspaceMode) -> Result<(), String> {
        if matches!(self.status, LaunchStatus::Starting | LaunchStatus::Ready) {
            return Err("Stop the workspace before changing its mode.".to_string());
        }
        self.mode = mode;
        Ok(())
    }

    #[cfg(test)]
    fn mode(&self) -> WorkspaceMode {
        self.mode
    }

    #[cfg(test)]
    fn local_url(&self) -> Option<&str> {
        self.local_url.as_deref()
    }

    #[cfg(test)]
    fn lan_url(&self) -> Option<&str> {
        self.lan_url.as_deref()
    }

    fn stop(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1).max(1);
        self.status = LaunchStatus::Stopped;
        self.stage = "Local server stopped".to_string();
        self.local_url = None;
        self.lan_url = None;
        self.generation
    }

    #[cfg(test)]
    fn status(&self) -> LaunchStatus {
        self.status.clone()
    }

    #[cfg(test)]
    fn stage(&self) -> &str {
        &self.stage
    }

    fn apply(&mut self, generation: u64, event: LaunchEvent) -> Option<LaunchAction> {
        if generation != self.generation {
            return None;
        }
        match event {
            LaunchEvent::Prepared { urls } => {
                self.local_url = Some(urls.local);
                self.lan_url = urls.lan;
                self.stage = "Opening the database".to_string();
                None
            }
            LaunchEvent::Progress(stage) => {
                self.stage = stage;
                None
            }
            LaunchEvent::Output(_) => None,
            LaunchEvent::Ready => {
                self.status = LaunchStatus::Ready;
                self.stage = "Workspace ready".to_string();
                if self.open_on_ready && !self.browser_opened {
                    let url = self.local_url.clone()?;
                    self.browser_opened = true;
                    Some(LaunchAction::OpenBrowser(url))
                } else {
                    None
                }
            }
            LaunchEvent::Failed(message) => {
                self.stage = message.clone();
                self.status = LaunchStatus::Error(message);
                // A workspace that failed to start has no working URL; keeping
                // the last prepared one would show a dead link.
                self.local_url = None;
                self.lan_url = None;
                None
            }
        }
    }
}

#[derive(Debug)]
struct EventEnvelope {
    generation: u64,
    event: LaunchEvent,
}

struct LauncherController {
    model: LauncherModel,
    process: ProcessSlot,
    executable: PathBuf,
    preferred_port: u16,
    preferences_path: PathBuf,
    events_tx: Sender<EventEnvelope>,
    events_rx: Receiver<EventEnvelope>,
    logs: VecDeque<String>,
    started_at: Option<Instant>,
    lan_bind_address: Option<Ipv4Addr>,
}

impl LauncherController {
    fn new(config: LauncherConfig, executable: PathBuf) -> Self {
        let (events_tx, events_rx) = mpsc::channel();
        let preferences_path = launcher_preferences_path(&config.db_path);
        Self {
            model: LauncherModel::new(config.db_path, config.open_browser),
            process: ProcessSlot::default(),
            executable,
            preferred_port: config.preferred_port,
            preferences_path,
            events_tx,
            events_rx,
            logs: VecDeque::new(),
            started_at: None,
            lan_bind_address: None,
        }
    }

    fn set_lan_bind_address(&mut self, address: Option<Ipv4Addr>) {
        self.lan_bind_address = address;
    }

    fn update_preferences(
        &mut self,
        mode: WorkspaceMode,
        preferred_port: u16,
    ) -> Result<(), String> {
        if preferred_port == 0 {
            return Err("Preferred port must be between 1 and 65535.".to_string());
        }
        if matches!(
            self.model.status,
            LaunchStatus::Starting | LaunchStatus::Ready
        ) {
            return Err("Stop the workspace before changing launcher settings.".to_string());
        }
        let preferences = LauncherPreferences {
            version: LAUNCHER_PREFERENCES_VERSION,
            mode,
            preferred_port,
        };
        save_launcher_preferences(&self.preferences_path, &preferences)?;
        self.model.mode = mode;
        self.preferred_port = preferred_port;
        Ok(())
    }

    fn start(&mut self) {
        let generation = self.model.begin_start();
        if let Err(error) = self.process.activate(generation) {
            self.set_error(error);
            return;
        }
        self.started_at = Some(Instant::now());
        self.logs.clear();

        let executable = self.executable.clone();
        let db_path = self.model.db_path.clone();
        let mode = self.model.mode;
        let lan_bind_address = self.lan_bind_address;
        let preferred_port = self.preferred_port;
        let process = self.process.clone();
        let events = self.events_tx.clone();
        std::thread::spawn(move || {
            start_server_worker(ServerWorkerRequest {
                executable,
                db_path,
                mode,
                lan_bind_address,
                preferred_port,
                process,
                generation,
                events,
            });
        });
    }

    fn stop(&mut self) -> Result<(), String> {
        let generation = self.model.stop();
        self.started_at = None;
        self.process.activate(generation)
    }

    fn restart(&mut self) {
        match self.stop() {
            Ok(()) => self.start(),
            Err(error) => self.set_error(error),
        }
    }

    fn poll(&mut self) {
        while let Ok(envelope) = self.events_rx.try_recv() {
            if envelope.generation != self.model.generation {
                continue;
            }
            if let LaunchEvent::Output(line) = &envelope.event {
                self.push_log(line.clone());
            }
            if let Some(action) = self.model.apply(envelope.generation, envelope.event) {
                self.apply_action(action);
            }
        }

        if self.model.status == LaunchStatus::Ready
            && !self
                .process
                .is_running_for_generation(self.model.generation)
        {
            self.set_error("The local server stopped unexpectedly. Restart it to continue.");
        }
    }

    fn apply_action(&mut self, action: LaunchAction) {
        match action {
            LaunchAction::OpenBrowser(url) => {
                if let Err(error) = open_url(&url) {
                    self.push_log(format!("Could not open the default browser: {error}"));
                    self.model.stage = "Workspace ready; open the URL below manually".to_string();
                }
            }
        }
    }

    fn open_workspace(&mut self) {
        let Some(url) = self.model.local_url.clone() else {
            return;
        };
        if let Err(error) = open_url(&url) {
            self.push_log(format!("Could not open the default browser: {error}"));
        }
    }

    fn open_legacy(&mut self) {
        if let Err(error) = spawn_legacy_workspace(&self.executable) {
            self.push_log(format!("Could not open the legacy workspace: {error}"));
        }
    }

    fn set_error(&mut self, error: impl Into<String>) {
        let error = error.into();
        let generation = self.model.generation;
        let _ = self
            .model
            .apply(generation, LaunchEvent::Failed(error.clone()));
        self.push_log(error);
    }

    fn push_log(&mut self, line: String) {
        let line = line.trim().to_string();
        if line.is_empty() {
            return;
        }
        if self.logs.len() == MAX_LOG_LINES {
            self.logs.pop_front();
        }
        self.logs.push_back(line);
    }

    fn elapsed(&self) -> Option<Duration> {
        self.started_at.map(|started| started.elapsed())
    }
}

impl Drop for LauncherController {
    fn drop(&mut self) {
        let _ = self.process.stop_all();
    }
}

struct LauncherApp {
    controller: LauncherController,
    lan_account_state: LanAccountState,
    lan_interfaces: Vec<TrustedIpv4Interface>,
    selected_lan_address: Option<Ipv4Addr>,
    lan_interface_error: Option<String>,
    lan_confirmed: bool,
    owner_username: String,
    owner_password: String,
    owner_confirmation: String,
    owner_feedback: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LanAccountState {
    NotChecked,
    NeedsOwner,
    Ready,
    Error(String),
}

impl LauncherApp {
    fn new(mut config: LauncherConfig, executable: PathBuf) -> Self {
        let preferences_path = launcher_preferences_path(&config.db_path);
        let loaded = load_launcher_preferences(&preferences_path, config.preferred_port);
        config.preferred_port = loaded.preferences.preferred_port;
        let mut controller = LauncherController::new(config, executable);
        let _ = controller.model.set_mode(loaded.preferences.mode);
        if let Some(warning) = loaded.warning {
            controller.push_log(warning);
        }
        let mut app = Self {
            controller,
            lan_account_state: LanAccountState::NotChecked,
            lan_interfaces: Vec::new(),
            selected_lan_address: None,
            lan_interface_error: None,
            lan_confirmed: false,
            owner_username: String::new(),
            owner_password: String::new(),
            owner_confirmation: String::new(),
            owner_feedback: None,
        };
        if app.controller.model.mode == WorkspaceMode::TrustedLan {
            app.refresh_lan_accounts();
            app.refresh_lan_interfaces();
        }
        app
    }

    fn refresh_lan_interfaces(&mut self) {
        let previous = self.selected_lan_address;
        match discover_trusted_ipv4_interfaces() {
            Ok(interfaces) => {
                self.lan_interfaces = interfaces;
                self.selected_lan_address = previous
                    .filter(|address| {
                        self.lan_interfaces
                            .iter()
                            .any(|interface| interface.address == *address)
                    })
                    .or_else(|| {
                        self.lan_interfaces
                            .first()
                            .map(|interface| interface.address)
                    });
                self.lan_interface_error = self.lan_interfaces.is_empty().then(|| {
                    "No private LAN or VPN IPv4 interface is available. Connect to a trusted network, then refresh interfaces."
                        .to_string()
                });
            }
            Err(error) => {
                self.lan_interfaces.clear();
                self.selected_lan_address = None;
                self.lan_interface_error =
                    Some(format!("Network interfaces could not be read: {error}"));
            }
        }
        self.controller
            .set_lan_bind_address(self.selected_lan_address);
    }

    fn refresh_lan_accounts(&mut self) {
        let db_path = &self.controller.model.db_path;
        if let Some(parent) = db_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            && let Err(error) = fs::create_dir_all(parent)
        {
            self.lan_account_state =
                LanAccountState::Error(format!("Cannot create the database folder: {error}"));
            return;
        }
        self.lan_account_state = match base_search::server::list_accounts(db_path) {
            Ok(accounts) if accounts.is_empty() => LanAccountState::NeedsOwner,
            Ok(_) => LanAccountState::Ready,
            Err(error) => LanAccountState::Error(error),
        };
    }

    fn create_first_owner(&mut self) -> Result<(), String> {
        let result = bootstrap_first_owner(
            &self.controller.model.db_path,
            &self.owner_username,
            &self.owner_password,
            &self.owner_confirmation,
        );
        match result {
            Ok(()) => {
                self.owner_password.clear();
                self.owner_confirmation.clear();
                self.refresh_lan_accounts();
                if self.lan_account_state != LanAccountState::Ready {
                    let error =
                        "The owner account was created but could not be reloaded.".to_string();
                    self.owner_feedback = Some(error.clone());
                    return Err(error);
                }
                self.owner_feedback =
                    Some("Owner account created. LAN sign-in is ready.".to_string());
                Ok(())
            }
            Err(error) => {
                self.owner_feedback = Some(error.clone());
                Err(error)
            }
        }
    }

    fn can_configure(&self) -> bool {
        !matches!(
            self.controller.model.status,
            LaunchStatus::Starting | LaunchStatus::Ready
        )
    }

    fn account_ready(&self) -> bool {
        self.lan_account_state == LanAccountState::Ready
    }

    fn apply_preferences(&mut self, mode: WorkspaceMode, preferred_port: u16) {
        let mode_changed = mode != self.controller.model.mode;
        match self.controller.update_preferences(mode, preferred_port) {
            Ok(()) => {
                if mode_changed {
                    self.lan_confirmed = false;
                    self.owner_feedback = None;
                    if mode == WorkspaceMode::TrustedLan {
                        self.refresh_lan_accounts();
                        self.refresh_lan_interfaces();
                    } else {
                        self.lan_account_state = LanAccountState::NotChecked;
                        self.selected_lan_address = None;
                        self.lan_interfaces.clear();
                        self.lan_interface_error = None;
                        self.controller.set_lan_bind_address(None);
                    }
                }
            }
            Err(error) => self.controller.push_log(error),
        }
    }

    fn start_selected_workspace(&mut self) {
        let result = validate_start_requirements(
            self.controller.model.mode,
            self.lan_confirmed,
            self.account_ready(),
            self.selected_lan_address,
        );
        match result {
            Ok(()) => {
                self.owner_feedback = None;
                self.controller
                    .set_lan_bind_address(self.selected_lan_address);
                self.controller.start();
            }
            Err(error) => self.owner_feedback = Some(error),
        }
    }

    fn stop_workspace(&mut self) {
        if let Err(error) = self.controller.stop() {
            self.controller.set_error(error);
        } else if self.controller.model.mode == WorkspaceMode::TrustedLan {
            self.lan_confirmed = false;
        }
    }
}

impl LauncherApp {
    fn render_root(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::Frame::NONE
                        .inner_margin(egui::Margin::same(18))
                        .show(ui, |ui| self.render_content(ui));
                });
        });
    }

    fn render_content(&mut self, ui: &mut egui::Ui) {
        ui.heading("Base Search");
        ui.label("Choose how this workspace should run, then start it.");
        ui.add_space(16.0);

        ui.label(egui::RichText::new("Workspace mode").strong());
        let can_configure = self.can_configure();
        let current_mode = self.controller.model.mode;
        let mut selected_mode = current_mode;
        ui.add_enabled_ui(can_configure, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(
                    &mut selected_mode,
                    WorkspaceMode::Personal,
                    "Personal workspace",
                );
                ui.selectable_value(
                    &mut selected_mode,
                    WorkspaceMode::TrustedLan,
                    "Trusted LAN workspace",
                );
            });
        });
        if selected_mode != current_mode {
            self.apply_preferences(selected_mode, self.controller.preferred_port);
        }

        let mut preferred_port = self.controller.preferred_port;
        ui.horizontal(|ui| {
            ui.label("Preferred port");
            let response = ui.add_enabled(
                can_configure,
                egui::DragValue::new(&mut preferred_port)
                    .range(1..=u16::MAX)
                    .speed(1),
            );
            if response.changed() {
                self.apply_preferences(self.controller.model.mode, preferred_port);
            }
        });

        match self.controller.model.mode {
            WorkspaceMode::Personal => {
                ui.label("For one person on this computer. Sign-in is not required.");
            }
            WorkspaceMode::TrustedLan => {
                ui.add_space(8.0);
                ui.colored_label(
                    egui::Color32::from_rgb(232, 166, 82),
                    egui::RichText::new("Trusted network only").strong(),
                );
                ui.label(
                            "LAN mode shares this workspace over HTTP. Use it only on a trusted LAN or VPN; never expose the port directly to the public internet.",
                        );
                ui.add_space(8.0);

                ui.label(egui::RichText::new("Network interface").strong());
                if self.lan_interfaces.is_empty() {
                    if let Some(error) = &self.lan_interface_error {
                        ui.colored_label(egui::Color32::from_rgb(225, 90, 80), error);
                    }
                    if ui.button("Refresh interfaces").clicked() {
                        self.refresh_lan_interfaces();
                    }
                } else {
                    let previous = self.selected_lan_address;
                    let selected_text = self
                        .lan_interfaces
                        .iter()
                        .find(|interface| Some(interface.address) == self.selected_lan_address)
                        .map(|interface| format!("{} - {}", interface.name, interface.address))
                        .unwrap_or_else(|| "Select an interface".to_string());
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("lan-interface")
                            .selected_text(selected_text)
                            .show_ui(ui, |ui| {
                                for interface in &self.lan_interfaces {
                                    ui.selectable_value(
                                        &mut self.selected_lan_address,
                                        Some(interface.address),
                                        format!("{} - {}", interface.name, interface.address),
                                    );
                                }
                            });
                        if ui.button("Refresh").clicked() {
                            self.refresh_lan_interfaces();
                        }
                    });
                    if previous != self.selected_lan_address {
                        self.lan_confirmed = false;
                        self.controller
                            .set_lan_bind_address(self.selected_lan_address);
                    }
                }
                ui.add_space(8.0);

                match self.lan_account_state.clone() {
                    LanAccountState::NotChecked => {
                        if ui.button("Check accounts").clicked() {
                            self.refresh_lan_accounts();
                        }
                    }
                    LanAccountState::NeedsOwner => {
                        ui.label(egui::RichText::new("Create the first owner").strong());
                        ui.label("This account protects the workspace when other devices connect.");
                        ui.horizontal(|ui| {
                            ui.label("Username");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.owner_username)
                                    .desired_width(220.0),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label("Password");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.owner_password)
                                    .password(true)
                                    .desired_width(220.0),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label("Confirm");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.owner_confirmation)
                                    .password(true)
                                    .desired_width(220.0),
                            );
                        });
                        if ui.button("Create owner").clicked() {
                            let _ = self.create_first_owner();
                        }
                    }
                    LanAccountState::Ready => {
                        ui.label("Account protection is ready. LAN visitors must sign in.");
                    }
                    LanAccountState::Error(error) => {
                        ui.colored_label(egui::Color32::from_rgb(225, 90, 80), error);
                        if ui.button("Check again").clicked() {
                            self.refresh_lan_accounts();
                        }
                    }
                }

                if let Some(feedback) = &self.owner_feedback {
                    let color = if self.account_ready() {
                        egui::Color32::from_rgb(90, 190, 125)
                    } else {
                        egui::Color32::from_rgb(225, 90, 80)
                    };
                    ui.colored_label(color, feedback);
                }
                ui.checkbox(
                    &mut self.lan_confirmed,
                    "I confirm this is a trusted LAN or VPN.",
                );
            }
        }

        ui.add_space(14.0);
        ui.separator();
        ui.add_space(12.0);

        ui.horizontal(|ui| {
            if self.controller.model.status == LaunchStatus::Starting {
                ui.spinner();
            }
            let (label, color) = match &self.controller.model.status {
                LaunchStatus::Starting => ("Starting", egui::Color32::from_rgb(226, 153, 68)),
                LaunchStatus::Ready => ("Ready", egui::Color32::from_rgb(90, 190, 125)),
                LaunchStatus::Error(_) => ("Error", egui::Color32::from_rgb(225, 90, 80)),
                LaunchStatus::Stopped => ("Stopped", egui::Color32::GRAY),
            };
            ui.colored_label(color, egui::RichText::new(label).strong());
            if self.controller.model.stage.is_empty() {
                ui.label("Waiting to start");
            } else {
                ui.label(&self.controller.model.stage);
            }
        });

        if self.controller.model.status == LaunchStatus::Starting
            && let Some(elapsed) = self.controller.elapsed()
        {
            ui.label(format!("Elapsed: {}s", elapsed.as_secs()));
        }

        ui.add_space(14.0);
        ui.label(egui::RichText::new("Database").strong());
        ui.add(
            egui::Label::new(
                egui::RichText::new(self.controller.model.db_path.display().to_string())
                    .monospace(),
            )
            .selectable(true)
            .wrap(),
        );
        ui.add_space(8.0);
        let ready = self.controller.model.status == LaunchStatus::Ready;
        ui.label(egui::RichText::new("Local URL").strong());
        if let Some(url) = &self.controller.model.local_url {
            // The URL becomes a working link only once the server answered a
            // health check; before that it is informational text.
            if ready {
                ui.hyperlink_to(url, url);
            } else {
                ui.label(egui::RichText::new(format!("{url} (starting…)")).monospace());
            }
        } else {
            ui.label("Available after the workspace starts");
        }
        if self.controller.model.mode == WorkspaceMode::TrustedLan {
            ui.add_space(8.0);
            ui.label(egui::RichText::new("LAN URL").strong());
            if let Some(url) = &self.controller.model.lan_url {
                if ready {
                    ui.hyperlink_to(url, url);
                } else {
                    ui.label(egui::RichText::new(format!("{url} (starting…)")).monospace());
                }
            } else if self.controller.model.status == LaunchStatus::Ready {
                ui.label(
                            "A private LAN address could not be detected. Check this computer's network address.",
                        );
            } else {
                ui.label("Available after the workspace starts");
            }
        }

        ui.add_space(18.0);
        ui.horizontal(|ui| match self.controller.model.status.clone() {
            LaunchStatus::Starting => {
                if ui.button("Stop").clicked() {
                    self.stop_workspace();
                }
            }
            LaunchStatus::Ready => {
                if ui.button("Open workspace").clicked() {
                    self.controller.open_workspace();
                }
                if ui.button("Restart").clicked() {
                    self.controller.restart();
                }
                if ui.button("Stop").clicked() {
                    self.stop_workspace();
                }
            }
            LaunchStatus::Error(_) => {
                let can_start = validate_start_requirements(
                    self.controller.model.mode,
                    self.lan_confirmed,
                    self.account_ready(),
                    self.selected_lan_address,
                )
                .is_ok();
                if ui
                    .add_enabled(can_start, egui::Button::new("Start again"))
                    .clicked()
                {
                    self.start_selected_workspace();
                }
            }
            LaunchStatus::Stopped => {
                let can_start = validate_start_requirements(
                    self.controller.model.mode,
                    self.lan_confirmed,
                    self.account_ready(),
                    self.selected_lan_address,
                )
                .is_ok();
                if ui
                    .add_enabled(can_start, egui::Button::new("Start workspace"))
                    .clicked()
                {
                    self.start_selected_workspace();
                }
            }
        });

        ui.add_space(14.0);
        egui::CollapsingHeader::new("Startup details")
            .default_open(matches!(
                &self.controller.model.status,
                LaunchStatus::Starting | LaunchStatus::Error(_)
            ))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(120.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        if self.controller.logs.is_empty() {
                            ui.label("Waiting for startup output...");
                        } else {
                            for line in &self.controller.logs {
                                ui.monospace(line);
                            }
                        }
                    });
            });

        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            if ui
                .button("Open legacy desktop workspace (fallback)")
                .clicked()
            {
                self.controller.open_legacy();
            }
            ui.label("The legacy workspace is kept only as an explicit fallback.");
        });
    }
}

impl eframe::App for LauncherApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.controller.poll();
        ctx.request_repaint_after(match &self.controller.model.status {
            LaunchStatus::Starting => Duration::from_millis(100),
            _ => Duration::from_millis(500),
        });

        self.render_root(ui);
    }
}

pub fn run(config: LauncherConfig) -> Result<(), String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("cannot locate the Base Search executable: {error}"))?;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Base Search")
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!(
                    "../web-ui/public/base-search-icon.png"
                ))
                .expect("embedded Base Search icon must be a valid PNG"),
            )
            .with_inner_size([680.0, 620.0])
            .with_min_inner_size([560.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Base Search",
        options,
        Box::new(move |_cc| Ok(Box::new(LauncherApp::new(config, executable)))),
    )
    .map_err(|error| format!("launcher window failed: {error}"))
}

struct ServerWorkerRequest {
    executable: PathBuf,
    db_path: PathBuf,
    mode: WorkspaceMode,
    lan_bind_address: Option<Ipv4Addr>,
    preferred_port: u16,
    process: ProcessSlot,
    generation: u64,
    events: Sender<EventEnvelope>,
}

fn start_server_worker(request: ServerWorkerRequest) {
    let ServerWorkerRequest {
        executable,
        db_path,
        mode,
        lan_bind_address,
        preferred_port,
        process,
        generation,
        events,
    } = request;
    if !process.is_generation_active(generation) {
        return;
    }
    let bind_host = match selected_bind_host(mode, lan_bind_address) {
        Ok(address) => address,
        Err(error) => {
            send_event(&events, generation, LaunchEvent::Failed(error));
            return;
        }
    };
    let listener_hosts = if mode == WorkspaceMode::TrustedLan {
        vec![bind_host, Ipv4Addr::LOCALHOST]
    } else {
        vec![Ipv4Addr::LOCALHOST]
    };
    // If a healthy Base Search already answers on the preferred port, refuse
    // to silently start a second server (usually against the same database)
    // on a neighboring port — that is how users end up with several launcher
    // windows showing URLs that do not respond.
    if preferred_port != 0
        && health_is_ready(SocketAddr::from((Ipv4Addr::LOCALHOST, preferred_port)))
    {
        send_event(
            &events,
            generation,
            LaunchEvent::Failed(format!(
                "A Base Search workspace is already running at http://127.0.0.1:{preferred_port}/. Use that window or browser tab, or stop the other copy before starting a new workspace. To run a second independent workspace on purpose, choose a different preferred port first."
            )),
        );
        return;
    }
    let port = match select_available_port(&listener_hosts, preferred_port, PORT_SEARCH_WIDTH) {
        Ok(port) => port,
        Err(error) => {
            send_event(
                &events,
                generation,
                LaunchEvent::Failed(format!("Cannot select a local port: {error}")),
            );
            return;
        }
    };
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let urls = workspace_urls(mode, port, lan_bind_address);
    send_event(&events, generation, LaunchEvent::Prepared { urls });

    let mut command = build_server_command(executable.clone(), db_path, port, bind_host);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(parent) = executable.parent() {
        command.current_dir(parent);
    }
    configure_background_command(&mut command);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            send_event(
                &events,
                generation,
                LaunchEvent::Failed(format!("Cannot start the local server: {error}")),
            );
            return;
        }
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    match process.replace_for_generation(generation, child) {
        Ok(true) => {}
        Ok(false) => return,
        Err(error) => {
            send_event(&events, generation, LaunchEvent::Failed(error));
            return;
        }
    }
    if let Some(stdout) = stdout {
        forward_output(stdout, generation, events.clone());
    }
    if let Some(stderr) = stderr {
        forward_output(stderr, generation, events.clone());
    }
    send_event(
        &events,
        generation,
        LaunchEvent::Progress("Opening the database and applying upgrades".to_string()),
    );

    match wait_for_health(address, STARTUP_TIMEOUT, HEALTH_POLL_INTERVAL, || {
        process.is_running_for_generation(generation)
    }) {
        Ok(()) => send_event(&events, generation, LaunchEvent::Ready),
        Err(error) => {
            let _ = process.stop_for_generation(generation);
            send_event(&events, generation, LaunchEvent::Failed(error));
        }
    }
}

fn forward_output(
    reader: impl Read + Send + 'static,
    generation: u64,
    events: Sender<EventEnvelope>,
) {
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            let Ok(line) = line else {
                break;
            };
            send_event(&events, generation, LaunchEvent::Output(line.clone()));
            if let Some(progress) = startup_progress_text(&line) {
                send_event(&events, generation, LaunchEvent::Progress(progress));
            }
        }
    });
}

fn send_event(events: &Sender<EventEnvelope>, generation: u64, event: LaunchEvent) {
    let _ = events.send(EventEnvelope { generation, event });
}

fn open_url(url: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", "", url]);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(url);
        command
    };
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_background_command(&mut command);
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn spawn_legacy_workspace(executable: &PathBuf) -> Result<(), String> {
    let mut command = Command::new(executable);
    command
        .arg("--legacy-desktop")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_background_command(&mut command);
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "windows")]
fn configure_background_command(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn configure_background_command(_command: &mut Command) {}

fn select_available_port(hosts: &[Ipv4Addr], preferred: u16, attempts: u16) -> io::Result<u16> {
    if hosts.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "at least one listener address is required",
        ));
    }
    if preferred != 0 {
        for offset in 0..attempts.max(1) {
            let Some(candidate) = preferred.checked_add(offset) else {
                break;
            };
            if port_is_available_on_all(hosts, candidate) {
                return Ok(candidate);
            }
        }
    }

    for _ in 0..attempts.max(1) {
        let listener = TcpListener::bind((hosts[0], 0))?;
        let candidate = listener.local_addr()?.port();
        let mut reservations = vec![listener];
        let mut available = true;
        for host in &hosts[1..] {
            match TcpListener::bind((*host, candidate)) {
                Ok(listener) => reservations.push(listener),
                Err(_) => {
                    available = false;
                    break;
                }
            }
        }
        if available {
            return Ok(candidate);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AddrNotAvailable,
        "no port is available on every selected listener address",
    ))
}

fn port_is_available_on_all(hosts: &[Ipv4Addr], port: u16) -> bool {
    let mut reservations = Vec::with_capacity(hosts.len());
    for host in hosts {
        match TcpListener::bind((*host, port)) {
            Ok(listener) => reservations.push(listener),
            Err(_) => return false,
        }
    }
    true
}

fn wait_for_health(
    address: SocketAddr,
    timeout: Duration,
    poll_interval: Duration,
    mut process_is_running: impl FnMut() -> bool,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if !process_is_running() {
            return Err("the local server stopped before it became ready".to_string());
        }
        if health_is_ready(address) {
            return Ok(());
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(format!(
                "the local server did not become ready within {} seconds",
                timeout.as_secs()
            ));
        }
        std::thread::sleep(poll_interval.min(deadline.saturating_duration_since(now)));
    }
}

fn health_is_ready(address: SocketAddr) -> bool {
    let connect_timeout = Duration::from_millis(250);
    let Ok(mut stream) = TcpStream::connect_timeout(&address, connect_timeout) else {
        return false;
    };
    let io_timeout = Some(Duration::from_millis(500));
    if stream.set_read_timeout(io_timeout).is_err() || stream.set_write_timeout(io_timeout).is_err()
    {
        return false;
    }
    let request = format!(
        "GET /api/health HTTP/1.1\r\nHost: {address}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }

    let mut response = Vec::with_capacity(1024);
    if stream.take(16 * 1024).read_to_end(&mut response).is_err() {
        return false;
    }
    let response = String::from_utf8_lossy(&response);
    let Some((headers, body)) = response.split_once("\r\n\r\n") else {
        return false;
    };
    if !headers
        .lines()
        .next()
        .is_some_and(|line| line.contains(" 200 "))
    {
        return false;
    }
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .is_some_and(|value| {
            value.get("status").and_then(|status| status.as_str()) == Some("ok")
                && value.get("name").and_then(|name| name.as_str()) == Some("Base Search")
        })
}

fn startup_progress_text(line: &str) -> Option<String> {
    let line = line.trim();
    let message = line.strip_prefix("[base-search]")?.trim();
    let normalized = message.to_ascii_lowercase();
    (normalized.contains("upgrade")
        || normalized.contains("migration")
        || normalized.contains("index"))
    .then(|| message.to_string())
}

fn build_server_command(
    executable: PathBuf,
    db_path: PathBuf,
    port: u16,
    bind_host: Ipv4Addr,
) -> Command {
    let mut command = Command::new(executable);
    command.args(["--browser", "--db"]);
    command.arg(db_path);
    command.args([
        "--host",
        &bind_host.to_string(),
        "--port",
        &port.to_string(),
        "--no-open",
    ]);
    command
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, TcpListener};
    use std::path::PathBuf;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn launcher_preferences_round_trip_and_replace_atomically() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("base_search.db.launcher.json");
        let defaults = LauncherPreferences::defaults(7833);

        let missing = load_launcher_preferences(&path, 7833);
        assert_eq!(missing.preferences, defaults);
        assert!(missing.warning.is_none());

        let lan = LauncherPreferences {
            version: LAUNCHER_PREFERENCES_VERSION,
            mode: WorkspaceMode::TrustedLan,
            preferred_port: 8123,
        };
        save_launcher_preferences(&path, &lan).unwrap();
        assert_eq!(load_launcher_preferences(&path, 7833).preferences, lan);
        let saved = fs::read_to_string(&path).unwrap();
        assert!(!saved.to_ascii_lowercase().contains("password"));

        let personal = LauncherPreferences {
            version: LAUNCHER_PREFERENCES_VERSION,
            mode: WorkspaceMode::Personal,
            preferred_port: 9000,
        };
        save_launcher_preferences(&path, &personal).unwrap();
        assert_eq!(load_launcher_preferences(&path, 7833).preferences, personal);
        assert_eq!(
            fs::read_dir(temp.path()).unwrap().count(),
            1,
            "an atomic replacement must not leave temporary files behind"
        );
    }

    #[test]
    fn launcher_preferences_live_next_to_the_workspace_database() {
        assert_eq!(
            launcher_preferences_path(Path::new("data/custom.db")),
            PathBuf::from("data/custom.db.launcher.json")
        );
    }

    #[test]
    fn invalid_launcher_preferences_fall_back_to_personal_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("launcher.json");
        fs::write(
            &path,
            r#"{"version":1,"mode":"trusted_lan","preferred_port":0}"#,
        )
        .unwrap();

        let loaded = load_launcher_preferences(&path, 7833);

        assert_eq!(loaded.preferences, LauncherPreferences::defaults(7833));
        assert!(
            loaded
                .warning
                .as_deref()
                .is_some_and(|warning| warning.contains("invalid"))
        );
    }

    #[test]
    fn launcher_preferences_reject_unknown_or_sensitive_fields() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("launcher.json");
        fs::write(
            &path,
            r#"{"version":1,"mode":"trusted_lan","preferred_port":8123,"password":"must-not-load"}"#,
        )
        .unwrap();

        let loaded = load_launcher_preferences(&path, 7833);

        assert_eq!(loaded.preferences, LauncherPreferences::defaults(7833));
        assert!(loaded.warning.is_some());
    }

    #[test]
    fn first_owner_bootstrap_validates_and_creates_exactly_one_owner() {
        let calls = RefCell::new(Vec::new());
        bootstrap_first_owner_with(
            Path::new("data/base_search.db"),
            "  workspace-owner  ",
            "strong-password",
            "strong-password",
            |_| Ok(Vec::new()),
            |path, username, password, role| {
                calls.borrow_mut().push((
                    path.to_path_buf(),
                    username.to_string(),
                    password.to_string(),
                    role.to_string(),
                ));
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(
            calls.into_inner(),
            vec![(
                PathBuf::from("data/base_search.db"),
                "workspace-owner".to_string(),
                "strong-password".to_string(),
                "owner".to_string(),
            )]
        );
    }

    #[test]
    fn first_owner_bootstrap_rejects_mismatch_or_existing_accounts() {
        let add_calls = RefCell::new(0_u32);
        let mismatch = bootstrap_first_owner_with(
            Path::new("data/base_search.db"),
            "owner",
            "strong-password",
            "different-password",
            |_| Ok(Vec::new()),
            |_, _, _, _| {
                *add_calls.borrow_mut() += 1;
                Ok(())
            },
        )
        .unwrap_err();
        assert!(mismatch.contains("match"));

        let existing = bootstrap_first_owner_with(
            Path::new("data/base_search.db"),
            "owner",
            "strong-password",
            "strong-password",
            |_| {
                Ok(vec![(
                    "existing".to_string(),
                    "owner".to_string(),
                    "now".to_string(),
                )])
            },
            |_, _, _, _| {
                *add_calls.borrow_mut() += 1;
                Ok(())
            },
        )
        .unwrap_err();
        assert!(existing.contains("already"));
        assert_eq!(*add_calls.borrow(), 0);
    }

    #[test]
    fn first_owner_bootstrap_uses_the_public_server_account_api() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("workspace").join("base_search.db");
        assert!(!db_path.parent().unwrap().exists());

        bootstrap_first_owner(
            &db_path,
            "workspace-owner",
            "strong-password",
            "strong-password",
        )
        .unwrap();

        assert!(db_path.parent().unwrap().exists());
        let accounts = base_search::server::list_accounts(&db_path).unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].0, "workspace-owner");
        assert_eq!(accounts[0].1, "owner");
    }

    #[test]
    fn port_selection_skips_an_occupied_preferred_port_in_both_modes() {
        let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let preferred = occupied.local_addr().unwrap().port();

        let selected = select_available_port(&[Ipv4Addr::LOCALHOST], preferred, 32).unwrap();

        assert_ne!(selected, preferred);
        let rebound = TcpListener::bind((Ipv4Addr::LOCALHOST, selected));
        assert!(rebound.is_ok(), "selected port must still be available");

        let selected_for_two_listeners =
            select_available_port(&[Ipv4Addr::LOCALHOST, Ipv4Addr::new(127, 0, 0, 2)], 0, 32)
                .unwrap();
        assert!(
            port_is_available_on_all(
                &[Ipv4Addr::LOCALHOST, Ipv4Addr::new(127, 0, 0, 2)],
                selected_for_two_listeners
            ),
            "the selected port must be free on both listeners"
        );
    }

    #[test]
    fn workspace_urls_always_open_locally_and_only_lan_mode_exposes_a_lan_url() {
        let address = Ipv4Addr::new(192, 168, 50, 23);

        let personal = workspace_urls(WorkspaceMode::Personal, 8123, Some(address));
        assert_eq!(personal.local, "http://127.0.0.1:8123");
        assert_eq!(personal.lan, None);

        let lan = workspace_urls(WorkspaceMode::TrustedLan, 8123, Some(address));
        assert_eq!(lan.local, "http://127.0.0.1:8123");
        assert_eq!(lan.lan.as_deref(), Some("http://192.168.50.23:8123"));
    }

    #[test]
    fn selected_lan_bind_rejects_missing_or_public_interfaces_and_accepts_cgnat() {
        let missing = selected_bind_host(WorkspaceMode::TrustedLan, None).unwrap_err();
        assert!(missing.contains("No usable private LAN or VPN"));

        assert!(
            selected_bind_host(WorkspaceMode::TrustedLan, Some(Ipv4Addr::new(8, 8, 8, 8))).is_err()
        );
        assert_eq!(
            selected_bind_host(
                WorkspaceMode::TrustedLan,
                Some(Ipv4Addr::new(100, 100, 42, 7))
            )
            .unwrap(),
            Ipv4Addr::new(100, 100, 42, 7)
        );
    }

    #[test]
    fn readiness_waits_for_a_successful_api_health_response() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(120));
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 512];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.starts_with("GET /api/health HTTP/1.1\r\n"));
            let body = r#"{"status":"ok","name":"Base Search","version":"2.0.0"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });

        let started = Instant::now();
        wait_for_health(
            address,
            Duration::from_secs(2),
            Duration::from_millis(20),
            || true,
        )
        .unwrap();

        assert!(started.elapsed() >= Duration::from_millis(100));
        server.join().unwrap();
    }

    #[test]
    fn readiness_rejects_an_unrelated_service_on_the_selected_port() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 512];
            let _ = stream.read(&mut request).unwrap();
            let body = r#"{"status":"ok","name":"Another service","version":"2.0.0"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });

        assert!(!health_is_ready(address));
        server.join().unwrap();
    }

    #[test]
    fn browser_open_is_emitted_only_after_the_current_server_is_ready() {
        let mut model = LauncherModel::new(PathBuf::from("data/base_search.db"), true);
        let generation = model.begin_start();

        assert_eq!(model.status(), LaunchStatus::Starting);
        assert_eq!(
            model.apply(
                generation,
                LaunchEvent::Prepared {
                    urls: WorkspaceUrls {
                        local: "http://127.0.0.1:7834".to_string(),
                        lan: None,
                    },
                },
            ),
            None
        );
        assert_eq!(
            model.apply(
                generation,
                LaunchEvent::Progress("Upgrading database: 40%".to_string()),
            ),
            None
        );
        assert_eq!(model.stage(), "Upgrading database: 40%");
        assert_eq!(
            model.apply(generation.wrapping_sub(1), LaunchEvent::Ready),
            None,
            "a late event from an old server must be ignored"
        );
        assert_eq!(
            model.apply(generation, LaunchEvent::Ready),
            Some(LaunchAction::OpenBrowser(
                "http://127.0.0.1:7834".to_string()
            ))
        );
        assert_eq!(model.status(), LaunchStatus::Ready);
        assert_eq!(model.apply(generation, LaunchEvent::Ready), None);
    }

    #[test]
    fn failed_and_stopped_launches_never_keep_a_dead_workspace_url() {
        let mut model = LauncherModel::new(PathBuf::from("data/base_search.db"), false);
        let generation = model.begin_start();
        model.apply(
            generation,
            LaunchEvent::Prepared {
                urls: WorkspaceUrls {
                    local: "http://127.0.0.1:7834".to_string(),
                    lan: Some("http://192.168.1.50:7834".to_string()),
                },
            },
        );
        assert_eq!(model.local_url(), Some("http://127.0.0.1:7834"));

        model.apply(
            generation,
            LaunchEvent::Failed("port stolen mid-start".to_string()),
        );
        assert_eq!(
            model.local_url(),
            None,
            "a failed launch has no working URL"
        );
        assert_eq!(model.lan_url(), None);

        let generation = model.begin_start();
        model.apply(
            generation,
            LaunchEvent::Prepared {
                urls: WorkspaceUrls {
                    local: "http://127.0.0.1:7835".to_string(),
                    lan: None,
                },
            },
        );
        model.apply(generation, LaunchEvent::Ready);
        model.stop();
        assert_eq!(model.local_url(), None, "a stopped workspace has no URL");
        assert_eq!(model.lan_url(), None);
    }

    #[test]
    fn lan_mode_state_exposes_both_urls_but_opens_loopback_and_locks_mode_while_running() {
        let mut model = LauncherModel::new(PathBuf::from("data/base_search.db"), true);
        model.set_mode(WorkspaceMode::TrustedLan).unwrap();
        let generation = model.begin_start();

        model.apply(
            generation,
            LaunchEvent::Prepared {
                urls: WorkspaceUrls {
                    local: "http://127.0.0.1:7833".to_string(),
                    lan: Some("http://192.168.1.50:7833".to_string()),
                },
            },
        );
        assert_eq!(model.local_url(), Some("http://127.0.0.1:7833"));
        assert_eq!(model.lan_url(), Some("http://192.168.1.50:7833"));
        assert_eq!(
            model.apply(generation, LaunchEvent::Ready),
            Some(LaunchAction::OpenBrowser(
                "http://127.0.0.1:7833".to_string()
            ))
        );
        assert!(model.set_mode(WorkspaceMode::Personal).is_err());

        model.stop();
        model.set_mode(WorkspaceMode::Personal).unwrap();
        assert_eq!(model.mode(), WorkspaceMode::Personal);
    }

    #[test]
    fn launcher_waits_for_mode_confirmation_before_starting() {
        let config = LauncherConfig {
            db_path: PathBuf::from("data/base_search.db"),
            preferred_port: 7833,
            open_browser: false,
        };

        let app = LauncherApp::new(config, PathBuf::from("missing-base-search-executable"));

        assert_eq!(app.controller.model.status(), LaunchStatus::Stopped);
    }

    #[test]
    fn launcher_loads_saved_mode_and_port_without_starting() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("base_search.db");
        let preferences_path = launcher_preferences_path(&db_path);
        save_launcher_preferences(
            &preferences_path,
            &LauncherPreferences {
                version: LAUNCHER_PREFERENCES_VERSION,
                mode: WorkspaceMode::TrustedLan,
                preferred_port: 8456,
            },
        )
        .unwrap();
        let config = LauncherConfig {
            db_path,
            preferred_port: 7833,
            open_browser: false,
        };

        let app = LauncherApp::new(config, PathBuf::from("missing-base-search-executable"));

        assert_eq!(app.controller.model.status(), LaunchStatus::Stopped);
        assert_eq!(app.controller.model.mode(), WorkspaceMode::TrustedLan);
        assert_eq!(app.controller.preferred_port, 8456);
    }

    #[test]
    fn launcher_mode_and_port_changes_are_persisted_together() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("base_search.db");
        let config = LauncherConfig {
            db_path: db_path.clone(),
            preferred_port: 7833,
            open_browser: false,
        };
        let mut controller =
            LauncherController::new(config, PathBuf::from("missing-base-search-executable"));

        controller
            .update_preferences(WorkspaceMode::TrustedLan, 8456)
            .unwrap();

        assert_eq!(controller.model.mode(), WorkspaceMode::TrustedLan);
        assert_eq!(controller.preferred_port, 8456);
        let loaded = load_launcher_preferences(&launcher_preferences_path(&db_path), 7833);
        assert_eq!(loaded.preferences.mode, WorkspaceMode::TrustedLan);
        assert_eq!(loaded.preferences.preferred_port, 8456);
    }

    #[test]
    fn saved_lan_mode_prompts_for_first_owner_without_retaining_credentials() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("base_search.db");
        save_launcher_preferences(
            &launcher_preferences_path(&db_path),
            &LauncherPreferences {
                version: LAUNCHER_PREFERENCES_VERSION,
                mode: WorkspaceMode::TrustedLan,
                preferred_port: 7833,
            },
        )
        .unwrap();
        let app = LauncherApp::new(
            LauncherConfig {
                db_path,
                preferred_port: 7833,
                open_browser: false,
            },
            PathBuf::from("missing-base-search-executable"),
        );

        assert_eq!(app.lan_account_state, LanAccountState::NeedsOwner);
        assert!(!app.lan_confirmed);
        assert!(app.owner_password.is_empty());
        assert!(app.owner_confirmation.is_empty());
    }

    #[test]
    fn successful_first_owner_creation_clears_password_fields_and_enables_lan() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("base_search.db");
        save_launcher_preferences(
            &launcher_preferences_path(&db_path),
            &LauncherPreferences {
                version: LAUNCHER_PREFERENCES_VERSION,
                mode: WorkspaceMode::TrustedLan,
                preferred_port: 7833,
            },
        )
        .unwrap();
        let mut app = LauncherApp::new(
            LauncherConfig {
                db_path,
                preferred_port: 7833,
                open_browser: false,
            },
            PathBuf::from("missing-base-search-executable"),
        );
        app.owner_username = "owner".to_string();
        app.owner_password = "strong-password".to_string();
        app.owner_confirmation = "strong-password".to_string();

        app.create_first_owner().unwrap();

        assert_eq!(app.lan_account_state, LanAccountState::Ready);
        assert!(app.owner_password.is_empty());
        assert!(app.owner_confirmation.is_empty());
        assert!(
            validate_start_requirements(
                WorkspaceMode::TrustedLan,
                true,
                app.lan_account_state == LanAccountState::Ready,
                Some(Ipv4Addr::new(192, 168, 1, 20)),
            )
            .is_ok()
        );
    }

    #[test]
    fn launcher_content_produces_visible_shapes_in_a_native_sized_viewport() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = LauncherApp::new(
            LauncherConfig {
                db_path: temp.path().join("base_search.db"),
                preferred_port: 7833,
                open_browser: false,
            },
            PathBuf::from("missing-base-search-executable"),
        );
        let context = egui::Context::default();
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(680.0, 620.0),
                )),
                ..Default::default()
            },
            |ui| {
                app.render_root(ui);
            },
        );

        assert!(
            output.shapes.len() > 10,
            "the launcher must paint controls instead of an empty window"
        );
    }

    #[test]
    fn trusted_lan_requires_an_account_and_explicit_confirmation() {
        assert!(validate_start_requirements(WorkspaceMode::Personal, false, false, None).is_ok());

        let address = Some(Ipv4Addr::new(192, 168, 1, 20));
        let no_account =
            validate_start_requirements(WorkspaceMode::TrustedLan, true, false, address)
                .unwrap_err();
        assert!(no_account.contains("owner"));

        let no_confirmation =
            validate_start_requirements(WorkspaceMode::TrustedLan, false, true, address)
                .unwrap_err();
        assert!(no_confirmation.contains("confirm"));

        let no_interface =
            validate_start_requirements(WorkspaceMode::TrustedLan, true, true, None).unwrap_err();
        assert!(no_interface.contains("No usable private LAN or VPN"));

        assert!(
            validate_start_requirements(WorkspaceMode::TrustedLan, true, true, address).is_ok()
        );
    }

    #[test]
    fn stopping_invalidates_late_readiness_events() {
        let mut model = LauncherModel::new(PathBuf::from("data/base_search.db"), true);
        let generation = model.begin_start();

        model.stop();

        assert_eq!(model.apply(generation, LaunchEvent::Ready), None);
        assert_eq!(model.status(), LaunchStatus::Stopped);
    }

    #[test]
    fn migration_output_is_converted_into_visible_startup_progress() {
        assert_eq!(
            startup_progress_text(
                "[base-search] Database upgrade: 40% (800000 of 2000000 rows, 15s elapsed)"
            ),
            Some("Database upgrade: 40% (800000 of 2000000 rows, 15s elapsed)".to_string())
        );
        assert_eq!(
            startup_progress_text(
                "[base-search] One-time database upgrade: computing typed columns for 2000000 rows."
            ),
            Some(
                "One-time database upgrade: computing typed columns for 2000000 rows.".to_string()
            )
        );
        assert_eq!(startup_progress_text("ordinary diagnostic output"), None);
    }

    #[test]
    fn managed_process_can_be_stopped_and_replaced_cleanly() {
        let process = ProcessSlot::default();
        process.activate(1).unwrap();
        assert!(process.replace_for_generation(1, spawn_sleeper()).unwrap());
        assert!(process.is_running_for_generation(1));

        process.stop_for_generation(1).unwrap();
        assert!(!process.is_running_for_generation(1));

        assert!(process.replace_for_generation(1, spawn_sleeper()).unwrap());
        assert!(process.is_running_for_generation(1));
        assert!(process.replace_for_generation(1, spawn_sleeper()).unwrap());
        assert!(process.is_running_for_generation(1));
        process.stop_for_generation(1).unwrap();
        assert!(!process.is_running_for_generation(1));
    }

    #[test]
    fn stale_start_cannot_install_a_process_after_stop_or_restart() {
        let process = ProcessSlot::default();
        process.activate(1).unwrap();
        process.activate(2).unwrap();

        assert!(!process.replace_for_generation(1, spawn_sleeper()).unwrap());
        assert!(!process.is_running_for_generation(1));
        assert!(!process.is_running_for_generation(2));

        assert!(process.replace_for_generation(2, spawn_sleeper()).unwrap());
        assert!(process.is_running_for_generation(2));
        process.activate(3).unwrap();
        assert!(!process.is_running_for_generation(2));
        assert!(!process.is_running_for_generation(3));
    }

    #[test]
    fn child_server_commands_use_loopback_or_the_selected_lan_interface() {
        let personal = build_server_command(
            PathBuf::from("BaseSearch"),
            PathBuf::from("data/base_search.db"),
            8123,
            Ipv4Addr::LOCALHOST,
        );
        let personal_args = personal
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            personal_args,
            vec![
                "--browser",
                "--db",
                "data/base_search.db",
                "--host",
                "127.0.0.1",
                "--port",
                "8123",
                "--no-open",
            ]
        );

        let lan = build_server_command(
            PathBuf::from("BaseSearch"),
            PathBuf::from("data/base_search.db"),
            8123,
            Ipv4Addr::new(100, 100, 42, 7),
        );
        let lan_args = lan
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(lan_args[3..5], ["--host", "100.100.42.7"]);
        assert_eq!(lan_args.last().map(String::as_str), Some("--no-open"));
    }

    fn spawn_sleeper() -> Child {
        #[cfg(target_os = "windows")]
        let mut command = {
            let mut command = Command::new("powershell");
            command.args(["-NoProfile", "-Command", "Start-Sleep -Seconds 30"]);
            command
        };
        #[cfg(not(target_os = "windows"))]
        let mut command = {
            let mut command = Command::new("sh");
            command.args(["-c", "sleep 30"]);
            command
        };
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }
}
