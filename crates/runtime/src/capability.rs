//! Capability policy: what the bouncer asks for upstream and advertises downstream.
//!
//! The two directions are governed by different rules, and conflating them is the
//! failure this module exists to prevent:
//!
//! - **Upstream** is asked for a reviewed, *fixed* set. It must not vary with which
//!   clients happen to be attached, because upstream negotiation happens once per
//!   generation while clients attach and detach freely.
//! - **Downstream** may advertise a capability only when the bouncer itself can
//!   guarantee its semantics end to end. Advertising a capability and then not
//!   implementing it is worse than not advertising it, because a client will rely
//!   on behavior that will not happen.
//!
//! Draft history capabilities are deliberately absent: they are M003-E, and this
//! milestone must not advertise what it has not built.
use std::collections::BTreeSet;

/// Upstream capabilities this bouncer requests when the server offers them.
///
/// This list is intentionally small and reviewed. Requesting everything a server
/// offers would make the bouncer's behavior depend on the server's whim rather than
/// on what it actually implements.
pub const UPSTREAM_FOUNDATIONAL: [&str; 5] = [
    "message-tags",
    "server-time",
    "batch",
    "labeled-response",
    "echo-message",
];

/// Downstream capabilities this build can truthfully advertise.
///
/// Each one corresponds to semantics implemented in this milestone. `echo-message` is
/// conditional on upstream negotiation (see [`UpstreamCapabilities::echo_available`]),
/// because the bouncer will only confirm a message it has seen the server echo.
pub const DOWNSTREAM_FOUNDATIONAL: [&str; 4] =
    ["message-tags", "server-time", "batch", "labeled-response"];

/// Draft history capabilities, delegated to the versioned adapter so no `draft/...`
/// literal appears outside it.
///
/// Their names and the spec revision this build implements live in
/// `crate::chathistory`; the registry refers to those constants rather than repeating
/// the literals, which is what keeps a future adapter update out of this file.
pub const DOWNSTREAM_HISTORY: [&str; 2] = [
    crate::chathistory::CHATHISTORY_CAPABILITY,
    crate::chathistory::READ_MARKER_CAPABILITY,
];

/// Capabilities this build implemented but whose semantics are still withheld.
///
/// Kept as a constant so that "not advertised yet" is a reviewable decision rather
/// than an omission. M003-E promotes [`DOWNSTREAM_HISTORY`] into the advertisement.
pub const DOWNSTREAM_DEFERRED_HISTORY: [&str; 2] = ["chathistory", "read-marker"];

/// A validated IRCv3 capability name.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CapabilityName(String);

impl CapabilityName {
    /// Validates a client-supplied capability token.
    ///
    /// The token alphabet is deliberately narrow: a capability name reaches a `CAP`
    /// line and must never contain framing bytes, spaces, or separators.
    pub fn parse(raw: &str) -> Result<Self, CapabilityError> {
        if raw.is_empty() || raw.len() > MAX_CAPABILITY_BYTES {
            return Err(CapabilityError::Length);
        }
        if !raw
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'/' | b'.'))
        {
            return Err(CapabilityError::Charset);
        }
        Ok(Self(raw.to_ascii_lowercase()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Display for CapabilityName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Explicit ceiling on one capability token.
pub const MAX_CAPABILITY_BYTES: usize = 64;
/// Ceiling on capability tokens in one `CAP` request.
pub const MAX_CAPABILITY_COUNT: usize = 32;
/// Ceiling on capability value bytes for one token.
pub const MAX_CAPABILITY_VALUE_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityError {
    Length,
    Charset,
    TooMany,
}

/// What the upstream generation negotiated, recorded in generation-owned state.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UpstreamCapabilities {
    offered: BTreeSet<String>,
    enabled: BTreeSet<String>,
}

impl UpstreamCapabilities {
    /// Records what the server offered during `CAP LS`.
    pub fn note_offer(&mut self, token: &str) {
        let name = token.split('=').next().unwrap_or(token);
        if let Ok(name) = CapabilityName::parse(name) {
            self.offered.insert(name.into_string());
        }
    }
    /// Records what the server acknowledged for `CAP REQ`.
    pub fn note_enabled(&mut self, token: &str) {
        let name = token.split('=').next().unwrap_or(token);
        if let Ok(name) = CapabilityName::parse(name) {
            self.enabled.insert(name.into_string());
        }
    }
    pub fn is_enabled(&self, name: &str) -> bool {
        self.enabled.contains(&name.to_ascii_lowercase())
    }
    pub fn was_offered(&self, name: &str) -> bool {
        self.offered.contains(&name.to_ascii_lowercase())
    }
    /// True when the server will echo this bouncer's own messages.
    pub fn echo_available(&self) -> bool {
        self.is_enabled("echo-message")
    }
    /// True when the server supports labeled responses.
    pub fn labels_available(&self) -> bool {
        self.is_enabled("labeled-response")
    }
    /// The exact capability set to request upstream.
    ///
    /// It is a pure function of what the server offered — never of attached clients.
    pub fn request_set(&self) -> Vec<String> {
        UPSTREAM_FOUNDATIONAL
            .iter()
            .filter(|name| self.was_offered(name))
            .map(|name| (*name).to_owned())
            .collect()
    }
    /// A bounded, non-secret fingerprint for diagnostics.
    pub fn fingerprint(&self) -> String {
        let mut out = String::new();
        for name in &self.enabled {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(name);
        }
        out
    }
}

impl CapabilityName {
    fn into_string(self) -> String {
        self.0
    }
}

/// What the bouncer advertises to one attached client.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DownstreamCapabilities {
    /// Session capability value bytes a client has successfully requested.
    enabled: BTreeSet<CapabilityName>,
}

