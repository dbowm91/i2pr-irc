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
//! only for the explicitly specified command families in [`RequestClass`], with at most
//! one outstanding query per family so that attribution is unambiguous.
//!
//! # Why an unknown label is dropped rather than fanned out
//!
//! A `label` tag on an upstream frame is a *response* label: it exists only to answer a
//! request. If it matches no live route the request is stale or was never ours, and the
//! frame belongs to nobody. Fanning it out would hand one client's orphaned reply to
//! every other client. An unknown *batch* reference is the opposite case — the server
//! opens batches of its own (`server-time` batching, for example) — so it fans out
//! normally.
use crate::{RuntimeError, capability::CapabilityError};
use i2pr_irc_core::{ClientId, SessionId};
use std::collections::HashMap;

/// Ceiling on simultaneous routes for one Network generation.
///
/// A full table refuses new correlated requests with a local busy disposition rather
/// than growing without limit.
pub const MAX_ROUTES: usize = 128;
/// Ceiling on tracked upstream batch references for one generation.
///
/// A batch is tracked only to attribute it to a labeled route, so this bounds how much
/// batch state a generation can hold. Exhaustion fails the route closed rather than
/// leaving its batch contents unattributable.
pub const MAX_ROUTE_BATCHES: usize = MAX_ROUTES;
/// Ceiling on one generated upstream label.
pub const MAX_LABEL_BYTES: usize = 64;
/// Ceiling on one downstream label the bouncer will accept in translation.
pub const MAX_DOWNSTREAM_LABEL_BYTES: usize = 64;
/// Default lifetime of one route.
pub const ROUTE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
/// Ceiling on how many times a route may absorb additional replies.
pub const MAX_MULTIPART_REPLIES: usize = 64;

/// The IRCv3 response-label tag. It carries a required value, so a routed frame is
/// emitted as `@label=<opaque>` rather than a valueless tag.
pub const LABEL_TAG: &str = "label";

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
    /// Every numeric reply that belongs to this family, terminator included.
    ///
    /// An unlabeled reply carries no request identity at all, so the whole family —
    /// not just its terminator — is what a single outstanding query can claim. Routing
    /// only the terminator would leave `RPL_WHOISUSER` and friends fanning out to every
    /// attached client, which is exactly the disclosure this module exists to prevent.
    ///
    /// The sets are disjoint, which is what makes "at most one outstanding query per
    /// family" sufficient: a numeric can belong to only one family, so attributing it
    /// to that family's open route is never a guess.
    pub fn numerics(self) -> &'static [&'static str] {
        match self {
            Self::Whois => &["311", "312", "313", "314", "317", "318", "319"],
            Self::Who => &["315", "352", "354"],
            Self::Names => &["353", "366"],
            Self::List => &["321", "322", "323"],
        }
    }
    /// The family a numeric reply belongs to, if any.
    pub fn family_of(numeric: &str) -> Option<Self> {
        [Self::Whois, Self::Who, Self::Names, Self::List]
            .into_iter()
            .find(|class| class.numerics().contains(&numeric))
    }
}

/// Who is asking, and what, for one routed request.
///
/// Grouping these keeps [`ResponseRouter::route`]'s signature to a single argument
/// object, so adding a field later does not silently widen a seven-parameter call.
#[derive(Clone, Copy, Debug)]
pub struct RoutingRequest<'a> {
    /// The live attachment making the request.
    pub session: SessionId,
    /// The durable lineage that attachment belongs to.
    pub client: ClientId,
    /// The uppercase command.
    pub command: &'a str,
    /// Its parameters, with any label already removed.
    pub params: &'a [String],
    /// The client's own label, if it sent one.
    pub downstream_label: Option<&'a str>,
    /// Whether upstream negotiated `labeled-response`.
    pub labeled_upstream: bool,
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
    ///
    /// `upstream_label` is the opaque token that was allocated, so the caller can
    /// cancel exactly this route if the upstream queue then refuses the frame.
    Frame {
        line: String,
        upstream_label: Option<String>,
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
    /// The frame is not a correlated reply: ordinary fanout to every session.
    Fanout,
    /// The frame looks like a correlated reply but belongs to no live route. It is
    /// dropped rather than delivered to a client that did not ask for it.
    Dropped,
    /// A reply was delivered and the route remains open (multipart/batch).
    Continued(Delivered),
    /// A reply was delivered and the route is now closed.
    Completed(Delivered),
}

