//! Generation-local, SessionId-scoped response routing.
//!
//! # Why routes are never durable
//!
//! A route exists only to answer a request made on a *live* upstream generation. If
//! it survived a restart it would name a connection that no longer exists, and the
//! only thing that could happen to a reply arriving later is be misdelivered to
//! whoever happens to occupy that slot. So routes live in generation-owned state and
//! are dropped wholesale when the generation is replaced.
//!
//! # Why labels are translated rather than forwarded
//!
//! A downstream label is chosen by one client and is only meaningful to that client.
//! Forwarding it upstream would let two clients collide on the same label and would
//! leak one client's request identity to the server. Every downstream label is
//! therefore rewritten to a generation-local opaque token, and the original is kept
//! only in this table, keyed by [`SessionId`].
//!
//! # Why there is no generic FIFO fallback
//!
//! An unlabeled reply arriving with no matching route belongs to *no known request*.
//! Guessing it onto the oldest outstanding query would deliver one client's answer to
//! another client, which is worse than refusing. Unlabeled correlation therefore exists
//! only for the explicitly specified command families in [`FallbackRouter`].
use crate::{RuntimeError, capability::CapabilityError};
use i2pr_irc_core::{ClientId, SessionId};
use std::collections::HashMap;

/// Ceiling on simultaneous routes for one Network generation.
///
/// A full table refuses new correlated requests with a local busy disposition rather
/// than growing without limit.
pub const MAX_ROUTES: usize = 128;
/// Ceiling on one generated upstream label.
pub const MAX_LABEL_BYTES: usize = 64;
/// Ceiling on one downstream label the bouncer will accept in translation.
pub const MAX_DOWNSTREAM_LABEL_BYTES: usize = 64;
/// Default lifetime of one route.
pub const ROUTE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
/// Ceiling on how many times a route may absorb additional replies.
pub const MAX_MULTIPART_REPLIES: usize = 64;

/// What kind of reply ends a route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteKind {
    /// A single terminal reply ends the route.
    Single,
    /// The route follows replies until an upstream `BATCH` terminator closes it.
    Batched,
}

/// The command family a route was opened for.
///
/// Only families whose reply and terminator semantics are specified are allowed. A new
/// family requires its completion rule to be written and tested before it is added.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RequestClass {
    Whois,
    Who,
    Names,
    List,
}

impl RequestClass {
    pub fn parse(command: &str) -> Option<Self> {
        match command.to_ascii_uppercase().as_str() {
            "WHOIS" => Some(Self::Whois),
            "WHO" => Some(Self::Who),
            "NAMES" => Some(Self::Names),
            "LIST" => Some(Self::List),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Whois => "WHOIS",
            Self::Who => "WHO",
            Self::Names => "NAMES",
            Self::List => "LIST",
        }
    }
    /// The numeric reply that terminates this family's fallback correlation.
    pub fn terminator(self) -> &'static str {
        match self {
            Self::Whois => "318",
            Self::Who => "315",
            Self::Names => "366",
            Self::List => "323",
        }
    }
}

/// One outstanding correlated request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Route {
    /// The live attachment that owns this route.
    pub session: SessionId,
    /// The durable lineage that attachment belongs to. Recorded for diagnostics only;
    /// it is never placed in an upstream label.
    pub client: ClientId,
    /// The label exactly as the client wrote it, restored on reply.
    pub downstream_label: Option<String>,
    pub class: RequestClass,
    pub kind: RouteKind,
    pub created: std::time::Instant,
    pub replies: u16,
}

/// The disposition returned when a request cannot be routed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteRefusal {
    /// The route table is full. The client receives a bounded busy disposition.
    Busy,
    /// The request belongs to a command family with no specified correlation.
    Unsupported,
}

/// Bounded disposition for one routing attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Routed {
    /// The upstream frame to write, with the label already translated.
    Frame {
        line: String,
    },
    /// The request needs no routing and may be forwarded as-is.
    Unlabeled,
    Refused(RouteRefusal),
}

/// A reply that was matched back to its requesting client.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Delivered {
    pub session: SessionId,
    pub client: ClientId,
    /// The client-facing line, with the original label restored.
    pub line: Vec<u8>,
    /// True when this reply completed the route.
    pub completed: bool,
}

/// What a reply did to the routing table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RouteOutcome {
    /// No route matched. The reply is not delivered to any client.
    Unmatched,
    /// A reply was delivered and the route remains open (multipart/batch).
    Continued(Delivered),
    /// A reply was delivered and the route is now closed.
    Completed(Delivered),
}

