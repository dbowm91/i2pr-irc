//! Operator presence and preferred-nick policy.
//!
//! Two Operator decisions live here, and both are about what the bouncer presents
//! upstream on the Operator's behalf.
//!
//! **Presence.** A bouncer is a presence client: the Operator signs in when they are at
//! the keyboard and away when they are not. This module decides when the bouncer is away,
//! and the answer is never "a socket is attached". A socket count cannot tell an idle
//! foreground window from a background history sync, and a bouncer that reports itself
//! present because something is connected would be permanently, silently online.
//!
//! **Nick.** The configured nick is what the Operator asked for; the observed nick is
//! what the server agreed to. When they differ, reclaiming the preferred nick is a
//! policy the Operator opts into, and the two mechanisms available for it — MONITOR
//! availability notifications and bounded `ISON` probing — are both rate-bounded and
//! generation-owned.
//!
//! Three rules hold across everything in this module:
//!
//! - Nothing here derives identity from the host. Fallback nicks come from the configured
//!   nick and the server's own protocol state, never a hostname, login, process id, or
//!   environment value. That is a product boundary, not a style preference.
//! - Only *transitions* emit upstream traffic. Repeated equivalent events emit nothing,
//!   so a reconnect loop or a client that re-registers cannot turn into upstream chatter.
//! - Nothing here survives a generation. Reclaim timers and pending probes belong to the
//!   generation that created them and disappear with it.
use std::{collections::BTreeMap, time::Duration};

use i2pr_irc_core::SessionId;

pub use crate::session::SessionPresence;

/// The capability this build serves for passive/background downstream sessions.
///
/// The draft's name and revision live here rather than at the call sites, so a future
/// revision change is one edit. Advertising it is only truthful because
/// [`SessionPresence`] is implemented end to end: a session that says `PASSIVE` really is
/// excluded from active presence, and a session that says `ACTIVE` really is included.
pub const PRE_AWAY_CAPABILITY: &str = "draft/pre-away";

/// The away message the bouncer sends for automatic away.
///
/// Fixed and bouncer-owned rather than Operator-configurable. An away message is a line
/// that reaches every member of every channel the bouncer holds, and a configurable one
/// would mean validating and re-validiting an arbitrary string on every path that can
/// reach it. The decision is recorded in the closure record: a configurable text is
/// Operator-facing polish, and it is not worth a durable field whose only job is to
/// become an IRC field later.
pub const AUTO_AWAY_TEXT: &str = "Bouncer away: no active local client";

/// Explicit ceiling on registration attempts made because the preferred nick is in use.
///
/// Each attempt is a full `NICK` exchange with the server, so the ceiling is small on
/// purpose: a collision is usually one other client, and a loop that keeps guessing is
/// indistinguishable from an attack. Exhaustion is terminal until configuration or a
/// reconcile changes it.
pub const MAX_FALLBACK_NICK_ATTEMPTS: usize = 4;

/// Lowest nick length this build will assume.
///
/// The IRC default is 9, and a server that advertises no `NICKLEN` is entitled to the
/// default. Clamping upward to whatever a server advertises is done by the caller's
/// bounds, not here.
pub const DEFAULT_NICK_LENGTH: usize = 9;

/// Hard ceiling on a nick this build will ever write upstream.
///
/// A server advertising an absurd `NICKLEN` must not be able to make the bouncer emit an
/// unbounded line. This is above any real server's limit on purpose.
pub const MAX_NICK_LENGTH: usize = 64;

/// Floor on the reclaim probe interval.
///
/// Reclaim is a background courtesy, not a race. Probing more often than this because a
/// client attached would let local activity change upstream traffic, which is exactly the
/// client-independence this module exists to protect.
pub const RECLAIM_INTERVAL: Duration = Duration::from_secs(300);

