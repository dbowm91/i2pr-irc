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
use i2pr_irc_wire::Message;
use std::collections::BTreeSet;

/// Upstream capabilities this bouncer requests when the server offers them.
///
/// This list is intentionally small and reviewed. Requesting everything a server
/// offers would make the bouncer's behavior depend on the server's whim rather than
/// on what it actually implements.
///
/// M005-G adds the five member-state capabilities. Each is requested only because the
/// runtime handles every message form it enables, and each name is still filtered
/// through what the server actually offered, so the request set remains a pure function
/// of the server's answer.
pub const UPSTREAM_FOUNDATIONAL: [&str; 12] = [
    "message-tags",
    "server-time",
    "batch",
    "labeled-response",
    "echo-message",
    MEMBER_EXTENDED_JOIN,
    MEMBER_ACCOUNT_NOTIFY,
    MEMBER_AWAY_NOTIFY,
    MEMBER_MULTI_PREFIX,
    MEMBER_SETNAME,
    ACCOUNT_TAG,
    INVITE_NOTIFY,
];

/// `account-tag`: forward server-authenticated account metadata carried on live messages.
pub const ACCOUNT_TAG: &str = "account-tag";
/// `invite-notify`: forward third-party invite notifications only to sessions that asked.
pub const INVITE_NOTIFY: &str = "invite-notify";

/// `extended-join`: a JOIN carries the joining member's account and realname, so an
/// attaching client can be shown them without a WHO.
pub const MEMBER_EXTENDED_JOIN: &str = "extended-join";
/// `account-notify`: the server reports when a member logs into or out of an account.
pub const MEMBER_ACCOUNT_NOTIFY: &str = "account-notify";
/// `away-notify`: the server reports when a member becomes away or returns.
pub const MEMBER_AWAY_NOTIFY: &str = "away-notify";
/// `multi-prefix`: membership carries the member's complete prefix run rather than only
/// its highest symbol.
pub const MEMBER_MULTI_PREFIX: &str = "multi-prefix";
/// `setname`: a realname can be changed mid-connection, and the change is reported.
pub const MEMBER_SETNAME: &str = "setname";

/// The accepted member-state capabilities, as one reviewed set.
///
/// Named as a set rather than as five independent booleans because they share one
/// property that decides whether any of them may be advertised: the bouncer can only
/// mediate richer member metadata if the server first supplied it. A capability here
/// whose upstream negotiation failed is withheld downstream rather than advertised with
/// nothing behind it.
pub const MEMBER_CAPABILITIES: [&str; 5] = [
    MEMBER_EXTENDED_JOIN,
    MEMBER_ACCOUNT_NOTIFY,
    MEMBER_AWAY_NOTIFY,
    MEMBER_MULTI_PREFIX,
    MEMBER_SETNAME,
];

/// Member-state capabilities this build implements but does not serve.
///
/// Each is withheld for a stated reason rather than silently omitted, so "not offered"
/// is a reviewable decision. `chghost` is the instructive one: its specification falls
/// back to a synthetic `QUIT`/`JOIN`/`MODE` sequence for clients that did not negotiate
/// it, and a bouncer whose whole projection discipline is that it never claims a
/// membership a client did not see established must not synthesise membership events.
/// The other three are additive metadata this build does not yet derive or route.
pub const DOWNSTREAM_DEFERRED_MEMBER: [&str; 2] = ["chghost", "extended-monitor"];

/// Downstream capabilities this build can truthfully advertise.
///
/// This list is intentionally small and reviewed. Every entry must correspond to
/// semantics the live `SessionReader` actually implements behind
/// [`crate::downstream::DOWNSTREAM_ADVERTISED`], because a `CAP LS` the client cannot
/// rely on is a lie it has no way to detect.
///
/// M004-A promotes the tag surface now that the client-tag mediator and
/// `CLIENTTAGDENY` are live. `batch` is required alongside `labeled-response`: a client
/// that could not read a batch could not tell a multipart answer from a truncated one.
///
/// M005-F promotes `server-time` and `standard-replies`. `server-time` is promoted
/// because a `time` tag is already forwarded when upstream sent one and already
/// synthesized for history replay, and the only thing withholding it was that the
/// per-session filtering for it did not exist -- a client with `message-tags` but without
/// `server-time` would have received a tag it never asked for. That filter now exists.
/// `standard-replies` is promoted because `FAIL` is implemented across `CHATHISTORY`,
/// `MARKREAD`, `BOUNCER`, and `SEARCH` rather than borrowed for a single failure format.
pub const DOWNSTREAM_FOUNDATIONAL: [&str; 5] = [
    MESSAGE_TAGS,
    BATCH,
    LABELED_RESPONSE,
    SERVER_TIME,
    STANDARD_REPLIES,
];