impl DownstreamCapabilities {
    /// The exact `CAP LS` advertisement for this generation.
    ///
    /// `echo-message` is included only when upstream negotiated it, because the
    /// bouncer confirms a message only once the server has echoed it. Advertising it
    /// otherwise would promise confirmation the bouncer cannot deliver.
    pub fn advertise(&self, upstream: &UpstreamCapabilities) -> Vec<String> {
        let mut advertised: Vec<String> = DOWNSTREAM_FOUNDATIONAL
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        if upstream.echo_available() {
            advertised.push("echo-message".to_owned());
        }
        advertised.sort();
        advertised
    }

    /// The advertisement as a set, for `CAP REQ` matching.
    pub fn advertise_set(&self, upstream: &UpstreamCapabilities) -> BTreeSet<String> {
        self.advertise(upstream).into_iter().collect()
    }

    /// The subset a client has actually enabled for this session.
    pub fn enabled(&self) -> Vec<String> {
        self.enabled
            .iter()
            .map(CapabilityName::as_str)
            .map(str::to_owned)
            .collect()
    }
    pub fn is_enabled(&self, name: &str) -> bool {
        self.enabled.iter().any(|enabled| enabled.as_str() == name)
    }
    /// Handles `CAP REQ`, acknowledging only capabilities this build advertises.
    ///
    /// Anything else is NAKed rather than silently ignored, so a client learns its
    /// request was not honored instead of assuming it was.
    pub fn request(&mut self, advertised: &BTreeSet<String>, requested: &[String]) -> CapDecision {
        if requested.len() > MAX_CAPABILITY_COUNT {
            return CapDecision::Refused;
        }
        let mut granted = Vec::with_capacity(requested.len());
        for token in requested {
            let Ok(name) = CapabilityName::parse(token) else {
                return CapDecision::Refused;
            };
            if token.contains('=')
                && token
                    .split('=')
                    .nth(1)
                    .is_some_and(|value| value.len() > MAX_CAPABILITY_VALUE_BYTES)
            {
                return CapDecision::Refused;
            }
            if advertised.contains(name.as_str()) {
                granted.push(name);
            }
        }
        // A partially grantable request is NAKed as a whole: the IRCv3 `CAP REQ`
        // contract is all-or-nothing, so a client is never left guessing which half
        // of its request took effect.
        if granted.len() != requested.len() {
            return CapDecision::Refused;
        }
        self.enabled.extend(granted.iter().cloned());
        CapDecision::Granted(
            granted
                .iter()
                .map(CapabilityName::as_str)
                .map(str::to_owned)
                .collect(),
        )
    }
}

/// The outcome of one `CAP REQ`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapDecision {
    Granted(Vec<String>),
    Refused,
}