/// Ceiling on reclaim `NICK` writes in one generation.
///
/// A reclaim that keeps writing forever is indistinguishable, from upstream, from a
/// client stuck in a loop. The cap turns that into a diagnostic and a quiet bouncer,
/// which is strictly better than either silence about it or unlimited traffic.
pub const MAX_RECLAIM_WRITES_PER_GENERATION: usize = 8;

/// Ceiling on monitored nicks in one `MONITOR` request.
///
/// The bouncer watches at most its own preferred nick. The ceiling exists so a
/// mis-parsed limit cannot make the bouncer ask the server about an unbounded set.
pub const MAX_MONITOR_TARGETS: usize = 8;

/// One Operator's durable presence policy, read from the Network record.
///
/// Live observation — the current nick, the current away state — is deliberately absent.
/// Only the decisions belong here; what the server currently agreed to is a fact about
/// the connection, and it is re-derived after every registration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresencePolicy {
    /// Go away upstream when no active session remains.
    pub auto_away: bool,
    /// Keep trying to reclaim the configured nick.
    pub keep_nick: bool,
}
impl PresencePolicy {
    /// Reads the policy from one durable Network record.
    pub fn from_record(record: &i2pr_irc_store::NetworkRecord) -> Self {
        Self {
            auto_away: record.auto_away,
            keep_nick: record.keep_nick,
        }
    }
    /// The policy a Network that predates this feature has.
    ///
    /// Both disabled. This is what the schema migration produces, and it is what every
    /// existing deployment must keep behaving like: a Network that started emitting
    /// `AWAY` or reclaim traffic because its binary was upgraded would be a change
    /// nobody asked for and nothing in the evidence would explain.
    pub const LEGACY: Self = Self {
        auto_away: false,
        keep_nick: false,
    };
}

/// The away state the bouncer should be holding upstream, derived rather than stored.
///
/// Precedence, in order, and the order is the whole point:
///
/// 1. an explicit manual `AWAY` from any session, whatever the session counts are;
/// 2. otherwise, automatic away when the policy is on and no active session exists;
/// 3. otherwise, present.
///
/// Manual-away is first because a session count cannot express intent. A client that says
/// "I am away" while ten other sessions are attached has said something more specific
/// than anything the counts imply, and a bouncer that overrides it with a count is
/// answering a question nobody asked.
pub fn away_decision(
    policy: PresencePolicy,
    manual: Option<&str>,
    active_sessions: usize,
) -> Option<String> {
    away_decision_with_origin(policy, manual, active_sessions).map(|(text, _)| text)
}

/// The away state and its [`AwayOrigin`] from the single precedence rule above.
///
/// [`away_decision`] delegates here so the text and the class can never disagree: there is
/// one decision, and the two outputs are two views of it. A bouncer that classified the
/// text separately would eventually disagree with itself about what its own policy did.
pub fn away_decision_with_origin(
    policy: PresencePolicy,
    manual: Option<&str>,
    active_sessions: usize,
) -> Option<(String, AwayOrigin)> {
    if let Some(text) = manual {
        return Some((text.to_owned(), AwayOrigin::Manual));
    }
    if policy.auto_away && active_sessions == 0 {
        return Some((AUTO_AWAY_TEXT.to_owned(), AwayOrigin::Automatic));
    }
    None
}

/// *Why* an away state is being held, for the diagnostics projection.
///
/// Recorded alongside the away text rather than re-derived from it at read time. The text
/// is Operator- or bouncer-written free form, so classifying from it would mean string
/// matching on a value the bouncer is free to change; the decision and the class come from
/// the same [`away_decision`] call, so they cannot disagree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AwayOrigin {
    /// The Operator asked to be away.
    Manual,
    /// The policy is on and no session counted as active.
    Automatic,
}

impl AwayOrigin {
    /// The fixed classification string shown in diagnostics.
    ///
    /// A closed set with a fixed spelling: a diagnostic reader parses these, so the set
    /// cannot grow silently.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Automatic => "automatic",
        }
    }
}