/// `server-time`: an upstream `time` tag is forwarded, and a history replay without one
/// is stamped from the same single rule that indexes it. A live frame the upstream never
/// stamped stays unstamped rather than gaining a fabricated time.
pub const SERVER_TIME: &str = "server-time";

/// `standard-replies`: every refusal this build serves is a `FAIL` with a standard
/// numeric and a fixed reason, and no refusal echoes the request back.
pub const STANDARD_REPLIES: &str = "standard-replies";

/// `cap-notify`: the bouncer's downstream capability set is conditional on what upstream
/// negotiated (`echo-message`), so a client that negotiated only the initial `CAP LS`
/// would otherwise have no way to learn that a capability appeared or disappeared.
pub const CAP_NOTIFY: &str = "cap-notify";

/// `echo-message`: only serveable when upstream negotiated it, because the upstream echo
/// is the confirmation event and the bouncer cannot invent one.
pub const ECHO_MESSAGE: &str = "echo-message";

/// `draft/no-implicit-names`: a client that negotiated it is not sent the membership
/// block on JOIN and must ask for it.
pub const NO_IMPLICIT_NAMES: &str = "draft/no-implicit-names";

/// The pre-away draft this build serves.
///
/// M005-C promotes it because the semantics are implemented end to end: a session that
/// declares itself passive really is excluded from active presence, and one that declares
/// itself active really is included. Advertising it and then treating `PASSIVE` as a no-op
/// would make a background history sync silently keep the Operator looking online, which
/// is the failure this capability exists to fix.
pub const DOWNSTREAM_PRE_AWAY: [&str; 1] = [crate::presence::PRE_AWAY_CAPABILITY];

/// The message-tag capability.
pub const MESSAGE_TAGS: &str = "message-tags";
/// The batch capability.
pub const BATCH: &str = "batch";
/// The labeled-response capability.
pub const LABELED_RESPONSE: &str = "labeled-response";

/// Capabilities this build implements but does not serve downstream.
///
/// Kept as reviewable constants so that "advertised yet" is a decision someone made
/// rather than an omission someone forgot. `echo-message` is requested upstream but is
/// not served downstream, because confirming a message requires implementing the
/// confirmation and nothing does yet.
pub const DOWNSTREAM_DEFERRED_FOUNDATIONAL: [&str; 0] = [];
/// Nothing is deferred here any more: M005-F promotes `echo-message` (conditionally, on
/// upstream) and `server-time`. The empty array is kept so the "reviewed and deliberate"
/// statement survives the next promotion.
pub const DOWNSTREAM_DEFERRED_ECHO: [&str; 0] = [];

/// Nothing is deferred here any more: M005-F promotes `server-time`.
pub const DOWNSTREAM_DEFERRED_SERVER_TIME: [&str; 0] = [];

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

/// Ceiling on how many capability names one `CAP NEW`/`CAP DEL` may name.
///
/// A `CAP` line is bounded by the wire decoder, but the names inside it are what this
/// bouncer would then act on: an unbounded announcement would be an unbounded fanout
/// decision. Eight is above every capability set this build negotiates.
pub const MAX_CAPABILITY_CHANGE_NAMES: usize = 8;

