//! The only address this crate is ever allowed to connect to.
//!
//! # Why a type and not a `SocketAddr` plus a validation call
//!
//! Plan 030 section 3 requires that a non-loopback address be *unrepresentable*, not
//! merely rejected. A validated `SocketAddr` returned by a function that a caller might
//! not call, or might call on a different value, is a boundary that exists only in review.
//!
//! Making it a distinct type means the authority is carried by the signature: a
//! `SamBridgeEndpoint` can only be constructed by this module, and every connect path
//! takes one. A future edit that wants to reach a non-loopback router cannot express the
//! address, so it cannot be a one-line change someone makes without noticing.

use std::{
    fmt,
    net::{IpAddr, Ipv4Addr, SocketAddr},
};

/// Default SAM bridge port. 7656 is the SAM bridge's registered IANA-suggested port and
/// what both Java I2P and i2pd bind by default.
pub const DEFAULT_SAM_BRIDGE_PORT: u16 = 7656;

/// The default bridge endpoint: the IPv4 loopback address and the standard SAM port.
///
/// Chosen over `[::1]` deliberately. A router that binds only one loopback family is
/// common enough that defaulting to IPv4 reaches more routers without configuration, and
/// an operator who needs IPv6 can configure it explicitly.
pub const DEFAULT_SAM_BRIDGE: SamBridgeEndpoint = SamBridgeEndpoint(SocketAddr::new(
    IpAddr::V4(Ipv4Addr::LOCALHOST),
    DEFAULT_SAM_BRIDGE_PORT,
));

/// A loopback SAM bridge address.
///
/// Constructible only through [`SamBridgeEndpoint::parse`] or
/// [`SamBridgeEndpoint::DEFAULT`], and only from a numeric literal — there is no
/// resolver call anywhere in this crate, so `"localhost"` cannot be resolved into a name
/// that happens to point somewhere.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SamBridgeEndpoint(SocketAddr);

impl SamBridgeEndpoint {
    /// The default loopback endpoint, `127.0.0.1:7656`.
    pub const DEFAULT: Self = DEFAULT_SAM_BRIDGE;

    /// Accepts an address only when it is a loopback literal with a usable port.
    ///
    /// Rejects, with the reason rather than a bare `false`:
    ///
    /// - a non-loopback address, which is the whole boundary;
    /// - `port 0`, which is a request for the OS to pick a port and therefore not an
    ///   address anyone can reach;
    /// - a host name of any form, because resolving one is exactly the authority this
    ///   crate must not have. `"localhost"` is not special-cased into acceptance: it is a
    ///   name, and accepting names is what would let a resolver decide where to connect.
    pub fn parse(value: &str) -> Result<Self, BridgeEndpointError> {
        if value.is_empty() {
            return Err(BridgeEndpointError::Empty);
        }
        // A URL, or anything with a scheme, path, or userinfo.
        if value.contains("://") || value.contains('/') {
            return Err(BridgeEndpointError::NotNumeric);
        }
        // Split on the *last* colon so a bracketed IPv6 literal works.
        let Some((host, port)) = value.rsplit_once(':') else {
            return Err(BridgeEndpointError::MissingPort);
        };
        // Strip the IPv6 brackets. A bare `::1` would otherwise arrive with a leading
        // colon and no host part.
        let host = host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(host);
        if host.is_empty() {
            return Err(BridgeEndpointError::MissingHost);
        }
        // Reject anything that is not a numeric address literal, before parsing as one.
        // `parse` below would reject most of these anyway; being explicit here is what
        // makes "no resolver call" a property of this function rather than a property of
        // the standard library's error message.
        // Hex digits are allowed because an IPv6 literal uses them; nothing else is.
        // Rejecting them here would report a routable IPv6 address as "not numeric",
        // which sends an operator looking for a typo that is not there.
        if !host
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'.' || byte == b':')
        {
            return Err(BridgeEndpointError::NotNumeric);
        }
        let address: IpAddr = host.parse().map_err(|_| BridgeEndpointError::NotNumeric)?;
        if !address.is_loopback() {
            return Err(BridgeEndpointError::NotLoopback);
        }
        let port: u16 = port.parse().map_err(|_| BridgeEndpointError::InvalidPort)?;
        if port == 0 {
            return Err(BridgeEndpointError::InvalidPort);
        }
        Ok(Self(SocketAddr::new(address, port)))
    }

    /// The loopback address and port, for handing to a socket connect.
    pub fn socket_addr(self) -> SocketAddr {
        self.0
    }

    /// The port alone.
    pub fn port(self) -> u16 {
        self.0.port()
    }

    /// The IP alone.
    pub fn ip(self) -> IpAddr {
        self.0.ip()
    }

    /// Whether this endpoint is IPv6.
    ///
    /// Needed because a connect to an IPv6 loopback address must be performed on a
    /// dual-stack-capable socket, and choosing that socket is a decision a caller would
    /// otherwise have to make from a family check of its own.
    pub fn is_ipv6(self) -> bool {
        matches!(self.0.ip(), IpAddr::V6(_))
    }
}