/// Aggregated presence for one Network, generation-scoped except for manual-away.
///
/// Manual-away lives here rather than on a session because it is Network-wide intent: any
/// session may set it and any session may clear it, and it must survive sessions
/// attaching and detaching. Tying it to a session would let an unrelated client silently
/// drop the Operator's away state.
#[derive(Clone, Debug)]
pub struct PresenceState {
    /// The Operator's explicit away text, if any.
    manual_away: Option<String>,
    /// The away state the bouncer last told upstream.
    ///
    /// Tracked so that only a *change* emits traffic. Without it, every reconnect and
    /// every session attach would rewrite the same away line upstream.
    upstream_away: Option<String>,
    policy: PresencePolicy,
}

impl Default for PresenceState {
    /// The legacy policy: neither automatic away nor nick reclaim.
    fn default() -> Self {
        Self::new(PresencePolicy::LEGACY)
    }
}

impl PresenceState {
    pub fn new(policy: PresencePolicy) -> Self {
        Self {
            manual_away: None,
            upstream_away: None,
            policy,
        }
    }

    pub fn policy(&self) -> PresencePolicy {
        self.policy
    }

    pub fn set_policy(&mut self, policy: PresencePolicy) {
        self.policy = policy;
    }

    /// The Operator's explicit away text.
    pub fn manual_away(&self) -> Option<&str> {
        self.manual_away.as_deref()
    }

    /// Records an explicit `AWAY` with a message.
    pub fn set_manual_away(&mut self, text: &str) {
        self.manual_away = Some(text.to_owned());
    }

    /// Records an explicit `AWAY` with no message, which clears manual-away.
    pub fn clear_manual_away(&mut self) {
        self.manual_away = None;
    }

    /// What upstream should currently believe about our away state.
    pub fn away_state(&self, active_sessions: usize) -> Option<String> {
        away_decision(self.policy, self.manual_away.as_deref(), active_sessions)
    }

    /// The away state together with why it is being held, from the same rule.
    pub fn away_state_with_origin(&self, active_sessions: usize) -> Option<(String, AwayOrigin)> {
        away_decision_with_origin(self.policy, self.manual_away.as_deref(), active_sessions)
    }

    /// What upstream is currently being told.
    pub fn upstream_away(&self) -> Option<&str> {
        self.upstream_away.as_deref()
    }

    /// Records what upstream was told, and reports whether it changed.
    ///
    /// The owner writes upstream `AWAY` only when this returns true, so a repeated
    /// equivalent event costs nothing. Returns the frame to write, if any.
    pub fn note_upstream_away(&mut self, desired: Option<String>) -> Option<Option<String>> {
        if self.upstream_away == desired {
            return None;
        }
        self.upstream_away = desired.clone();
        Some(Some(desired.unwrap_or_default()))
    }

    /// The copy a new generation starts from.
    ///
    /// The Operator's manual-away is *carried over* because it is their decision, made
    /// seconds ago and still current. The observed upstream away state is *dropped*
    /// because the server that reported it no longer knows us. Carrying one and dropping
    /// the other is the whole point: a reconnect re-applies intent rather than restoring
    /// a stale observation from a connection that is gone.
    pub fn for_generation(&self) -> Self {
        Self {
            manual_away: self.manual_away.clone(),
            upstream_away: None,
            policy: self.policy,
        }
    }

    /// The Operator's explicit away text, for an owner to carry across generations.
    pub fn manual(&self) -> Option<&str> {
        self.manual_away.as_deref()
    }
}

/// One session's presence classification.
pub fn count_active_sessions(sessions: &BTreeMap<SessionId, SessionPresence>) -> usize {
    sessions
        .values()
        .filter(|presence| **presence == SessionPresence::Active)
        .count()
}

/// A bounded, deterministic fallback nick sequence.
///
/// Every candidate is a function of the configured nick and the server's own `NICKLEN`.
/// Nothing else is available to the generator, which is what makes it safe to put in an
/// IRC field: there is no code path that could reach a hostname, a login, a process id,
/// or an environment value, because none of those are inputs.
#[derive(Clone, Debug)]
pub struct NickFallback {
    preferred: String,
    /// Server-advertised length, clamped to a sane range.
    length: usize,
    attempts: usize,
}