/// Whether an upstream `BATCH` frame opens or closes a batch.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BatchRole {
    /// Not a `BATCH` command frame.
    #[default]
    None,
    /// `BATCH +<reference>`.
    Open,
    /// `BATCH -<reference>`.
    Close,
}

/// One upstream message, as the router sees it.
///
/// The owner derives this from a parsed frame so the router never re-parses wire text
/// and never has to guess what a tag contained.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Incoming<'a> {
    /// The `label` tag value, when the server sent one.
    pub label: Option<&'a str>,
    /// The batch reference: the `batch` tag, or the reference of a `BATCH` command.
    pub batch: Option<&'a str>,
    /// The three-digit numeric, when this frame is one.
    pub numeric: Option<&'a str>,
    /// Whether this frame opens or closes a batch.
    pub batch_role: BatchRole,
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
    /// Upstream batch reference -> the upstream label whose route owns it.
    ///
    /// This exists so the messages *inside* a labeled-response batch, which carry only
    /// `batch=<reference>` and no label of their own, still reach the session that
    /// asked the question.
    batches: HashMap<String, String>,
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
            batches: HashMap::new(),
            timeout,
        }
    }

    pub fn open_routes(&self) -> usize {
        self.routes.len() + self.fallback.len()
    }
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty() && self.fallback.is_empty()
    }
    /// Batch references currently attributed to an open route.
    pub fn open_batches(&self) -> usize {
        self.batches.len()
    }

    /// Removes every expired route, releasing its fallback slot deterministically.
    pub fn expire(&mut self, now: std::time::Instant) -> usize {
        let before = self.open_routes();
        self.routes
            .retain(|_, route| now.duration_since(route.created) < self.timeout);
        self.fallback
            .retain(|_, route| now.duration_since(route.created) < self.timeout);
        self.prune_batches();
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
        self.prune_batches();
        before - self.open_routes()
    }

    /// Routes one client command, translating its label when present.
    pub fn route(&mut self, request: RoutingRequest<'_>, now: std::time::Instant) -> Routed {
        let RoutingRequest {
            session,
            client,
            command,
            params,
            downstream_label: label,
            labeled_upstream,
        } = request;
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
            let Some(upstream_label) = self.allocate_label() else {
                return Routed::Refused(RouteRefusal::Busy);
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
                self.routes.remove(&upstream_label);
                return Routed::Refused(RouteRefusal::Busy);
            }
            return Routed::Frame {
                line: render(command, params, Some(&upstream_label)),
                upstream_label: Some(upstream_label),
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
            upstream_label: None,
        }
    }

    /// Cancels one labeled route, used when the upstream queue refuses its frame.
    ///
    /// A frame that was never admitted definitely did not reach the server, so the
    /// route that was opened for it must not survive: it would otherwise consume a
    /// route slot and, worse, capture a later reply that belongs to nobody.
    pub fn cancel_labeled(&mut self, upstream_label: &str) -> bool {
        self.remove_labeled(upstream_label).is_some()
    }

    /// Cancels one unlabeled fallback route for the same reason.
    pub fn cancel_fallback(&mut self, class: RequestClass) -> bool {
        self.fallback.remove(&class).is_some()
    }

    /// Routes one upstream reply back to its requesting session.
    pub fn deliver(
        &mut self,
        incoming: Incoming<'_>,
        rebuild: impl FnOnce(&Route) -> Vec<u8>,
    ) -> RouteOutcome {
        if let Some(label) = incoming.label {
            // A response label identifies one request. No live route means the reply
            // is orphaned, and delivering it anywhere would be a misdelivery.
            if !self.routes.contains_key(label) {
                return RouteOutcome::Dropped;
            }
            return self.deliver_labeled(label, incoming, rebuild);
        }
        if let Some(reference) = incoming.batch {
            if let Some(label) = self.batches.get(reference).cloned() {
                return self.deliver_batch(&label, incoming, rebuild);
            }
            // An untracked batch reference belongs to the server, not to a route: the
            // server opens batches of its own and those fan out normally.
            return RouteOutcome::Fanout;
        }
        let Some(numeric) = incoming.numeric else {
            return RouteOutcome::Fanout;
        };
        let Some(class) = RequestClass::family_of(numeric) else {
            return RouteOutcome::Fanout;
        };
        if !self.fallback.contains_key(&class) {
            return RouteOutcome::Fanout;
        }
        self.deliver_fallback(class, incoming, rebuild)
    }

    /// Delivers a frame that carried the route's own label.
    fn deliver_labeled(
        &mut self,
        label: &str,
        incoming: Incoming<'_>,
        rebuild: impl FnOnce(&Route) -> Vec<u8>,
    ) -> RouteOutcome {
        if incoming.batch_role == BatchRole::Open {
            let Some(reference) = incoming.batch else {
                return RouteOutcome::Dropped;
            };
            if self.batches.len() >= MAX_ROUTE_BATCHES {
                // Fail closed: an untrackable batch would leave its contents
                // unattributable, so the route is abandoned rather than half-tracked.
                self.remove_labeled(label);
                return RouteOutcome::Dropped;
            }
            self.batches.insert(reference.to_owned(), label.to_owned());
            if let Some(route) = self.routes.get_mut(label) {
                route.kind = RouteKind::Batched;
            }
        }
        let Some(route) = self.routes.get(label).cloned() else {
            return RouteOutcome::Dropped;
        };
        let line = rebuild(&route);
        let session = route.session;
        let client = route.client;
        let batched = route.kind == RouteKind::Batched;
        let terminating = incoming
            .numeric
            .is_some_and(|value| value == route.class.terminator());
        let capped = self.charge_reply(label, RouteKey::Labeled);
        // A batched response ends when its batch closes: the closing frame carries no
        // label, so ending earlier would leave it orphaned and fanned out to everyone.
        let completed = if batched {
            incoming.batch_role == BatchRole::Close || capped
        } else {
            terminating || capped
        };
        if completed {
            self.remove_labeled(label);
            return RouteOutcome::Completed(Delivered {
                session,
                client,
                line,
                completed: true,
            });
        }
        RouteOutcome::Continued(Delivered {
            session,
            client,
            line,
            completed: false,
        })
    }

    /// Delivers a frame that belongs to a batch opened by a labeled route.
    fn deliver_batch(
        &mut self,
        label: &str,
        incoming: Incoming<'_>,
        rebuild: impl FnOnce(&Route) -> Vec<u8>,
    ) -> RouteOutcome {
        let Some(route) = self.routes.get(label).cloned() else {
            return RouteOutcome::Dropped;
        };
        let line = rebuild(&route);
        let session = route.session;
        let client = route.client;
        let capped = self.charge_reply(label, RouteKey::Labeled);
        let completed = incoming.batch_role == BatchRole::Close || capped;
        if completed {
            self.remove_labeled(label);
            return RouteOutcome::Completed(Delivered {
                session,
                client,
                line,
                completed: true,
            });
        }
        RouteOutcome::Continued(Delivered {
            session,
            client,
            line,
            completed: false,
        })
    }

    /// Delivers an unlabeled reply to the one outstanding query of its family.
    fn deliver_fallback(
        &mut self,
        class: RequestClass,
        incoming: Incoming<'_>,
        rebuild: impl FnOnce(&Route) -> Vec<u8>,
    ) -> RouteOutcome {
        let Some(route) = self.fallback.get(&class).cloned() else {
            return RouteOutcome::Fanout;
        };
        let line = rebuild(&route);
        let session = route.session;
        let client = route.client;
        let capped = self.charge_reply(class.as_str(), RouteKey::Fallback(class));
        let terminating = incoming
            .numeric
            .is_some_and(|value| value == class.terminator());
        if terminating || capped {
            self.fallback.remove(&class);
            return RouteOutcome::Completed(Delivered {
                session,
                client,
                line,
                completed: true,
            });
        }
        RouteOutcome::Continued(Delivered {
            session,
            client,
            line,
            completed: false,
        })
    }

    /// Counts one absorbed reply and reports whether the route hit its ceiling.
    fn charge_reply(&mut self, key: &str, kind: RouteKey) -> bool {
        let entry = match kind {
            RouteKey::Labeled => self.routes.get_mut(key),
            RouteKey::Fallback(_) => {
                let class = match kind {
                    RouteKey::Fallback(class) => class,
                    RouteKey::Labeled => unreachable!("labeled key with fallback key"),
                };
                self.fallback.get_mut(&class)
            }
        };
        let Some(entry) = entry else {
            return true;
        };
        entry.replies = entry.replies.saturating_add(1);
        entry.replies as usize >= MAX_MULTIPART_REPLIES
    }

    /// Removes one labeled route together with every batch attributed to it.
    fn remove_labeled(&mut self, label: &str) -> Option<Route> {
        let route = self.routes.remove(label)?;
        self.batches.retain(|_, owner| owner.as_str() != label);
        Some(route)
    }

    /// Drops batch references whose owning route no longer exists.
    fn prune_batches(&mut self) {
        let Self {
            routes, batches, ..
        } = self;
        batches.retain(|_, owner| routes.contains_key(owner));
    }

    /// Monotonic, collision-free within a generation.
    ///
    /// The value embeds the generation-scoped counter only. It never encodes a
    /// ClientId, SessionId, network name or nick, because this string is visible to the
    /// upstream server.
    fn allocate_label(&mut self) -> Option<String> {
        let value = self.next_label;
        self.next_label = self.next_label.checked_add(1)?;
        let label = format!("i2p{value:016x}");
        (label.len() <= MAX_LABEL_BYTES).then_some(label)
    }
}

