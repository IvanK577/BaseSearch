//! Runtime configuration for the local browser workspace server.

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::time::Duration;

use super::network::{
    is_loopback_ipv4, is_trusted_lan_ipv4, local_workspace_url, trusted_lan_workspace_url,
};

/// Default loopback port. Chosen high and uncommon to avoid clashes with other
/// local dev servers.
pub const DEFAULT_PORT: u16 = 7833;
/// Heavy SQLite/DuckDB reads admitted at once. Keeping this low protects RAM,
/// the blocking pool, and interactive endpoints on ordinary laptops.
pub const DEFAULT_MAX_HEAVY_READS: usize = 2;
/// Heavy reads allowed to wait briefly before a deterministic overload reply.
pub const DEFAULT_MAX_QUEUED_HEAVY_READS: usize = 8;
pub const DEFAULT_HEAVY_READ_QUEUE_WAIT: Duration = Duration::from_millis(250);

#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// SQLite database the workspace operates on.
    pub db_path: PathBuf,
    /// Address to bind. Defaults to loopback; anything else is an explicit
    /// opt-in to exposing the workspace on the local network.
    pub host: IpAddr,
    pub port: u16,
    /// Open the default browser once the server is listening.
    pub open_browser: bool,
    /// Maximum search/analytics/card requests executing concurrently.
    pub max_concurrent_heavy_reads: usize,
    /// Maximum additional heavy requests waiting for an execution slot.
    pub max_queued_heavy_reads: usize,
    /// Maximum time a heavy request may wait before receiving `503`.
    pub heavy_read_queue_wait: Duration,
    /// Wildcard listeners are intentionally unavailable to normal callers.
    /// Direct CLI mode must set this only after its separate confirmation flag
    /// has been supplied.
    wildcard_bind_confirmed: bool,
}

impl ServerConfig {
    /// Local, single-user configuration bound to `127.0.0.1`.
    pub fn local(db_path: PathBuf) -> Self {
        Self {
            db_path,
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: DEFAULT_PORT,
            open_browser: true,
            max_concurrent_heavy_reads: DEFAULT_MAX_HEAVY_READS,
            max_queued_heavy_reads: DEFAULT_MAX_QUEUED_HEAVY_READS,
            heavy_read_queue_wait: DEFAULT_HEAVY_READ_QUEUE_WAIT,
            wildcard_bind_confirmed: false,
        }
    }

    /// Network workspace configuration for one explicitly selected private or
    /// CGNAT/VPN IPv4 interface. The server also opens a loopback companion
    /// listener on the same port so the launcher can keep using localhost.
    pub fn trusted_lan(db_path: PathBuf, address: Ipv4Addr) -> Result<Self, String> {
        if !is_trusted_lan_ipv4(address) {
            return Err(format!(
                "{address} is not an RFC1918 or CGNAT private/VPN IPv4 address"
            ));
        }
        let mut config = Self::local(db_path);
        config.host = IpAddr::V4(address);
        Ok(config)
    }

    /// Confirms the intentionally broad `0.0.0.0` advanced CLI bind. The
    /// launcher never calls this method.
    pub fn confirm_wildcard_bind(&mut self) {
        self.wildcard_bind_confirmed = true;
    }

    pub fn validate_bind_policy(&self) -> Result<(), String> {
        match self.host {
            IpAddr::V4(address) if is_loopback_ipv4(address) => Ok(()),
            IpAddr::V4(address) if is_trusted_lan_ipv4(address) => Ok(()),
            IpAddr::V4(address)
                if address.is_unspecified() && self.wildcard_bind_confirmed =>
            {
                Ok(())
            }
            IpAddr::V4(address) if address.is_unspecified() => Err(
                "binding 0.0.0.0 requires the explicit advanced CLI confirmation flag"
                    .to_string(),
            ),
            _ => Err(
                "the browser workspace may bind only to loopback, an RFC1918 address, or a CGNAT VPN address"
                    .to_string(),
            ),
        }
    }

    /// True when the server is bound to a loopback address only.
    pub fn is_loopback(&self) -> bool {
        self.host.is_loopback()
    }

    pub fn is_wildcard(&self) -> bool {
        self.host.is_unspecified()
    }

    /// Listener addresses required for the configured workspace. A selected
    /// LAN/VPN address gets a separate local-only companion listener.
    pub fn listener_hosts(&self) -> Vec<IpAddr> {
        match self.host {
            IpAddr::V4(address) if is_trusted_lan_ipv4(address) => {
                vec![IpAddr::V4(address), IpAddr::V4(Ipv4Addr::LOCALHOST)]
            }
            host => vec![host],
        }
    }

    /// The URL a browser should open to reach the workspace.
    pub fn workspace_url(&self) -> String {
        local_workspace_url(self.port)
    }

    pub fn lan_url(&self) -> Option<String> {
        match self.host {
            IpAddr::V4(address) => trusted_lan_workspace_url(address, self.port),
            IpAddr::V6(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> PathBuf {
        PathBuf::from("data/test.db")
    }

    #[test]
    fn trusted_lan_uses_the_selected_interface_and_a_loopback_companion() {
        let address = Ipv4Addr::new(100, 100, 42, 7);
        let config = ServerConfig::trusted_lan(database(), address).unwrap();

        assert_eq!(config.host, IpAddr::V4(address));
        assert_eq!(
            config.listener_hosts(),
            vec![IpAddr::V4(address), IpAddr::V4(Ipv4Addr::LOCALHOST)]
        );
        assert_eq!(config.workspace_url(), "http://127.0.0.1:7833");
        assert_eq!(
            config.lan_url().as_deref(),
            Some("http://100.100.42.7:7833")
        );
        config.validate_bind_policy().unwrap();
    }

    #[test]
    fn wildcard_bind_requires_separate_explicit_confirmation() {
        let mut config = ServerConfig::local(database());
        config.host = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
        assert!(config.validate_bind_policy().is_err());

        config.confirm_wildcard_bind();
        config.validate_bind_policy().unwrap();
        assert_eq!(config.listener_hosts(), vec![config.host]);
    }

    #[test]
    fn public_and_link_local_interface_binds_are_rejected() {
        for address in [Ipv4Addr::new(8, 8, 8, 8), Ipv4Addr::new(169, 254, 1, 2)] {
            let mut config = ServerConfig::local(database());
            config.host = IpAddr::V4(address);
            assert!(config.validate_bind_policy().is_err(), "{address}");
            assert!(ServerConfig::trusted_lan(database(), address).is_err());
        }
    }
}