impl NickFallback {
    /// Builds a sequence from the configured nick.
    ///
    /// `advertised` is the server's `NICKLEN` when it advertised one. It is clamped to
    /// `[DEFAULT_NICK_LENGTH, MAX_NICK_LENGTH]`: a smaller value would truncate below a
    /// usable nick, and a larger one would let a server decide how long a line this
    /// bouncer writes may be.
    pub fn new(preferred: &str, advertised: Option<usize>) -> Self {
        let length = advertised
            .unwrap_or(DEFAULT_NICK_LENGTH)
            .clamp(1, MAX_NICK_LENGTH);
        Self {
            preferred: preferred.to_owned(),
            length,
            attempts: 0,
        }
    }

    /// How many candidates have been offered so far.
    pub fn attempts(&self) -> usize {
        self.attempts
    }

    /// Records that the preferred nick has already been offered.
    ///
    /// Registration offers it first, so the sequence's *first* candidate has been used
    /// before the server has said anything. Without this, the first `433` would be
    /// answered with the same nick the bouncer just sent — a second identical `NICK`
    /// that a server may answer identically, turning one collision into a loop of them
    /// while consuming the whole fallback budget on one name.
    pub fn prime(&mut self) {
        self.attempts = self.attempts.max(1);
    }

    /// Whether the sequence is exhausted.
    pub fn exhausted(&self) -> bool {
        self.attempts >= MAX_FALLBACK_NICK_ATTEMPTS
    }

    /// The next candidate, or `None` once the ceiling is reached.
    ///
    /// The sequence is `nick`, `nick_1`, `nick_2`, ... truncated to the server's length.
    /// Truncation is applied to the *base* first, so the suffix always survives and the
    /// candidates stay distinct: truncating `nick_1` to nine bytes would produce `nick_1`
    /// and truncating the next one the same way would produce the same nick forever,
    /// which is a loop rather than a fallback.
    ///
    /// Deliberately not named `next`: it is not an iterator, and a caller reaching for
    /// `next` on something that is not one tends to assume it can be chained.
    #[allow(clippy::should_implement_trait)]
    pub fn next_candidate(&mut self) -> Option<String> {
        if self.exhausted() {
            return None;
        }
        let index = self.attempts;
        self.attempts += 1;
        Some(self.candidate(index))
    }

    fn candidate(&self, index: usize) -> String {
        if index == 0 {
            return truncate_nick(&self.preferred, self.length);
        }
        let suffix = format!("_{index}");
        let room = self.length.saturating_sub(suffix.len());
        let base = truncate_nick(&self.preferred, room);
        // A one-byte nick cannot hold a suffix, so the index is folded in as the whole
        // nick rather than silently producing the same candidate repeatedly.
        if room == 0 {
            return suffix.trim_start_matches('_').to_owned();
        }
        format!("{base}{suffix}")
    }

    /// Replaces the observed length once the server has advertised one.
    ///
    /// Applied before any candidate is generated, because a candidate built against the
    /// default length could exceed what the server accepts and be rejected as a protocol
    /// error rather than as a collision.
    pub fn set_advertised_length(&mut self, advertised: Option<usize>) {
        if let Some(value) = advertised {
            self.length = value.clamp(1, MAX_NICK_LENGTH);
        }
    }
}

