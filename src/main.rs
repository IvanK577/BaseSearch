#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(feature = "browser")]
mod launcher;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--help" | "-h" | "help") => {
            eprintln!(
                "Base Search starts a local server launcher and opens the browser only after it is ready.\n\
                 Use `BaseSearch --legacy-desktop` for the fallback desktop workspace.\n\
                 Use `BaseSearch --browser [--db PATH] [--host H] [--port P] [--no-open]` for direct server mode.\n\
                 Binding 0.0.0.0 additionally requires `--confirm-wildcard-bind`.\n\
                 Use `base-search-cli` for database maintenance commands."
            );
        }
        Some("--browser") => run_browser(&args[1..]),
        Some("--legacy-desktop") => run_legacy_desktop(),
        Some(other) => fail_main(&format!("Unknown option: {other}")),
        None => run_default(),
    }
}

#[cfg(feature = "browser")]
fn run_default() {
    let config = launcher::LauncherConfig::local(base_search::app::default_db_path());
    if let Err(error) = launcher::run(config) {
        fail_main(&format!("Base Search launcher error: {error}"));
    }
}

#[cfg(not(feature = "browser"))]
fn run_default() {
    run_legacy_desktop();
}

fn run_legacy_desktop() {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Base Search")
            .with_inner_size([1360.0, 850.0])
            .with_min_inner_size([960.0, 600.0]),
        ..Default::default()
    };
    if let Err(err) = eframe::run_native(
        "Base Search",
        options,
        Box::new(|cc| Ok(Box::new(base_search::app::App::new(cc)))),
    ) {
        eprintln!("Base Search error: {err}");
        std::process::exit(1);
    }
}

/// Launches the local browser workspace against the desktop's default database.
#[cfg(feature = "browser")]
fn run_browser(args: &[String]) {
    use base_search::server;

    let config = parse_browser_config(args, base_search::app::default_db_path())
        .unwrap_or_else(|message| fail(&message));

    if let Err(err) = server::run(config) {
        eprintln!("Base Search browser error: {err}");
        std::process::exit(1);
    }
}

#[cfg(feature = "browser")]
fn parse_browser_config(
    args: &[String],
    default_db_path: std::path::PathBuf,
) -> Result<base_search::server::ServerConfig, String> {
    let mut config = base_search::server::ServerConfig::local(default_db_path);
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--host" => {
                index += 1;
                let host = args
                    .get(index)
                    .ok_or_else(|| "--host requires a valid IP address".to_string())?
                    .parse()
                    .map_err(|_| "--host requires a valid IP address".to_string())?;
                config.host = host;
            }
            "--port" => {
                index += 1;
                config.port = args
                    .get(index)
                    .ok_or_else(|| "--port must be a number between 1 and 65535".to_string())?
                    .parse::<u16>()
                    .ok()
                    .filter(|port| *port != 0)
                    .ok_or_else(|| "--port must be a number between 1 and 65535".to_string())?;
            }
            "--db" => {
                index += 1;
                let value = args
                    .get(index)
                    .filter(|value| !value.starts_with("--"))
                    .ok_or_else(|| "--db requires a database path".to_string())?;
                config.db_path = value.into();
            }
            "--no-open" => config.open_browser = false,
            "--confirm-wildcard-bind" => config.confirm_wildcard_bind(),
            other => return Err(format!("Unknown option: {other}")),
        }
        index += 1;
    }
    config.validate_bind_policy()?;
    Ok(config)
}

#[cfg(not(feature = "browser"))]
fn run_browser(_args: &[String]) {
    eprintln!("This build was compiled without the browser feature.");
    std::process::exit(1);
}

#[cfg(feature = "browser")]
fn fail(message: &str) -> ! {
    fail_main(message)
}

fn fail_main(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(2)
}

#[cfg(all(test, feature = "browser"))]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn direct_browser_mode_accepts_the_launcher_database_path() {
        let args = vec![
            "--db".to_string(),
            "data/custom.db".to_string(),
            "--host".to_string(),
            "127.0.0.1".to_string(),
            "--port".to_string(),
            "8123".to_string(),
            "--no-open".to_string(),
        ];

        let config = parse_browser_config(&args, PathBuf::from("data/default.db")).unwrap();

        assert_eq!(config.db_path, PathBuf::from("data/custom.db"));
        assert_eq!(config.port, 8123);
        assert!(!config.open_browser);
    }

    #[test]
    fn direct_browser_mode_accepts_private_interfaces_and_requires_wildcard_confirmation() {
        let selected = vec![
            "--host".to_string(),
            "100.100.42.7".to_string(),
            "--no-open".to_string(),
        ];
        let selected_config =
            parse_browser_config(&selected, PathBuf::from("data/default.db")).unwrap();
        assert_eq!(selected_config.host.to_string(), "100.100.42.7");

        let lan = vec![
            "--host".to_string(),
            "0.0.0.0".to_string(),
            "--confirm-wildcard-bind".to_string(),
            "--no-open".to_string(),
        ];
        let lan_config = parse_browser_config(&lan, PathBuf::from("data/default.db")).unwrap();
        assert_eq!(lan_config.host.to_string(), "0.0.0.0");

        let unconfirmed = vec!["--host".to_string(), "0.0.0.0".to_string()];
        let error =
            parse_browser_config(&unconfirmed, PathBuf::from("data/default.db")).unwrap_err();
        assert!(error.contains("confirmation"));

        for host in ["8.8.8.8", "169.254.1.2", "::"] {
            let args = vec!["--host".to_string(), host.to_string()];
            let error = parse_browser_config(&args, PathBuf::from("data/default.db")).unwrap_err();
            assert!(error.contains("loopback") || error.contains("RFC1918"));
        }
    }
}