/// Which direction a server capability announcement goes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapChange {
    /// The server gained a capability it will honour.
    New,
    /// The server withdrew a capability it previously offered.
    Del,
}

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
    /// Records a mid-generation `CAP NEW` from the server.
    ///
    /// A server may add a capability at any time. Treating that as protocol failure
    /// would tear down a healthy generation over an advertisement, and ignoring it
    /// would leave the bouncer requesting a capability the server now offers but this
    /// connection never asked for. Neither is right: the offer is recorded, and what the
    /// bouncer can actually *do* with it is decided by the bouncer's own code.
    pub fn note_new(&mut self, token: &str) {
        let name = token.split('=').next().unwrap_or(token);
        if let Ok(name) = CapabilityName::parse(name) {
            self.offered.insert(name.into_string());
        }
    }

    /// Records a mid-generation `CAP DEL` from the server.
    ///
    /// The capability is removed from both the offered and the enabled set. Leaving it
    /// enabled would have the bouncer rely on something the server has just withdrawn,
    /// which is the one direction of drift that produces silent misbehaviour rather
    /// than a visible failure.
    pub fn note_deleted(&mut self, token: &str) {
        let name = token.split('=').next().unwrap_or(token);
        if let Ok(name) = CapabilityName::parse(name) {
            let name = name.into_string();
            self.offered.remove(&name);
            self.enabled.remove(&name);
        }
    }

    /// The change a server announced, if this line was one.
    ///
    /// Returns the subcommand and the capability names it names. Malformed lines are
    /// reported as no change rather than as a protocol error: a `CAP` line the bouncer
    /// cannot parse is not worth losing a generation over.
    pub fn note_change(message: &Message) -> Option<(CapChange, Vec<String>)> {
        // The nick is an optional first parameter, so the subcommand is found by
        // position rather than assumed. Assuming `params[0]` is the subcommand would read
        // a `CAP * NEW` as having no subcommand at all and silently ignore every
        // announcement a server ever sends with its own nick attached.
        // The nick is an optional first parameter, so the subcommand is found by
        // position rather than assumed at a fixed index. Assuming `params[0]` is the
        // subcommand reads a `CAP * NEW :x` as having none at all, which silently
        // ignores every announcement a server sends with its own nick attached.
        let at = message.params.iter().position(|param| {
            let value = String::from_utf8_lossy(param).to_ascii_uppercase();
            value == "NEW" || value == "DEL"
        })?;
        let change = match String::from_utf8_lossy(&message.params[at])
            .to_ascii_uppercase()
            .as_str()
        {
            "NEW" => CapChange::New,
            _ => CapChange::Del,
        };
        let names: Vec<String> = message
            .params
            .iter()
            .skip(at + 1)
            .flat_map(|param| {
                String::from_utf8_lossy(param)
                    .into_owned()
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .filter(|name| !name.is_empty())
            .take(MAX_CAPABILITY_CHANGE_NAMES)
            .map(|name| name.to_ascii_lowercase())
            .collect();
        (!names.is_empty()).then_some((change, names))
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
        advertised.push(CAP_NOTIFY.to_owned());
        advertised.push(NO_IMPLICIT_NAMES.to_owned());
        if upstream.echo_available() {
            advertised.push(ECHO_MESSAGE.to_owned());
        }
        if upstream.is_enabled(ACCOUNT_TAG) {
            advertised.push(ACCOUNT_TAG.to_owned());
        }
        if upstream.is_enabled(INVITE_NOTIFY) {
            advertised.push(INVITE_NOTIFY.to_owned());
        }
        // Member-state capabilities are conditional on upstream for the same reason
        // `echo-message` is: the bouncer mediates what the server supplied, so a server
        // that never offered `extended-join` leaves nothing to mediate and advertising it
        // would promise a richer JOIN than any client could ever receive.
        advertised.extend(
            MEMBER_CAPABILITIES
                .iter()
                .filter(|name| upstream.is_enabled(name))
                .map(|name| (*name).to_owned()),
        );
        advertised.sort();
        advertised
    }

    /// The complete downstream capability set for one generation.
    ///
    /// This is the single authority the live `SessionReader` uses for `CAP LS`. It is
    /// defined here rather than in `downstream` so that the history drafts and the
    /// foundational set cannot drift apart, and so a capability cannot be advertised by
    /// one module while the live reader omits it.
    pub fn advertisement(upstream: &UpstreamCapabilities) -> Vec<String> {
        let mut advertised: Vec<String> = crate::downstream::DOWNSTREAM_ADVERTISED
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        let foundational = DownstreamCapabilities::default().advertise(upstream);
        advertised.extend(foundational);
        advertised.sort();
        advertised.dedup();
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
        assert_eq!(before.request_set(), UPSTREAM_FOUNDATIONAL);
        // Every accepted member-state capability is requested for the same reason: the
        // runtime mediates every message form it enables. Nothing in the request set may
        // be one this build only serves sometimes.
        for name in MEMBER_CAPABILITIES {
            assert!(
                UPSTREAM_FOUNDATIONAL.contains(&name),
                "{name} is mediated downstream, so it must be requested upstream"
            );
        }
    }

    #[test]
    fn member_capabilities_are_withheld_downstream_when_upstream_never_supplied_them() {
        // The property that decides whether any of the five may be advertised: with
        // nothing upstream, advertising them would promise a richer view than any client
        // could ever receive. It is the *acknowledged* set that counts, not the offered
        // one -- a server can offer a capability and still refuse it.
        let mut offered = UpstreamCapabilities::default();
        for name in UPSTREAM_FOUNDATIONAL {
            offered.note_offer(name);
            offered.note_enabled(name);
        }
        let advertised = DownstreamCapabilities::default().advertise(&offered);
        for name in MEMBER_CAPABILITIES {
            assert!(
                advertised.iter().any(|entry| entry == name),
                "{name} was offered upstream, so it must be advertised downstream"
            );
        }
        // A server that negotiated only the foundational set supplies no member metadata,
        // so every member capability is withheld rather than advertised empty.
        let mut bare = UpstreamCapabilities::default();
        bare.note_offer("message-tags");
        let advertised = DownstreamCapabilities::default().advertise(&bare);
        for name in MEMBER_CAPABILITIES {
            assert!(
                !advertised.iter().any(|entry| entry == name),
                "{name} must not be advertised without upstream agreement"
            );
        }
        // And the deferred names are never advertised at all, so a client probing for
        // them learns this build does not serve them.
        for name in DOWNSTREAM_DEFERRED_MEMBER {
            assert!(
                !advertised.iter().any(|entry| entry == name),
                "{name} is deferred and must not be advertised"
            );
        }
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
    fn account_tag_and_invite_notify_are_advertised_only_after_upstream_ack() {
        let mut upstream = UpstreamCapabilities::default();
        upstream.note_offer(ACCOUNT_TAG);
        upstream.note_offer(INVITE_NOTIFY);
        assert!(
            !DownstreamCapabilities::default()
                .advertise(&upstream)
                .contains(&ACCOUNT_TAG.to_owned())
        );
        assert!(
            !DownstreamCapabilities::default()
                .advertise(&upstream)
                .contains(&INVITE_NOTIFY.to_owned())
        );
        upstream.note_enabled(ACCOUNT_TAG);
        upstream.note_enabled(INVITE_NOTIFY);
        let advertised = DownstreamCapabilities::default().advertise(&upstream);
        assert!(advertised.contains(&ACCOUNT_TAG.to_owned()));
        assert!(advertised.contains(&INVITE_NOTIFY.to_owned()));
    }

    #[test]
    fn only_advertised_capabilities_are_granted_and_the_request_is_all_or_nothing() {
        // The advertisement is the live set, so this exercises the same authority the
        // session reader uses rather than a parallel claim that could drift from it.
        let upstream = UpstreamCapabilities::default();
        let advertised: BTreeSet<String> = DownstreamCapabilities::advertisement(&upstream)
            .into_iter()
            .collect();
        let history = crate::downstream::DOWNSTREAM_ADVERTISED[0];
        let mut downstream = DownstreamCapabilities::default();
        assert_eq!(
            downstream.request(&advertised, &[history.to_owned()]),
            CapDecision::Granted(vec![history.to_owned()]),
            "a capability the live session serves must be grantable"
        );
        assert!(downstream.is_enabled(history));
        // A request naming one unavailable capability is refused as a whole, so the
        // client is never left guessing which half took effect.
        assert_eq!(
            downstream.request(&advertised, &[history.to_owned(), "nonsense".to_owned()]),
            CapDecision::Refused
        );
        assert!(
            !downstream.is_enabled("nonsense"),
            "an all-or-nothing refusal must leave nothing half-enabled"
        );
    }

    #[test]
    fn the_advertisement_never_exceeds_what_the_live_reader_serves() {
        // Corrective 014 reconciliation: a capability advertised by one module while the
        // live SessionReader omits it is a `CAP LS` the client has no way to challenge.
        let upstream = UpstreamCapabilities::default();
        let advertised = DownstreamCapabilities::advertisement(&upstream);
        for withheld in DOWNSTREAM_DEFERRED_FOUNDATIONAL
            .iter()
            .chain(DOWNSTREAM_DEFERRED_SERVER_TIME.iter())
        {
            assert!(
                !advertised.iter().any(|name| name == withheld),
                "{withheld} is withheld downstream and must not be advertised"
            );
        }
        assert!(
            advertised
                .iter()
                .all(|name| crate::downstream::downstream_supported().contains(&name.as_str())),
            "every advertised capability must be one the live reader serves"
        );
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