/// Which table a charged reply belongs to.
#[derive(Clone, Copy)]
enum RouteKey {
    Labeled,
    Fallback(RequestClass),
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
///
/// The `label` tag carries a required value under the reviewed specification, so the
/// opaque token is emitted as `label=<value>`; a valueless `@label` would be a tag the
/// server cannot correlate against.
fn render(command: &str, params: &[String], label: Option<&str>) -> String {
    let mut line = String::new();
    if let Some(label) = label {
        line.push('@');
        line.push_str(LABEL_TAG);
        line.push('=');
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

/// The opaque upstream label a routed frame carries, for diagnostics and tests.
pub fn frame_label(frame: &str) -> Option<&str> {
    frame
        .strip_prefix("@")?
        .split_once('=')?
        .1
        .split(' ')
        .next()
}

/// Refuses an over-long label before it reaches the wire.
pub fn validate_label(label: &str) -> Result<(), RuntimeError> {
    if label.is_empty()
        || label.len() > MAX_DOWNSTREAM_LABEL_BYTES
        || label
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte == 0 || byte == b';' || byte == b'=')
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

    fn open(router: &mut ResponseRouter, session: u64, command: &str, label: &str) -> String {
        let Routed::Frame { line, .. } = router.route(
            RoutingRequest {
                session: SessionId(session),
                client: ClientId(session),
                command,
                params: &["alice".into()],
                downstream_label: Some(label),
                labeled_upstream: true,
            },
            std::time::Instant::now(),
        ) else {
            panic!("expected a routed frame")
        };
        line
    }

    fn labeled(label: Option<&str>) -> Incoming<'_> {
        Incoming {
            label,
            ..Incoming::default()
        }
    }

    #[test]
    fn only_specified_command_families_correlate() {
        let mut routes = router();
        let now = std::time::Instant::now();
        // An unsupported command family must not create a route.
        assert_eq!(
            routes.route(
                RoutingRequest {
                    session: SessionId(1),
                    client: ClientId(1),
                    command: "VERSION",
                    params: &["bouncer".into()],
                    downstream_label: Some("lbl"),
                    labeled_upstream: true,
                },
                now,
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
                RoutingRequest {
                    session: SessionId(1),
                    client: ClientId(1),
                    command,
                    params: &["target".into()],
                    downstream_label: Some("lbl"),
                    labeled_upstream: true,
                },
                now,
            );
            assert!(matches!(outcome, Routed::Frame { .. }), "{command}");
            assert_eq!(fresh.open_routes(), 1);
        }
    }

    #[test]
    fn the_same_downstream_label_from_two_sessions_never_collides_upstream() {
        let mut router = router();
        let first = open(&mut router, 1, "WHOIS", "same");
        let second = open(&mut router, 2, "WHOIS", "same");
        let first_label = frame_label(&first).expect("upstream label").to_owned();
        let second_label = frame_label(&second).expect("upstream label").to_owned();
        assert_ne!(
            first_label, second_label,
            "identical client labels must never collide upstream"
        );
        assert!(
            !first.contains("same"),
            "the client label must never appear upstream: {first}"
        );
    }

    #[test]
    fn an_upstream_label_never_encodes_a_session_or_client_identity() {
        // The label is a generation-local counter. It must be identical for identical
        // positions regardless of which client occupies them, which is only possible if
        // ClientId and SessionId are not inputs to its construction.
        let mut first_router = router();
        let a = open(&mut first_router, 1, "WHOIS", "l");
        let b = open(&mut first_router, 2, "WHOIS", "l");

        let mut other = router();
        let Routed::Frame { line: c, .. } = other.route(
            RoutingRequest {
                session: SessionId(4242),
                client: ClientId(4242),
                command: "WHOIS",
                params: &["alice".into()],
                downstream_label: Some("l"),
                labeled_upstream: true,
            },
            std::time::Instant::now(),
        ) else {
            panic!("expected a routed frame")
        };
        let Routed::Frame { line: d, .. } = other.route(
            RoutingRequest {
                session: SessionId(9_999_999),
                client: ClientId(7_777_777),
                command: "WHOIS",
                params: &["alice".into()],
                downstream_label: Some("l"),
                labeled_upstream: true,
            },
            std::time::Instant::now(),
        ) else {
            panic!("expected a routed frame")
        };

        assert_eq!(
            frame_label(&a),
            frame_label(&c),
            "the first label must not depend on who asked"
        );
        assert_eq!(
            frame_label(&b),
            frame_label(&d),
            "the second label must not depend on who asked"
        );
        for label in [a, b, c, d] {
            let rendered = label.clone();
            assert!(
                !rendered.contains("4242") && !rendered.contains("9999999"),
                "no session or client identity may appear in an upstream label: {rendered}"
            );
        }
    }

    #[test]
    fn a_generated_label_carries_the_value_the_specification_requires() {
        let mut router = router();
        let frame = open(&mut router, 1, "WHOIS", "mine");
        assert!(
            frame.starts_with("@label=i2p"),
            "the label tag must carry a value: {frame}"
        );
        assert!(frame_label(&frame).is_some());
        assert!(validate_label(frame_label(&frame).expect("label")).is_ok());
    }

    #[test]
    fn a_reply_is_delivered_to_the_session_that_asked_and_no_other() {
        let mut router = router();
        let frame = open(&mut router, 1, "WHOIS", "mine");
        let label = frame_label(&frame).expect("label").to_owned();
        let outcome = router.deliver(
            Incoming {
                label: Some(&label),
                numeric: Some("318"),
                ..Incoming::default()
            },
            |route| format!("WHOIS reply for session {}\r\n", route.session.0).into_bytes(),
        );
        let RouteOutcome::Completed(delivered) = outcome else {
            panic!("terminator must complete the route")
        };
        assert_eq!(delivered.session, SessionId(1));
        assert_eq!(delivered.client, ClientId(1));
    }

    #[test]
    fn a_stale_label_is_dropped_and_never_fanned_out() {
        let mut router = router();
        let frame = open(&mut router, 1, "WHOIS", "mine");
        let label = frame_label(&frame).expect("label").to_owned();
        router.drop_session(SessionId(1));
        assert_eq!(
            router.deliver(labeled(Some(&label)), |route| {
                panic!("no route may be rebuilt for {}", route.class.as_str())
            }),
            RouteOutcome::Dropped,
            "an orphaned labeled reply belongs to nobody"
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
                RoutingRequest {
                    session,
                    client: ClientId(1),
                    command: "WHO",
                    params: &["#c".into()],
                    downstream_label: Some("l"),
                    labeled_upstream: true,
                },
                now,
            ) else {
                panic!("route {index} should have been accepted")
            };
        }
        assert_eq!(router.open_routes(), MAX_ROUTES);
        assert_eq!(
            router.route(
                RoutingRequest {
                    session: SessionId(9999),
                    client: ClientId(1),
                    command: "WHO",
                    params: &["#c".into()],
                    downstream_label: Some("l"),
                    labeled_upstream: true,
                },
                now,
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
        open(&mut router, 1, "WHO", "l");
        open(&mut router, 2, "WHO", "l");
        assert_eq!(router.drop_session(SessionId(1)), 1);
        assert_eq!(router.open_routes(), 1);
    }

    #[test]
    fn a_refused_upstream_admission_leaves_no_route_behind() {
        let mut router = router();
        let frame = open(&mut router, 1, "WHOIS", "mine");
        let label = frame_label(&frame).expect("label").to_owned();
        assert_eq!(router.open_routes(), 1);
        // The upstream queue refused the frame, so the query definitely never reached
        // the server and nothing may still be waiting to claim its reply.
        assert!(router.cancel_labeled(&label));
        assert!(router.is_empty());
        assert_eq!(
            router.deliver(labeled(Some(&label)), |_| panic!("must not rebuild")),
            RouteOutcome::Dropped
        );
        assert!(
            !router.cancel_labeled(&label),
            "cancelling twice is a no-op"
        );
    }

    #[test]
    fn a_timed_out_route_releases_its_slot_deterministically() {
        let mut router = ResponseRouter::new(std::time::Duration::from_millis(1));
        let now = std::time::Instant::now();
        open(&mut router, 1, "WHO", "l");
        assert_eq!(router.open_routes(), 1);
        let later = now + std::time::Duration::from_millis(50);
        assert_eq!(router.expire(later), 1);
        assert!(router.is_empty());
        let later_frame = open(&mut router, 2, "WHO", "l");
        assert!(frame_label(&later_frame).is_some());
    }

    #[test]
    fn without_labels_only_one_ambiguous_query_per_family_may_be_open() {
        let mut router = router();
        let now = std::time::Instant::now();
        assert!(matches!(
            router.route(
                RoutingRequest {
                    session: SessionId(1),
                    client: ClientId(1),
                    command: "WHOIS",
                    params: &["alice".into()],
                    downstream_label: None,
                    labeled_upstream: false,
                },
                now,
            ),
            Routed::Frame { .. }
        ));
        // A competing request for the same family gets a deterministic busy result.
        assert_eq!(
            router.route(
                RoutingRequest {
                    session: SessionId(2),
                    client: ClientId(2),
                    command: "WHOIS",
                    params: &["bob".into()],
                    downstream_label: None,
                    labeled_upstream: false,
                },
                now,
            ),
            Routed::Refused(RouteRefusal::Busy)
        );
        // A different family is unaffected.
        assert!(matches!(
            router.route(
                RoutingRequest {
                    session: SessionId(2),
                    client: ClientId(2),
                    command: "WHO",
                    params: &["#c".into()],
                    downstream_label: None,
                    labeled_upstream: false,
                },
                now,
            ),
            Routed::Frame { .. }
        ));
    }

    #[test]
    fn a_refused_fallback_admission_frees_the_family_slot() {
        let mut router = router();
        let now = std::time::Instant::now();
        assert!(matches!(
            router.route(
                RoutingRequest {
                    session: SessionId(1),
                    client: ClientId(1),
                    command: "WHOIS",
                    params: &["alice".into()],
                    downstream_label: None,
                    labeled_upstream: false,
                },
                now,
            ),
            Routed::Frame { .. }
        ));
        assert!(router.cancel_fallback(RequestClass::Whois));
        assert!(router.is_empty());
        // The family slot is reusable, so a refused send cannot wedge the router.
        assert!(matches!(
            router.route(
                RoutingRequest {
                    session: SessionId(2),
                    client: ClientId(1),
                    command: "WHOIS",
                    params: &["bob".into()],
                    downstream_label: None,
                    labeled_upstream: false,
                },
                now,
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
                RoutingRequest {
                    session,
                    client: ClientId(1),
                    command: class.as_str(),
                    params: &["x".into()],
                    downstream_label: None,
                    labeled_upstream: false,
                },
                now,
            );
            // A numeric outside every family is not ours and still fans out.
            assert_eq!(
                router.deliver(
                    Incoming {
                        numeric: Some("372"),
                        ..Incoming::default()
                    },
                    |_| Vec::new()
                ),
                RouteOutcome::Fanout
            );
            assert_eq!(router.open_routes(), 1, "{terminator} must still be open");
            let RouteOutcome::Completed(delivered) = router.deliver(
                Incoming {
                    numeric: Some(terminator),
                    ..Incoming::default()
                },
                |route| format!("reply {}\r\n", route.session.0).into_bytes(),
            ) else {
                panic!("{terminator} must complete the route")
            };
            assert_eq!(delivered.session, session);
            assert!(router.is_empty());
        }
    }

    #[test]
    fn every_numeric_of_an_open_family_stays_with_its_asking_client() {
        // Routing only the terminator would let RPL_WHOISUSER fan out to every
        // attached client, disclosing one client's query to the others.
        for class in [
            RequestClass::Whois,
            RequestClass::Who,
            RequestClass::Names,
            RequestClass::List,
        ] {
            let mut router = router();
            let now = std::time::Instant::now();
            router.route(
                RoutingRequest {
                    session: SessionId(7),
                    client: ClientId(7),
                    command: class.as_str(),
                    params: &["x".into()],
                    downstream_label: None,
                    labeled_upstream: false,
                },
                now,
            );
            // Every non-terminator numeric must stay with the asking session, and the
            // terminator is checked last because reaching it ends the route.
            for numeric in class
                .numerics()
                .iter()
                .copied()
                .filter(|value| *value != class.terminator())
            {
                let outcome = router.deliver(
                    Incoming {
                        numeric: Some(numeric),
                        ..Incoming::default()
                    },
                    |route| format!("reply for {}\r\n", route.session.0).into_bytes(),
                );
                let RouteOutcome::Continued(delivered) = outcome else {
                    panic!("{numeric} must stay with the asking session")
                };
                assert_eq!(delivered.session, SessionId(7));
                assert_eq!(router.open_routes(), 1, "{numeric} must not complete");
            }
            let terminator = class.terminator();
            let RouteOutcome::Completed(delivered) = router.deliver(
                Incoming {
                    numeric: Some(terminator),
                    ..Incoming::default()
                },
                |route| format!("reply for {}\r\n", route.session.0).into_bytes(),
            ) else {
                panic!("{terminator} must complete {}", class.as_str())
            };
            assert_eq!(delivered.session, SessionId(7));
            assert!(router.is_empty());
        }
    }

    #[test]
    fn the_family_numeric_sets_are_disjoint() {
        // Disjointness is what makes "one outstanding query per family" sufficient to
        // attribute an unlabeled reply without guessing.
        let mut seen: Vec<&str> = Vec::new();
        for class in [
            RequestClass::Whois,
            RequestClass::Who,
            RequestClass::Names,
            RequestClass::List,
        ] {
            for numeric in class.numerics() {
                assert!(
                    !seen.contains(numeric),
                    "{numeric} belongs to more than one family"
                );
                seen.push(numeric);
            }
        }
        assert_eq!(RequestClass::family_of("372"), None);
        assert_eq!(RequestClass::family_of("318"), Some(RequestClass::Whois));
    }

    #[test]
    fn an_unlabeled_reply_with_no_open_route_fans_out() {
        let mut router = router();
        assert_eq!(
            router.deliver(
                Incoming {
                    numeric: Some("366"),
                    ..Incoming::default()
                },
                |route| panic!("must not rebuild for {}", route.class.as_str())
            ),
            RouteOutcome::Fanout
        );
        assert_eq!(
            router.deliver(Incoming::default(), |_| panic!("must not rebuild")),
            RouteOutcome::Fanout
        );
    }

    #[test]
    fn a_labeled_batch_reaches_its_owner_and_closes_on_the_terminator() {
        let mut router = router();
        let frame = open(&mut router, 1, "WHOIS", "mine");
        let label = frame_label(&frame).expect("label").to_owned();

        // `@label=X BATCH +ref labeled-response` opens the reply batch.
        let RouteOutcome::Continued(opened) = router.deliver(
            Incoming {
                label: Some(&label),
                batch: Some("ref"),
                batch_role: BatchRole::Open,
                ..Incoming::default()
            },
            |_| b"BATCH open\r\n".to_vec(),
        ) else {
            panic!("the batch opener belongs to the route")
        };
        assert_eq!(opened.session, SessionId(1));
        assert_eq!(router.open_batches(), 1);

        // The messages inside carry only `batch=ref`, never the label.
        let RouteOutcome::Continued(inner) = router.deliver(
            Incoming {
                batch: Some("ref"),
                numeric: Some("311"),
                ..Incoming::default()
            },
            |route| format!("311 for {}\r\n", route.session.0).into_bytes(),
        ) else {
            panic!("a batched message must keep reaching the asking session")
        };
        assert_eq!(inner.session, SessionId(1));
        assert!(
            !router.is_empty(),
            "a batched reply does not end at its own terminator"
        );

        // The closing frame is unlabeled and ends the route.
        let RouteOutcome::Completed(closed) = router.deliver(
            Incoming {
                batch: Some("ref"),
                batch_role: BatchRole::Close,
                ..Incoming::default()
            },
            |_| b"BATCH close\r\n".to_vec(),
        ) else {
            panic!("the batch terminator must close the route")
        };
        assert_eq!(closed.session, SessionId(1));
        assert!(router.is_empty());
        assert_eq!(router.open_batches(), 0, "a closed batch must not linger");
    }

    #[test]
    fn a_server_initiated_batch_still_fans_out() {
        let mut router = router();
        assert_eq!(
            router.deliver(
                Incoming {
                    batch: Some("serverbatch"),
                    batch_role: BatchRole::Open,
                    ..Incoming::default()
                },
                |_| panic!("must not rebuild")
            ),
            RouteOutcome::Fanout
        );
    }

    #[test]
    fn dropping_a_session_releases_its_batches() {
        let mut router = router();
        let frame = open(&mut router, 1, "WHOIS", "mine");
        let label = frame_label(&frame).expect("label").to_owned();
        router.deliver(
            Incoming {
                label: Some(&label),
                batch: Some("ref"),
                batch_role: BatchRole::Open,
                ..Incoming::default()
            },
            |_| Vec::new(),
        );
        assert_eq!(router.open_batches(), 1);
        router.drop_session(SessionId(1));
        assert_eq!(
            router.open_batches(),
            0,
            "batch state must not outlive the route that owns it"
        );
    }

    #[test]
    fn an_over_long_client_label_is_refused_before_it_reaches_the_wire() {
        let mut router = router();
        let now = std::time::Instant::now();
        let long = "x".repeat(MAX_DOWNSTREAM_LABEL_BYTES + 1);
        assert_eq!(
            router.route(
                RoutingRequest {
                    session: SessionId(1),
                    client: ClientId(1),
                    command: "WHOIS",
                    params: &["alice".into()],
                    downstream_label: Some(&long),
                    labeled_upstream: true,
                },
                now,
            ),
            Routed::Refused(RouteRefusal::Unsupported)
        );
        assert!(router.is_empty());
        assert!(validate_label(&long).is_err());
        assert!(validate_label("has space").is_err());
        assert!(validate_label("").is_err());
        assert!(validate_label("semi;colon").is_err());
        assert!(validate_label("ok-label_1").is_ok());
    }

    #[test]
    fn a_generated_label_is_bounded_and_opaque() {
        let mut router = router();
        let frame = open(&mut router, 1, "WHOIS", "mine");
        let label = frame_label(&frame).expect("label");
        assert!(label.len() <= MAX_LABEL_BYTES);
        assert!(
            !label.contains("mine"),
            "the client label must not be forwarded"
        );
    }
}