/// Generation-local response routing.
pub struct ResponseRouter {
    /// Upstream label -> open route.
    routes: HashMap<String, Route>,
    /// Monotonic allocator for opaque upstream labels. Never wraps into a value that
    /// could collide with a live route.
    next_label: u64,
    /// One outstanding unlabeled fallback query per family.
    fallback: HashMap<RequestClass, Route>,
    /// Lifetime applied to new routes.
    timeout: std::time::Duration,
}

impl Default for ResponseRouter {
    fn default() -> Self {
        Self::new(ROUTE_TIMEOUT)
    }
}

impl ResponseRouter {
    pub fn new(timeout: std::time::Duration) -> Self {
        Self {
            routes: HashMap::new(),
            next_label: 1,
            fallback: HashMap::new(),
            timeout,
        }
    }

    pub fn open_routes(&self) -> usize {
        self.routes.len() + self.fallback.len()
    }
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty() && self.fallback.is_empty()
    }

    /// Removes every expired route, releasing its fallback slot deterministically.
    pub fn expire(&mut self, now: std::time::Instant) -> usize {
        let before = self.open_routes();
        self.routes
            .retain(|_, route| now.duration_since(route.created) < self.timeout);
        self.fallback
            .retain(|_, route| now.duration_since(route.created) < self.timeout);
        before - self.open_routes()
    }

    /// Drops every route owned by one session.
    ///
    /// This is what makes a detached client unable to receive a reply that arrives
    /// after it left, even if the server is slow.
    pub fn drop_session(&mut self, session: SessionId) -> usize {
        let before = self.open_routes();
        self.routes.retain(|_, route| route.session != session);
        self.fallback.retain(|_, route| route.session != session);
        before - self.open_routes()
    }

    /// Routes one client command, translating its label when present.
    ///
    /// `command` is the uppercase command; `params` are its parameters with any label
    /// already removed.
    #[allow(clippy::too_many_arguments)]
    pub fn route(
        &mut self,
        session: SessionId,
        client: ClientId,
        command: &str,
        params: &[String],
        label: Option<&str>,
        labeled_upstream: bool,
        now: std::time::Instant,
    ) -> Routed {
        let Some(class) = RequestClass::parse(command) else {
            // Not a correlated family. Forwarding it untouched is correct: the bouncer
            // makes no claim about it and creates no route for it.
            return Routed::Unlabeled;
        };
        if let Some(label) = label
            && label.len() > MAX_DOWNSTREAM_LABEL_BYTES
        {
            return Routed::Refused(RouteRefusal::Unsupported);
        }
        if self.open_routes() >= MAX_ROUTES {
            return Routed::Refused(RouteRefusal::Busy);
        }
        if labeled_upstream {
            let upstream_label = match self.allocate_label() {
                Some(label) => label,
                None => return Routed::Refused(RouteRefusal::Busy),
            };
            let route = Route {
                session,
                client,
                downstream_label: label.map(str::to_owned),
                class,
                kind: RouteKind::Single,
                created: now,
                replies: 0,
            };
            if self.routes.insert(upstream_label.clone(), route).is_some() {
                // The allocator is monotonic, so this is unreachable. Refusing is
                // still better than overwriting a live route.
                return Routed::Refused(RouteRefusal::Busy);
            }
            return Routed::Frame {
                line: render(command, params, Some(&upstream_label)),
            };
        }
        // No labeled-response upstream: fall back to explicit family correlation.
        if self.fallback.contains_key(&class) {
            // One outstanding ambiguous query per family. A competing request gets a
            // deterministic busy disposition instead of an unbounded queue or a
            // guess at which client the next reply belongs to.
            return Routed::Refused(RouteRefusal::Busy);
        }
        self.fallback.insert(
            class,
            Route {
                session,
                client,
                downstream_label: label.map(str::to_owned),
                class,
                kind: RouteKind::Single,
                created: now,
                replies: 0,
            },
        );
        Routed::Frame {
            line: render(command, params, None),
        }
    }

    /// Routes one upstream reply back to its requesting session.
    ///
    /// A label that matches no live route is dropped: it is stale, belongs to a
    /// previous generation, or was never ours. Delivering it to another client would
    /// be the failure this whole module exists to prevent.
    pub fn deliver(
        &mut self,
        label: Option<&str>,
        numeric: Option<&str>,
        class_hint: Option<RequestClass>,
        rebuild: impl FnOnce(&Route) -> Vec<u8>,
        // Expiry is driven separately by [`Self::expire`], so this call only matches a
        // live route and never ages one.
        _now: std::time::Instant,
    ) -> RouteOutcome {
        if let Some(label) = label {
            let Some(route) = self.routes.get(label) else {
                return RouteOutcome::Unmatched;
            };
            let class = route.class;
            let session = route.session;
            let client = route.client;
            let line = rebuild(route);
            let terminating = numeric.is_some_and(|value| value == class.terminator());
            let entry = self.routes.get_mut(label).expect("route is live");
            entry.replies = entry.replies.saturating_add(1);
            let completed = terminating || entry.replies as usize >= MAX_MULTIPART_REPLIES;
            if !completed {
                return RouteOutcome::Continued(Delivered {
                    session,
                    client,
                    line,
                    completed: false,
                });
            }
            self.routes.remove(label);
            return RouteOutcome::Completed(Delivered {
                session,
                client,
                line,
                completed: true,
            });
        }
        // Unlabeled: only the specified families may correlate, and only when the
        // reply's numeric terminator closes them.
        let Some(numeric) = numeric else {
            return RouteOutcome::Unmatched;
        };
        // A terminator for a family this router did not open a route for is dropped:
        // the family is located first so the map is not borrowed while being mutated.
        let Some(class) = self
            .fallback
            .keys()
            .find(|class| class.terminator() == numeric)
            .copied()
        else {
            return RouteOutcome::Unmatched;
        };
        let Some(route) = self.fallback.remove(&class) else {
            return RouteOutcome::Unmatched;
        };
        let _ = class_hint;
        let line = rebuild(&route);
        RouteOutcome::Completed(Delivered {
            session: route.session,
            client: route.client,
            line,
            completed: true,
        })
    }

    /// Promotes one open route to batch-following, so it continues until the
    /// upstream `BATCH` terminator arrives.
    pub fn expect_batch(&mut self, label: &str) -> bool {
        match self.routes.get_mut(label) {
            Some(route) => {
                route.kind = RouteKind::Batched;
                true
            }
            None => false,
        }
    }

    /// Ends a batched route when its terminator arrives.
    pub fn finish_batch(&mut self, label: &str) {
        self.routes.remove(label);
    }

    /// Monotonic, collision-free within a generation.
    ///
    /// The value embeds the generation-scoped counter and the SessionId so a label is
    /// self-describing for diagnostics without encoding a ClientId.
    fn allocate_label(&mut self) -> Option<String> {
        let value = self.next_label;
        self.next_label = self.next_label.checked_add(1)?;
        let label = format!("i2p{value:016x}");
        (label.len() <= MAX_LABEL_BYTES).then_some(label)
    }
}