/// Truncates `nick` to `max` bytes while keeping it a legal nick.
///
/// A nick must start with a letter or one of the IRC special characters; truncating to
/// zero bytes, or to a byte that is not a legal first character, would produce a line the
/// server rejects as malformed — a different failure from a collision, and one that
/// would look like the bouncer generating garbage.
fn truncate_nick(nick: &str, max: usize) -> String {
    let mut out: String = nick.chars().take(max).collect();
    if out.is_empty() {
        return String::from("b");
    }
    if !is_nick_lead(out.as_bytes()[0]) {
        // Drop leading bytes until a legal lead byte appears. `?` and `*` are excluded
        // deliberately: they are wildcards in several server commands, so a nick
        // starting with one can be matched by patterns the Operator never wrote.
        let trimmed: Vec<char> = out
            .chars()
            .skip_while(|c| !is_nick_lead(*c as u8))
            .collect();
        out = if trimmed.is_empty() {
            String::from("b")
        } else {
            trimmed.into_iter().take(max).collect()
        };
    }
    out
}

/// Whether `byte` may begin an IRC nick.
fn is_nick_lead(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || matches!(byte, b'[' | b'\\' | b']' | b'^' | b'_' | b'{')
}

/// Whether `byte` may continue an IRC nick.
fn is_nick_body(byte: u8) -> bool {
    is_nick_lead(byte) || byte.is_ascii_digit() || matches!(byte, b'-' | b'|')
}

/// Whether `nick` is a syntactically valid IRC nick.
pub fn valid_nick(nick: &str) -> bool {
    let bytes = nick.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_NICK_LENGTH
        && is_nick_lead(bytes[0])
        && bytes[1..].iter().all(|byte| is_nick_body(*byte))
}

/// The mechanism a reclaim uses to learn that a nick is free.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReclaimStrategy {
    /// `MONITOR`, when the server advertises a usable limit.
    Monitor,
    /// Bounded `ISON` probing, when it does not.
    Probe,
}

/// Chooses the reclaim strategy from the server's advertised `MONITOR` limit.
///
/// `MONITOR=0` means the feature is disabled; a missing token means it is not advertised.
/// Both fall back to probing, which is slower but is a standard query every server
/// understands. Choosing `MONITOR` because it is merely *mentioned* would leave the
/// bouncer waiting forever for notifications a disabled feature never sends.
pub fn reclaim_strategy(monitor_limit: Option<usize>) -> ReclaimStrategy {
    match monitor_limit {
        Some(limit) if (1..=MAX_MONITOR_TARGETS).contains(&limit) => ReclaimStrategy::Monitor,
        _ => ReclaimStrategy::Probe,
    }
}

/// One generation's reclaim attempt state.
///
/// Generation-owned by construction: it is created when a generation starts and dropped
/// when that generation is replaced, so a probe scheduled by a connection that has since
/// died can never act on the connection that replaced it.
#[derive(Clone, Debug)]
pub struct ReclaimAttempt {
    /// The nick we are trying to get back.
    pub preferred: String,
    /// The nick we currently hold.
    pub current: String,
    /// How many `NICK` writes this generation has made.
    pub writes: usize,
    /// Whether this generation has evidence the preferred nick is free.
    pub evidence: bool,
}

impl ReclaimAttempt {
    pub fn new(preferred: &str, current: &str) -> Self {
        Self {
            preferred: preferred.to_owned(),
            current: current.to_owned(),
            writes: 0,
            evidence: false,
        }
    }

    /// Whether a `NICK` write is due now.
    ///
    /// Evidence — a `MONITOR OFFLINE` or an `ISON` reply — makes a write due
    /// immediately. Without evidence, a write happens on the bounded schedule, never on
    /// a client's activity.
    pub fn should_write(&self, elapsed: Duration, interval: Duration) -> bool {
        if self.current.eq_ignore_ascii_case(&self.preferred) {
            return false;
        }
        self.evidence || elapsed >= interval
    }
}