impl Default for SamBridgeEndpoint {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl fmt::Debug for SamBridgeEndpoint {
    /// Shows the address family and port, never the literal.
    ///
    /// A bridge endpoint is operator-supplied configuration rather than secret, so this
    /// is not redaction for its own sake. The reason is that a `Debug` line reaches
    /// diagnostics and support requests, and the port is the part anyone actually needs
    /// in order to tell two configured endpoints apart; the IP is the part that is
    /// redundant, since it can only be one of two loopback literals.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SamBridgeEndpoint({}:{})",
            if self.is_ipv6() {
                "ipv6-loopback"
            } else {
                "ipv4-loopback"
            },
            self.0.port()
        )
    }
}

/// Why a configured bridge address was refused.
///
/// Every variant is a configuration mistake the Operator can fix. None of them carries
/// the rejected text, because a rejected value is operator input that has no business in
/// a log line.
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum BridgeEndpointError {
    #[error("no bridge endpoint configured")]
    Empty,
    #[error("bridge endpoint must be a numeric loopback address with a port")]
    NotNumeric,
    #[error("bridge endpoint is missing a port")]
    MissingPort,
    #[error("bridge endpoint is missing an address")]
    MissingHost,
    #[error("bridge port must be 1-65535")]
    InvalidPort,
    /// The boundary itself. Named separately from every other variant so the failure a
    /// reviewer is looking for is legible in a diagnostic.
    #[error("only a loopback SAM bridge is permitted")]
    NotLoopback,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv6Addr;

    #[test]
    fn the_default_endpoint_is_ipv4_loopback_on_the_sam_port() {
        assert_eq!(SamBridgeEndpoint::DEFAULT.port(), 7656);
        assert_eq!(
            SamBridgeEndpoint::DEFAULT.ip(),
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        );
        assert!(!SamBridgeEndpoint::DEFAULT.is_ipv6());
        assert_eq!(
            SamBridgeEndpoint::parse("127.0.0.1:7656").expect("the default form parses"),
            SamBridgeEndpoint::DEFAULT
        );
    }

    /// The boundary is the whole reason this type exists, so it is pinned in both
    /// families and for several shapes of the same violation.
    #[test]
    fn a_non_loopback_address_is_refused() {
        for rejected in [
            "192.168.1.10:7656",
            "10.0.0.1:7656",
            "0.0.0.0:7656",
            "8.8.8.8:7656",
            "[2001:db8::1]:7656",
            // The unspecified addresses are loopback-adjacent and just as unroutable.
            "[::]:7656",
        ] {
            assert_eq!(
                SamBridgeEndpoint::parse(rejected),
                Err(BridgeEndpointError::NotLoopback),
                "{rejected} must be refused as non-loopback"
            );
        }
    }

    /// `localhost` is refused rather than accepted.
    ///
    /// It resolves to loopback on every sane host, so accepting it looks harmless. The
    /// reason it is refused is that accepting it means the *resolution* decides where to
    /// connect: a hosts file or a resolver could point it anywhere, and this crate has no
    /// resolver call to be wrong about.
    #[test]
    fn a_host_name_is_refused_including_localhost() {
        for rejected in [
            "localhost:7656",
            "LOCALHOST:7656",
            "localhost",
            "router.local:7656",
            "127.0.0.1.example.com:7656",
        ] {
            assert!(
                matches!(
                    SamBridgeEndpoint::parse(rejected),
                    Err(BridgeEndpointError::NotNumeric) | Err(BridgeEndpointError::MissingPort)
                ),
                "{rejected} must be refused as a name, not resolved"
            );
        }
    }

    /// A URL is not an address, and a path is not an address.
    #[test]
    fn a_url_or_path_is_refused() {
        for rejected in [
            "http://127.0.0.1:7656/",
            "tcp://127.0.0.1:7656",
            "127.0.0.1:7656/",
            "/var/run/sam.sock",
            "127.0.0.1:7656\nGET /",
        ] {
            assert_eq!(
                SamBridgeEndpoint::parse(rejected),
                Err(BridgeEndpointError::NotNumeric),
                "{rejected} must be refused as a non-address"
            );
        }
    }

    #[test]
    fn ipv6_loopback_is_accepted_in_bracket_form() {
        let parsed = SamBridgeEndpoint::parse("[::1]:7656").expect("explicit IPv6 parses");
        assert_eq!(parsed.ip(), IpAddr::V6(Ipv6Addr::LOCALHOST));
        assert_eq!(parsed.port(), 7656);
        assert!(parsed.is_ipv6());
        // Both loopback families are accepted; only non-loopback is not.
        for accepted in [
            "[::1]:7656",
            "[0:0:0:0:0:0:0:1]:7656",
            "[0000:0000:0000:0000:0000:0000:0000:0001]:7656",
            // All of 127.0.0.0/8 is loopback, not just 127.0.0.1.
            "127.0.0.1:1",
            "127.0.0.53:65535",
        ] {
            assert!(
                SamBridgeEndpoint::parse(accepted).is_ok(),
                "{accepted} is a loopback endpoint and must be accepted"
            );
        }
    }

    #[test]
    fn a_missing_or_unusable_port_is_refused() {
        for rejected in ["127.0.0.1", "127.0.0.1:", "127.0.0.1:0", "127.0.0.1:65536"] {
            assert!(
                matches!(
                    SamBridgeEndpoint::parse(rejected),
                    Err(BridgeEndpointError::InvalidPort) | Err(BridgeEndpointError::MissingPort)
                ),
                "{rejected} must be refused as a port problem"
            );
        }
        assert_eq!(
            SamBridgeEndpoint::parse(""),
            Err(BridgeEndpointError::Empty)
        );
        assert_eq!(
            SamBridgeEndpoint::parse(":7656"),
            Err(BridgeEndpointError::MissingHost)
        );
    }

    /// The `Debug` line is what reaches a diagnostic, so it is pinned.
    #[test]
    fn debug_reports_family_and_port_without_the_literal() {
        let rendered = format!("{:?}", SamBridgeEndpoint::DEFAULT);
        assert_eq!(rendered, "SamBridgeEndpoint(ipv4-loopback:7656)");
        assert!(!rendered.contains("127.0.0.1"));
        assert_eq!(
            format!(
                "{:?}",
                SamBridgeEndpoint::parse("[::1]:9000").expect("loopback parses")
            ),
            "SamBridgeEndpoint(ipv6-loopback:9000)"
        );
    }

    /// The maximum and maximum-plus-one of every accepted form.
    #[test]
    fn every_bound_is_reachable_from_its_literal_form() {
        for port in [1u16, 7656, 65535] {
            assert_eq!(
                SamBridgeEndpoint::parse(&format!("127.0.0.1:{port}"))
                    .expect("max port range parses")
                    .port(),
                port
            );
            assert_eq!(
                SamBridgeEndpoint::parse(&format!("[::1]:{port}"))
                    .expect("ipv6 max port range parses")
                    .port(),
                port
            );
        }
    }
}
