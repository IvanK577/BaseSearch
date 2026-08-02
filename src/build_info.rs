//! Which optional features this binary was actually compiled with.
//!
//! A plain `cargo build --release` binary and one built with the documented
//! `release-package` feature set look identical on disk, yet they answer
//! analytics questions through different engines. Reporting the set makes
//! "which build is this?" answerable from the shipped artifact instead of by
//! inspecting strings in the executable.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Optional features compiled into this binary, in a stable order.
pub fn features() -> Vec<&'static str> {
    let mut features = Vec::new();
    if cfg!(feature = "browser") {
        features.push("browser");
    }
    if cfg!(feature = "duckdb-olap") {
        features.push("duckdb-olap");
    }
    features
}

/// True when this binary carries the exact feature set every production
/// package is required to ship, i.e. the `release-package` feature in
/// `Cargo.toml`. The packaging smoke tests assert this so a binary built the
/// wrong way cannot be shipped unnoticed.
///
/// `duckdb-olap` is not part of that set — see the comment on `release-package`
/// in `Cargo.toml` — so it is reported in [`features`] but not required here.
pub fn is_release_package_build() -> bool {
    cfg!(feature = "browser")
}

/// One-line summary used by `base-search-cli version` and the smoke tests.
pub fn summary() -> String {
    let features = features();
    let rendered = if features.is_empty() {
        "none".to_string()
    } else {
        features.join(",")
    };
    format!(
        "Base Search {VERSION} (features: {rendered}; release-package: {})",
        if is_release_package_build() {
            "yes"
        } else {
            "no"
        }
    )
}

#[cfg(test)]
mod tests {
    use super::{features, is_release_package_build, summary};

    #[test]
    fn summary_reports_the_features_it_was_built_with() {
        let summary = summary();
        assert!(summary.starts_with("Base Search "));
        for feature in features() {
            assert!(
                summary.contains(feature),
                "summary must name every enabled feature: {summary}"
            );
        }
        assert_eq!(
            summary.contains("release-package: yes"),
            is_release_package_build(),
            "the release-package verdict must match the compiled feature set"
        );
    }

    #[test]
    fn release_package_requires_the_browser_workspace() {
        assert_eq!(is_release_package_build(), features().contains(&"browser"));
    }
}