/// Records whether a reclaim write was accepted for this generation.
pub fn note_reclaim_write(attempt: &mut ReclaimAttempt, ceiling: usize) -> bool {
    if attempt.writes >= ceiling {
        return false;
    }
    attempt.writes += 1;
    attempt.evidence = false;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_away_outranks_every_session_count() {
        let policy = PresencePolicy {
            auto_away: true,
            keep_nick: false,
        };
        // Ten attached sessions do not overrule an explicit AWAY: a client that says it
        // is away has said something more specific than a count.
        assert_eq!(
            away_decision(policy, Some("lunch"), 10),
            Some("lunch".to_owned())
        );
        assert_eq!(
            away_decision(policy, Some("lunch"), 0),
            Some("lunch".to_owned())
        );
        // Clearing it hands control back to the policy.
        assert_eq!(
            away_decision(policy, None, 0),
            Some(AUTO_AWAY_TEXT.to_owned())
        );
        assert_eq!(away_decision(policy, None, 1), None);
    }

    #[test]
    fn auto_away_is_off_until_it_is_asked_for() {
        let policy = PresencePolicy::LEGACY;
        assert_eq!(away_decision(policy, None, 0), None);
        assert!(
            !policy.auto_away && !policy.keep_nick,
            "a migrated Network emits no new upstream traffic"
        );
    }

    #[test]
    fn only_an_away_transition_emits_traffic() {
        let mut state = PresenceState::new(PresencePolicy::LEGACY);
        state.set_manual_away("back soon");
        let first = state.away_state(3);
        assert_eq!(
            state.note_upstream_away(first.clone()),
            Some(Some("back soon".to_owned())),
            "the first evaluation has something to say"
        );
        assert_eq!(
            state.note_upstream_away(first),
            None,
            "an equivalent repeat says nothing, so a reconnect loop cannot chatter"
        );
        state.clear_manual_away();
        assert_eq!(
            state.note_upstream_away(state.away_state(3)),
            Some(Some(String::new())),
            "clearing emits the bare AWAY that returns upstream"
        );
    }

    #[test]
    fn a_new_generation_re_derives_presence_instead_of_restoring_it() {
        let mut state = PresenceState::new(PresencePolicy::LEGACY);
        state.set_manual_away("away");
        state.note_upstream_away(state.away_state(1));
        let mut next = state.for_generation();
        assert_eq!(
            next.upstream_away(),
            None,
            "the server has no record of our away state after a fresh registration"
        );
        assert_eq!(
            next.manual(),
            Some("away"),
            "the Operator's intent is carried: a reconnect must not quietly drop an \
             away the Operator set seconds ago"
        );
        assert_eq!(
            next.note_upstream_away(next.away_state(1)),
            Some(Some("away".to_owned())),
            "and because nothing was said upstream yet, re-applying it is one transition \
             and emits exactly one frame"
        );
    }

    #[test]
    fn fallback_nicks_are_distinct_bounded_and_legal() {
        let mut sequence = NickFallback::new("bot", Some(9));
        sequence.prime();
        let candidates: Vec<String> = std::iter::from_fn(|| sequence.next_candidate()).collect();
        assert_eq!(candidates, vec!["bot_1", "bot_2", "bot_3"]);
        assert!(
            !candidates.contains(&"bot".to_owned()),
            "the preferred nick was already offered before the server said anything, so \
             no candidate may repeat it"
        );
        for candidate in &candidates {
            assert!(valid_nick(candidate), "{candidate} is a legal nick");
            assert!(candidate.len() <= 9, "{candidate} respects NICKLEN");
        }
        let mut unique = candidates.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            candidates.len(),
            "no candidate repeats itself"
        );
        assert!(sequence.exhausted() && sequence.next_candidate().is_none());
    }

    #[test]
    fn a_long_configured_nick_is_truncated_not_appended() {
        let mut sequence = NickFallback::new("a-very-long-configured-nick", Some(9));
        assert_eq!(sequence.next_candidate().as_deref(), Some("a-very-lo"));
        assert_eq!(sequence.next_candidate().as_deref(), Some("a-very-_1"));
        // Truncation must never produce a nick longer than the server's limit.
        for index in 0..MAX_FALLBACK_NICK_ATTEMPTS {
            let mut fresh = NickFallback::new("a-very-long-configured-nick", Some(9));
            let mut seen = vec![];
            for _ in 0..=index {
                if let Some(candidate) = fresh.next_candidate() {
                    assert!(candidate.len() <= 9);
                    seen.push(candidate);
                }
            }
        }
    }

    #[test]
    fn a_nicklen_from_an_absurd_server_is_clamped() {
        let mut huge = NickFallback::new("bot", Some(100_000));
        assert_eq!(huge.next_candidate().as_deref(), Some("bot"));
        let mut zero = NickFallback::new("bot", Some(0));
        let candidate = zero.next_candidate().expect("a candidate exists");
        assert!(
            valid_nick(&candidate),
            "even a clamped-to-1 nick stays legal"
        );
    }

    #[test]
    fn truncation_never_produces_an_illegal_lead_byte() {
        // Digits are legal nick *body* but not a lead byte, so a nick that begins with
        // one must be trimmed rather than sent as-is.
        assert!(!valid_nick("1bot"), "a digit may not lead a nick");
        assert!(valid_nick("bot1"), "but it may appear in the body");
        assert_eq!(truncate_nick("1bot", 4), "bot");
        assert_eq!(truncate_nick("1bot", 1), "b");
        assert!(valid_nick(&truncate_nick("1bot", 1)));
        // `?` is a pattern wildcard in several server commands and is not a nick lead byte.
        assert!(!valid_nick("?bot"));
        assert_eq!(truncate_nick("?bot", 4), "bot");
        // An empty configured nick still yields something sendable.
        assert!(valid_nick(&truncate_nick("", 9)));
    }

    #[test]
    fn reclaim_uses_monitor_only_when_the_server_offers_it() {
        assert_eq!(reclaim_strategy(Some(4)), ReclaimStrategy::Monitor);
        assert_eq!(
            reclaim_strategy(Some(0)),
            ReclaimStrategy::Probe,
            "MONITOR=0 means the feature is disabled, not unlimited"
        );
        assert_eq!(reclaim_strategy(None), ReclaimStrategy::Probe);
        assert_eq!(
            reclaim_strategy(Some(MAX_MONITOR_TARGETS + 1)),
            ReclaimStrategy::Probe,
            "a limit beyond anything this build will use is not a usable one"
        );
    }

    #[test]
    fn reclaim_writes_only_on_evidence_or_the_bounded_schedule() {
        let mut attempt = ReclaimAttempt::new("bot", "bot_1");
        assert!(
            !attempt.should_write(Duration::from_secs(1), RECLAIM_INTERVAL),
            "a client attaching must not make a write due"
        );
        assert!(
            attempt.should_write(RECLAIM_INTERVAL, RECLAIM_INTERVAL),
            "once the schedule is due a write is due without any client activity"
        );
        assert!(
            !attempt.should_write(RECLAIM_INTERVAL / 2, RECLAIM_INTERVAL),
            "and not a moment before it"
        );
        attempt.evidence = true;
        assert!(attempt.should_write(Duration::from_secs(0), RECLAIM_INTERVAL));
        assert!(note_reclaim_write(&mut attempt, 2));
        assert!(note_reclaim_write(&mut attempt, 2));
        assert!(
            !note_reclaim_write(&mut attempt, 2),
            "writes are capped per generation"
        );
        assert!(
            !attempt.should_write(Duration::from_secs(1), RECLAIM_INTERVAL),
            "holding the nick already means there is nothing to reclaim"
        );
    }

    #[test]
    fn passive_sessions_do_not_count_as_active() {
        let mut sessions = BTreeMap::new();
        sessions.insert(SessionId(1), SessionPresence::Active);
        sessions.insert(SessionId(2), SessionPresence::Passive);
        sessions.insert(SessionId(3), SessionPresence::Passive);
        assert_eq!(
            count_active_sessions(&sessions),
            1,
            "two background history syncs are not an Operator at the keyboard"
        );
        sessions.clear();
        assert_eq!(count_active_sessions(&sessions), 0);
    }
}
