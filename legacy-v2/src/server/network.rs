//! Shared IPv4 classification and interface discovery for browser workspaces.
//!
//! The launcher, bind policy, URL generation, and Host validation must use the
//! same rules. Otherwise an address can be advertised to users but rejected by
//! the server, or a listener can become reachable on an unintended interface.

use std::collections::HashSet;
use std::io;
use std::net::Ipv4Addr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ipv4NetworkClass {
    Loopback,
    Rfc1918,
    CarrierGradeNat,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustedIpv4Interface {
    pub name: String,
    pub address: Ipv4Addr,
    pub class: Ipv4NetworkClass,
}

pub const fn classify_ipv4(address: Ipv4Addr) -> Ipv4NetworkClass {
    let [first, second, _, _] = address.octets();
    if first == 127 {
        Ipv4NetworkClass::Loopback
    } else if first == 10
        || (first == 172 && second >= 16 && second <= 31)
        || (first == 192 && second == 168)
    {
        Ipv4NetworkClass::Rfc1918
    } else if first == 100 && second >= 64 && second <= 127 {
        Ipv4NetworkClass::CarrierGradeNat
    } else {
        Ipv4NetworkClass::Other
    }
}

pub const fn is_loopback_ipv4(address: Ipv4Addr) -> bool {
    matches!(classify_ipv4(address), Ipv4NetworkClass::Loopback)
}

pub const fn is_trusted_lan_ipv4(address: Ipv4Addr) -> bool {
    matches!(
        classify_ipv4(address),
        Ipv4NetworkClass::Rfc1918 | Ipv4NetworkClass::CarrierGradeNat
    )
}

pub fn local_workspace_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

pub fn trusted_lan_workspace_url(address: Ipv4Addr, port: u16) -> Option<String> {
    is_trusted_lan_ipv4(address).then(|| format!("http://{address}:{port}"))
}

pub fn discover_trusted_ipv4_interfaces() -> io::Result<Vec<TrustedIpv4Interface>> {
    let candidates = if_addrs::get_if_addrs()?
        .into_iter()
        .filter(|interface| interface.is_oper_up())
        .filter_map(|interface| match interface.ip() {
            std::net::IpAddr::V4(address) => Some((interface.name, address)),
            std::net::IpAddr::V6(_) => None,
        });
    Ok(normalize_trusted_ipv4_interfaces(candidates))
}

fn normalize_trusted_ipv4_interfaces(
    candidates: impl IntoIterator<Item = (String, Ipv4Addr)>,
) -> Vec<TrustedIpv4Interface> {
    let mut seen = HashSet::new();
    let mut interfaces: Vec<_> = candidates
        .into_iter()
        .filter_map(|(name, address)| {
            let class = classify_ipv4(address);
            if !matches!(
                class,
                Ipv4NetworkClass::Rfc1918 | Ipv4NetworkClass::CarrierGradeNat
            ) || !seen.insert(address)
            {
                return None;
            }
            Some(TrustedIpv4Interface {
                name,
                address,
                class,
            })
        })
        .collect();
    interfaces.sort_by(|left, right| {
        class_priority(left.class)
            .cmp(&class_priority(right.class))
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.address.octets().cmp(&right.address.octets()))
    });
    interfaces
}

const fn class_priority(class: Ipv4NetworkClass) -> u8 {
    match class {
        Ipv4NetworkClass::Rfc1918 => 0,
        Ipv4NetworkClass::CarrierGradeNat => 1,
        Ipv4NetworkClass::Loopback => 2,
        Ipv4NetworkClass::Other => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifier_covers_loopback_rfc1918_cgnat_and_public_ipv4() {
        assert_eq!(
            classify_ipv4(Ipv4Addr::new(127, 20, 30, 40)),
            Ipv4NetworkClass::Loopback
        );
        for address in [
            Ipv4Addr::new(10, 20, 30, 40),
            Ipv4Addr::new(172, 16, 0, 1),
            Ipv4Addr::new(172, 31, 255, 254),
            Ipv4Addr::new(192, 168, 50, 2),
        ] {
            assert_eq!(classify_ipv4(address), Ipv4NetworkClass::Rfc1918);
            assert!(is_trusted_lan_ipv4(address));
        }
        let tailscale = Ipv4Addr::new(100, 100, 42, 7);
        assert_eq!(classify_ipv4(tailscale), Ipv4NetworkClass::CarrierGradeNat);
        assert!(is_trusted_lan_ipv4(tailscale));
        for address in [
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::new(172, 32, 0, 1),
            Ipv4Addr::new(8, 8, 8, 8),
            Ipv4Addr::new(169, 254, 1, 1),
        ] {
            assert_eq!(classify_ipv4(address), Ipv4NetworkClass::Other);
            assert!(!is_trusted_lan_ipv4(address));
        }
    }

    #[test]
    fn interface_normalization_rejects_unusable_addresses_and_is_deterministic() {
        let interfaces = normalize_trusted_ipv4_interfaces([
            ("public".to_string(), Ipv4Addr::new(8, 8, 8, 8)),
            ("vpn".to_string(), Ipv4Addr::new(100, 100, 42, 7)),
            ("wifi".to_string(), Ipv4Addr::new(192, 168, 1, 20)),
            ("duplicate".to_string(), Ipv4Addr::new(192, 168, 1, 20)),
            ("loopback".to_string(), Ipv4Addr::LOCALHOST),
        ]);

        assert_eq!(interfaces.len(), 2);
        assert_eq!(interfaces[0].name, "wifi");
        assert_eq!(interfaces[0].address, Ipv4Addr::new(192, 168, 1, 20));
        assert_eq!(interfaces[1].name, "vpn");
        assert_eq!(interfaces[1].address, Ipv4Addr::new(100, 100, 42, 7));
    }

    #[test]
    fn no_usable_interface_returns_an_empty_selection() {
        let interfaces = normalize_trusted_ipv4_interfaces([
            ("loopback".to_string(), Ipv4Addr::LOCALHOST),
            ("link-local".to_string(), Ipv4Addr::new(169, 254, 3, 4)),
            ("public".to_string(), Ipv4Addr::new(203, 0, 113, 10)),
        ]);
        assert!(interfaces.is_empty());
    }
}
