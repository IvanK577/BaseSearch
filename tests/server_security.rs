#![cfg(feature = "browser")]

use std::net::{IpAddr, Ipv4Addr};

use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use base_search::server::security::{
    CONTENT_SECURITY_POLICY, PERMISSIONS_POLICY, TransportSecurity, apply_security_headers,
};

const PORT: u16 = 7833;

fn policy() -> TransportSecurity {
    TransportSecurity::new(IpAddr::V4(Ipv4Addr::LOCALHOST), PORT)
}

fn headers(host: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_str(host).unwrap());
    headers
}

#[test]
fn rejects_unapproved_host_as_misdirected_request() {
    let headers = headers("attacker.example:7833");

    let error = policy()
        .validate(&Method::GET, &headers)
        .expect_err("a foreign hostname must not reach the application");

    assert_eq!(error.status(), StatusCode::MISDIRECTED_REQUEST);
    assert_eq!(error.code(), "unapproved_host");
}

#[test]
fn accepts_supported_loopback_hosts_on_the_configured_port() {
    for host in ["127.0.0.1:7833", "localhost:7833", "[::1]:7833"] {
        policy()
            .validate(&Method::GET, &headers(host))
            .unwrap_or_else(|error| panic!("{host} should be accepted: {error:?}"));
    }
}

#[test]
fn wildcard_policy_accepts_tailscale_style_cgnat_hosts_but_not_public_hosts() {
    let policy = TransportSecurity::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), PORT);

    policy
        .validate(&Method::GET, &headers("100.100.42.7:7833"))
        .expect("a CGNAT VPN address approved by the launcher must also pass Host validation");

    let error = policy
        .validate(&Method::GET, &headers("8.8.8.8:7833"))
        .expect_err("a public Host value must not be approved for a wildcard listener");
    assert_eq!(error.code(), "unapproved_host");
}

#[test]
fn selected_lan_policy_accepts_only_that_interface_and_loopback_authorities() {
    let policy = TransportSecurity::new(IpAddr::V4(Ipv4Addr::new(100, 100, 42, 7)), PORT);

    for host in [
        "100.100.42.7:7833",
        "127.0.0.1:7833",
        "localhost:7833",
        "[::1]:7833",
    ] {
        policy
            .validate(&Method::GET, &headers(host))
            .unwrap_or_else(|error| panic!("{host} should be accepted: {error:?}"));
    }

    for hostile in [
        "100.100.42.8:7833",
        "192.168.1.20:7833",
        "8.8.8.8:7833",
        "0.0.0.0:7833",
        "localhost.attacker.example:7833",
        "100.100.42.7.attacker.example:7833",
        "100.100.42.7:7834",
    ] {
        let error = policy
            .validate(&Method::GET, &headers(hostile))
            .unwrap_err();
        assert_eq!(error.code(), "unapproved_host", "{hostile}");
    }
}

#[test]
fn rejects_foreign_origin_for_mutating_requests() {
    let mut headers = headers("127.0.0.1:7833");
    headers.insert(
        header::ORIGIN,
        HeaderValue::from_static("http://attacker.example:7833"),
    );

    let error = policy()
        .validate(&Method::POST, &headers)
        .expect_err("a foreign browser origin must be rejected");

    assert_eq!(error.status(), StatusCode::FORBIDDEN);
    assert_eq!(error.code(), "foreign_origin");
    assert_eq!(
        error.message(),
        "Origin does not match this Base Search workspace."
    );
}

#[test]
fn rejects_cross_site_fetch_metadata_for_mutating_requests() {
    let mut headers = headers("127.0.0.1:7833");
    headers.insert("sec-fetch-site", HeaderValue::from_static("cross-site"));

    let error = policy()
        .validate(&Method::DELETE, &headers)
        .expect_err("cross-site browser requests must be rejected");

    assert_eq!(error.status(), StatusCode::FORBIDDEN);
    assert_eq!(error.code(), "cross_site_request");
    assert_eq!(
        error.message(),
        "Cross-site browser requests are not allowed."
    );
}

#[test]
fn accepts_same_origin_and_originless_local_cli_mutations() {
    let mut browser_headers = headers("localhost:7833");
    browser_headers.insert(
        header::ORIGIN,
        HeaderValue::from_static("http://localhost:7833"),
    );
    browser_headers.insert("sec-fetch-site", HeaderValue::from_static("same-origin"));

    policy()
        .validate(&Method::PATCH, &browser_headers)
        .expect("same-origin browser request should pass");
    policy()
        .validate(&Method::PUT, &headers("127.0.0.1:7833"))
        .expect("a trusted local CLI request without Origin should pass");
}

#[test]
fn applies_security_headers_to_every_response() {
    let mut headers = HeaderMap::new();

    apply_security_headers(&mut headers);

    assert_eq!(
        headers[header::CONTENT_SECURITY_POLICY],
        CONTENT_SECURITY_POLICY
    );
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");
    assert_eq!(headers["permissions-policy"], PERMISSIONS_POLICY);
    assert!(
        headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'none'")
    );
}