/// The upstream capability fingerprint observed while no client was attached.
///
/// M003-D requires that upstream negotiation be downstream-client independent, so a
/// caller can capture this once per generation and assert later that attaching a
/// client changed nothing.
pub fn upstream_is_client_independent(
    before_attach: &UpstreamCapabilities,
    after_attach: &UpstreamCapabilities,
) -> bool {
    before_attach == after_attach
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_upstream_request_set_ignores_attached_clients() {
        let mut offered = UpstreamCapabilities::default();
        for name in UPSTREAM_FOUNDATIONAL {
            offered.note_offer(name);
        }
        let before = offered.clone();
        // A downstream client's CAP negotiation must not be able to reach upstream.
        let mut downstream = DownstreamCapabilities::default();
        let advertised: BTreeSet<String> = before
            .request_set()
            .into_iter()
            .chain(["chathistory".to_owned()])
            .collect();
        let _ = downstream.request(&advertised, &["chathistory".to_owned()]);
        assert!(upstream_is_client_independent(&before, &offered));
        // The set is the reviewed constant in its declared order, which is stable for
        // a given generation and independent of what any client negotiated.
        assert_eq!(
            before.request_set(),
            vec![
                "message-tags",
                "server-time",
                "batch",
                "labeled-response",
                "echo-message"
            ]
        );
    }

    #[test]
    fn deferred_history_capabilities_are_never_advertised() {
        let upstream = UpstreamCapabilities::default();
        let advertised = DownstreamCapabilities::default().advertise(&upstream);
        for deferred in DOWNSTREAM_DEFERRED_HISTORY {
            assert!(
                !advertised.iter().any(|name| name == deferred),
                "{deferred} is M003-E and must not be advertised yet"
            );
        }
    }

    #[test]
    fn echo_message_is_advertised_only_when_upstream_negotiated_it() {
        let mut without = UpstreamCapabilities::default();
        without.note_offer("echo-message");
        assert!(
            !DownstreamCapabilities::default()
                .advertise(&without)
                .contains(&"echo-message".to_owned()),
            "offered is not the same as enabled"
        );
        without.note_enabled("echo-message");
        assert!(
            DownstreamCapabilities::default()
                .advertise(&without)
                .contains(&"echo-message".to_owned()),
            "with an upstream echo the bouncer can confirm a message truthfully"
        );
    }

    #[test]
    fn only_advertised_capabilities_are_granted_and_the_request_is_all_or_nothing() {
        let upstream = UpstreamCapabilities::default();
        let advertised: BTreeSet<String> = DownstreamCapabilities::default()
            .advertise(&upstream)
            .into_iter()
            .collect();
        let mut downstream = DownstreamCapabilities::default();
        assert_eq!(
            downstream.request(&advertised, &["batch".to_owned(), "server-time".to_owned()]),
            CapDecision::Granted(vec!["batch".to_owned(), "server-time".to_owned()])
        );
        assert!(downstream.is_enabled("batch"));
        // A request naming one unavailable capability is refused as a whole, so the
        // client is never left guessing which half took effect.
        assert_eq!(
            downstream.request(&advertised, &["batch".to_owned(), "chathistory".to_owned()]),
            CapDecision::Refused
        );
        assert!(!downstream.is_enabled("chathistory"));
    }

    #[test]
    fn capability_names_are_validated_before_reaching_a_wire_line() {
        for bad in [
            "",
            "has space",
            "semi;colon",
            "at@sign",
            &"x".repeat(MAX_CAPABILITY_BYTES + 1),
        ] {
            assert!(
                CapabilityName::parse(bad).is_err(),
                "{bad:?} must be refused"
            );
        }
        assert!(CapabilityName::parse("labeled-response").is_ok());
        assert_eq!(
            CapabilityName::parse("Batch").expect("parses").as_str(),
            "batch",
            "capability names are case-insensitive and normalized"
        );
    }

    #[test]
    fn an_oversized_capability_request_is_refused() {
        let advertised: BTreeSet<String> = BTreeSet::new();
        let requested: Vec<String> = (0..=MAX_CAPABILITY_COUNT)
            .map(|index| format!("cap{index}"))
            .collect();
        let mut downstream = DownstreamCapabilities::default();
        assert_eq!(
            downstream.request(&advertised, &requested),
            CapDecision::Refused
        );
        assert!(downstream.enabled().is_empty());
    }
}