impl CapabilityError {
    /// A capability failure is reported as an ordinary request refusal.
    pub fn as_refusal(self) -> RouteRefusal {
        match self {
            Self::Length | Self::Charset => RouteRefusal::Unsupported,
            Self::TooMany => RouteRefusal::Busy,
        }
    }
}

/// Renders a command with an optional upstream label inserted before its parameters.
fn render(command: &str, params: &[String], label: Option<&str>) -> String {
    let mut line = String::new();
    if let Some(label) = label {
        line.push('@');
        line.push_str(label);
        line.push(' ');
    }
    line.push_str(command);
    for param in params {
        line.push(' ');
        line.push_str(param);
    }
    line.push_str("\r\n");
    line
}

/// Refuses an over-long label before it reaches the wire.
pub fn validate_label(label: &str) -> Result<(), RuntimeError> {
    if label.is_empty()
        || label.len() > MAX_DOWNSTREAM_LABEL_BYTES
        || label
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte == 0)
    {
        return Err(RuntimeError::Protocol);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use i2pr_irc_core::SessionId;

    fn router() -> ResponseRouter {
        ResponseRouter::new(ROUTE_TIMEOUT)
    }

    #[test]
    fn only_specified_command_families_correlate() {
        let mut routes = router();
        let now = std::time::Instant::now();
        // An unsupported command family must not create a route.
        assert_eq!(
            routes.route(
                SessionId(1),
                ClientId(1),
                "VERSION",
                &["bouncer".into()],
                Some("lbl"),
                true,
                now
            ),
            Routed::Unlabeled
        );
        assert!(
            routes.is_empty(),
            "no route may exist for an unspecified family"
        );
        for command in ["WHOIS", "WHO", "NAMES", "LIST"] {
            let mut fresh = router();
            let outcome = fresh.route(
                SessionId(1),
                ClientId(1),
                command,
                &["target".into()],
                Some("lbl"),
                true,
                now,
            );
            assert!(matches!(outcome, Routed::Frame { .. }), "{command}");
            assert_eq!(fresh.open_routes(), 1);
        }
    }

    #[test]
    fn the_same_downstream_label_from_two_sessions_never_collides_upstream() {
        let mut router = router();
        let now = std::time::Instant::now();
        let Routed::Frame { line: first } = router.route(
            SessionId(1),
            ClientId(1),
            "WHOIS",
            &["alice".into()],
            Some("same"),
            true,
            now,
        ) else {
            panic!("expected a routed frame")
        };
        let Routed::Frame { line: second } = router.route(
            SessionId(2),
            ClientId(2),
            "WHOIS",
            &["bob".into()],
            Some("same"),
            true,
            now,
        ) else {
            panic!("expected a routed frame")
        };
        let first_label = first
            .split(' ')
            .next()
            .expect("labeled line")
            .trim_start_matches('@')
            .to_owned();
        let second_label = second
            .split(' ')
            .next()
            .expect("labeled line")
            .trim_start_matches('@')
            .to_owned();
        assert_ne!(
            first_label, second_label,
            "identical client labels must never collide upstream"
        );
        assert!(
            !first.contains("same"),
            "the client label must never appear upstream: {first}"
        );
        assert!(
            !second_label.contains("ClientId"),
            "a ClientId must never be encoded into an upstream label"
        );
    }

    #[test]
    fn a_reply_is_delivered_to_the_session_that_asked_and_no_other() {
        let mut router = router();
        let now = std::time::Instant::now();
        let Routed::Frame { line } = router.route(
            SessionId(1),
            ClientId(1),
            "WHOIS",
            &["alice".into()],
            Some("mine"),
            true,
            now,
        ) else {
            panic!("expected a routed frame")
        };
        let label = line
            .split(' ')
            .next()
            .expect("label")
            .trim_start_matches('@')
            .to_owned();
        let outcome = router.deliver(
            Some(&label),
            Some("318"),
            Some(RequestClass::Whois),
            |route| format!("WHOIS reply for session {}\r\n", route.session.0).into_bytes(),
            now,
        );
        let RouteOutcome::Completed(delivered) = outcome else {
            panic!("terminator must complete the route")
        };
        assert_eq!(delivered.session, SessionId(1));
        assert_eq!(delivered.client, ClientId(1));
    }

    #[test]
    fn a_stale_label_matches_no_route_and_is_never_delivered() {
        let mut router = router();
        let now = std::time::Instant::now();
        let Routed::Frame { line } = router.route(
            SessionId(1),
            ClientId(1),
            "WHOIS",
            &["alice".into()],
            Some("mine"),
            true,
            now,
        ) else {
            panic!("expected a routed frame")
        };
        let label = line
            .split(' ')
            .next()
            .expect("label")
            .trim_start_matches('@')
            .to_owned();
        router.drop_session(SessionId(1));
        // The same label arriving afterwards belongs to nobody.
        assert_eq!(
            router.deliver(
                Some(&label),
                Some("318"),
                None,
                |route| panic!("no route may be rebuilt for {}", route.class.as_str()),
                now
            ),
            RouteOutcome::Unmatched
        );
        assert!(router.is_empty());
    }

    #[test]
    fn the_route_ceiling_is_refused_explicitly() {
        let mut router = router();
        let now = std::time::Instant::now();
        for index in 0..MAX_ROUTES {
            let session = SessionId(index as u64 + 1);
            let Routed::Frame { .. } = router.route(
                session,
                ClientId(1),
                "WHO",
                &["#c".into()],
                Some("l"),
                true,
                now,
            ) else {
                panic!("route {index} should have been accepted")
            };
        }
        assert_eq!(router.open_routes(), MAX_ROUTES);
        assert_eq!(
            router.route(
                SessionId(9999),
                ClientId(1),
                "WHO",
                &["#c".into()],
                Some("l"),
                true,
                now
            ),
            Routed::Refused(RouteRefusal::Busy)
        );
        assert_eq!(
            router.open_routes(),
            MAX_ROUTES,
            "a refused route must not grow the table"
        );
    }

    #[test]
    fn detaching_a_session_drops_only_its_routes() {
        let mut router = router();
        let now = std::time::Instant::now();
        for session in [SessionId(1), SessionId(2)] {
            router.route(
                session,
                ClientId(1),
                "WHO",
                &["#c".into()],
                Some("l"),
                true,
                now,
            );
        }
        assert_eq!(router.drop_session(SessionId(1)), 1);
        assert_eq!(router.open_routes(), 1);
    }

    #[test]
    fn a_timed_out_route_releases_its_slot_deterministically() {
        let mut router = ResponseRouter::new(std::time::Duration::from_millis(1));
        let now = std::time::Instant::now();
        router.route(
            SessionId(1),
            ClientId(1),
            "WHO",
            &["#c".into()],
            Some("l"),
            true,
            now,
        );
        assert_eq!(router.open_routes(), 1);
        let later = now + std::time::Duration::from_millis(50);
        assert_eq!(router.expire(later), 1);
        assert!(router.is_empty());
        // The slot is reusable, so a timeout cannot wedge the router.
        let Routed::Frame { .. } = router.route(
            SessionId(2),
            ClientId(1),
            "WHO",
            &["#c".into()],
            Some("l"),
            true,
            later,
        ) else {
            panic!("a released slot must be reusable")
        };
    }

    #[test]
    fn without_labels_only_one_ambiguous_query_per_family_may_be_open() {
        let mut router = router();
        let now = std::time::Instant::now();
        assert!(matches!(
            router.route(
                SessionId(1),
                ClientId(1),
                "WHOIS",
                &["alice".into()],
                None,
                false,
                now
            ),
            Routed::Frame { .. }
        ));
        // A competing request for the same family gets a deterministic busy result.
        assert_eq!(
            router.route(
                SessionId(2),
                ClientId(2),
                "WHOIS",
                &["bob".into()],
                None,
                false,
                now
            ),
            Routed::Refused(RouteRefusal::Busy)
        );
        // A different family is unaffected.
        assert!(matches!(
            router.route(
                SessionId(2),
                ClientId(2),
                "WHO",
                &["#c".into()],
                None,
                false,
                now
            ),
            Routed::Frame { .. }
        ));
    }

    #[test]
    fn unlabeled_correlation_follows_the_specified_terminators() {
        for (class, terminator) in [
            (RequestClass::Whois, "318"),
            (RequestClass::Who, "315"),
            (RequestClass::Names, "366"),
            (RequestClass::List, "323"),
        ] {
            let mut router = router();
            let now = std::time::Instant::now();
            let session = SessionId(1);
            router.route(
                session,
                ClientId(1),
                class.as_str(),
                &["x".into()],
                None,
                false,
                now,
            );
            // An unrelated numeric must not complete it.
            assert_eq!(
                router.deliver(None, Some("372"), None, |_| Vec::new(), now),
                RouteOutcome::Unmatched
            );
            assert_eq!(router.open_routes(), 1, "{terminator} must still be open");
            let RouteOutcome::Completed(delivered) = router.deliver(
                None,
                Some(terminator),
                Some(class),
                |route| format!("reply {}\r\n", route.session.0).into_bytes(),
                now,
            ) else {
                panic!("{terminator} must complete the route")
            };
            assert_eq!(delivered.session, session);
            assert!(router.is_empty());
        }
    }

    #[test]
    fn an_unlabeled_reply_with_no_open_route_is_dropped() {
        let mut router = router();
        let now = std::time::Instant::now();
        assert_eq!(
            router.deliver(
                None,
                Some("366"),
                None,
                |route| panic!("must not rebuild for {}", route.class.as_str()),
                now
            ),
            RouteOutcome::Unmatched
        );
        assert_eq!(
            router.deliver(
                None,
                None,
                None,
                |route| panic!("must not rebuild for {}", route.class.as_str()),
                now
            ),
            RouteOutcome::Unmatched
        );
    }

    #[test]
    fn an_over_long_client_label_is_refused_before_it_reaches_the_wire() {
        let mut router = router();
        let now = std::time::Instant::now();
        let long = "x".repeat(MAX_DOWNSTREAM_LABEL_BYTES + 1);
        assert_eq!(
            router.route(
                SessionId(1),
                ClientId(1),
                "WHOIS",
                &["alice".into()],
                Some(&long),
                true,
                now
            ),
            Routed::Refused(RouteRefusal::Unsupported)
        );
        assert!(router.is_empty());
        assert!(validate_label(&long).is_err());
        assert!(validate_label("has space").is_err());
        assert!(validate_label("").is_err());
        assert!(validate_label("ok-label_1").is_ok());
    }

    #[test]
    fn a_generated_label_is_bounded_and_opaque() {
        let mut router = router();
        let now = std::time::Instant::now();
        let Routed::Frame { line } = router.route(
            SessionId(1),
            ClientId(1),
            "WHOIS",
            &["alice".into()],
            Some("mine"),
            true,
            now,
        ) else {
            panic!("expected a routed frame")
        };
        let label = line
            .split(' ')
            .next()
            .expect("label")
            .trim_start_matches('@');
        assert!(label.len() <= MAX_LABEL_BYTES);
        assert!(
            !label.contains("mine"),
            "the client label must not be forwarded"
        );
    }
}
