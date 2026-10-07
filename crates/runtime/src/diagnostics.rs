//! The bounded, stable, secret-free diagnostics view of the running bouncer.
//!
//! Everything the Operator is shown comes from here, and this module is the reason it can
//! be shown safely. The values an owner already tracks are not secret, but the *shape* in
//! which they are exposed is a decision: a diagnostic surface that is assembled ad hoc at
//! each call site will eventually be assembled wrong once, and a credential in one string
//! format is a credential leaked.
//!
//! Two rules govern everything below.
//!
//! **Nothing here is derived from a secret-bearing value.** There is no endpoint, no
//! `Destination`, no SASL username or password, no registration-action payload, and no
//! filesystem path in any variant. That is not a policy applied while rendering; it is
//! the absence of a field to render, which is a stronger guarantee than a redaction pass.
//!
//! **Everything here is bounded.** Channel and capability lists are capped and counted, so
//! a Network with a thousand joined channels produces a diagnostic of a fixed size and a
//! separate overflow count rather than an unbounded reply.
//!
//! The projection is *stable* in the sense that matters: these names and meanings do not
//! change as the runtime changes underneath them, because an Operator's tooling reads
//! them. Adding a field is a compatible change; renaming or re-meaning one is not, and
//! needs its own plan.
use crate::owner::NetworkSnapshot;
use crate::reconnect::SchedulerDiagnostics;
use crate::resource::{Gauges, ResourceSnapshot};
use i2pr_irc_core::NetworkId;
use std::time::Duration;

/// Ceiling on channel names reported in one Network's diagnostic.
///
/// Overflow is counted, never silently dropped: a reader needs to know that a list is
/// truncated, and a list that merely stops is indistinguishable from a Network with six
/// channels.
pub const MAX_REPORTED_CHANNELS: usize = 64;
/// Ceiling on advertised capability names reported in one Network's diagnostic.
pub const MAX_REPORTED_CAPABILITIES: usize = 64;
/// Ceiling on rejected-join entries reported in one Network's diagnostic.
pub const MAX_REPORTED_REJECTIONS: usize = 16;

/// Whether the Operator's own presence is currently away, and why it is being reported as
/// such.
///
/// The class is the diagnostic, not the message. An away *reason* is text the Operator or
/// the bouncer wrote; whether the bouncer decided it, or the Operator did, is what an
/// operator reading this is actually trying to find out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AwayClass {
    /// No away is in effect, or nothing has been observed.
    Present,
    /// The bouncer went away on its own, because no session was active.
    Automatic,
    /// The Operator asked to be away.
    Manual,
    /// Upstream is being told an away state whose origin this generation did not decide.
    ///
    /// Reported rather than folded into the two above because a bouncer that is away for a
    /// reason it cannot account for is a real operational finding, and labelling it
    /// `Automatic` would hide exactly that.
    Unattributed,
}

/// One bounded, secret-free report of a single Network.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkDiagnostics {
    pub network: NetworkId,
    pub display_name: String,
    pub phase: Option<String>,
    pub generation: Option<u64>,
    /// The nick currently in use, if any.
    pub nick: Option<String>,
    pub away: AwayClass,
    /// How many desired channels are currently presented downstream.
    pub channels_visible: u64,
    /// How many are joined but withheld.
    pub channels_detached: u64,
    /// A bounded sample of the presented channels.
    pub channels_sample: Vec<String>,
    /// How many presented channels the sample did not include.
    pub channels_sample_overflow: u64,
    /// A bounded sample of the withheld channels, and the same overflow count.
    pub detached_sample: Vec<String>,
    pub detached_sample_overflow: u64,
    /// Sessions attached, and how they are classified.
    pub sessions_attached: u64,
    pub sessions_active: u64,
    /// Attached sessions that declared themselves passive.
    ///
    /// Reported separately from `sessions_attached` so a reader can tell "no Operator is
    /// here" from "the Operator is here and has stepped away", which are very different
    /// situations behind the same socket count.
    pub sessions_passive: u64,
    pub reconnect_attempt: u32,
    pub reconnect_in_flight: u64,
    pub reconnect_waiters: u64,
    /// Delay before the next reconnect attempt, when one is scheduled.
    ///
    /// `None` when nothing is scheduled, which is different from a zero delay and is the
    /// distinction an operator watching a flapping Network needs.
    pub next_retry_delay: Option<Duration>,
    pub upstream_normal_queue_depth: u64,
    pub upstream_control_queue_depth: u64,
    pub desired_reconcile_pending: u64,
    pub response_routes: u64,
    pub open_batches: u64,
    pub history_recorded: u64,
    pub history_skipped: u64,
    /// Lines dropped because the ingestion queue was full.
    ///
    /// The only one of the three that means data was lost. Reported next to its
    /// siblings so "zero recorded" can be distinguished from "everything was dropped".
    pub history_dropped: u64,
    pub upstream_rejected: u64,
    pub fanout_detached: u64,
    pub sessions_accepted: u64,
    pub sessions_ended: u64,
    pub last_session_disposition: Option<&'static str>,
    pub last_error: Option<&'static str>,
    /// A bounded fingerprint of the acknowledged upstream capabilities.
    pub upstream_capabilities: String,
    /// The advertisement this bouncer is currently offering clients.
    pub advertisement_sample: Vec<String>,
    pub advertisement_overflow: u64,
    /// A bounded sample of desired joins the server has not confirmed.
    pub pending_joins_sample: Vec<String>,
    pub pending_joins_overflow: u64,
    /// A bounded sample of joins the server refused, with a fixed reason each.
    pub rejected_joins_sample: Vec<(String, &'static str)>,
    pub rejected_joins_overflow: u64,
}

/// One bounded, secret-free report of the whole process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessDiagnostics {
    /// The controller's revision, so a reader can tell two reports apart.
    pub controller_revision: u64,
    pub networks: Vec<NetworkDiagnostics>,
    /// Current and peak resource gauges.
    pub current: Gauges,
    pub peak: Gauges,
    pub networks_tracked: u64,
    /// Recordings the ledger refused because it was at its Network ceiling.
    pub ledger_refused: u64,
    /// Process-wide connect budget, as the scheduler reports it.
    pub in_flight_connects: u64,
    pub peak_in_flight_connects: u64,
    pub reconnect_waiters: u64,
    /// Store ingress depth, read live.
    pub store_queue_depth: u64,
}

/// Everything the projection needs that does not live on the Network's own snapshot.
pub struct ProjectionInputs<'a> {
    pub display_name: &'a str,
    pub upstream: &'a SchedulerDiagnostics,
    pub resources: &'a ResourceSnapshot,
    pub store_queue_depth: u64,
    pub controller_revision: u64,
}

/// Projects one Network's live snapshot into the bounded report.
///
/// `away_origin` is supplied by the owner rather than derived, because only the owner
/// knows whether *this generation* decided the away state or inherited it.
pub fn project_network(
    snapshot: &NetworkSnapshot,
    away_origin: AwayClass,
    inputs: &ProjectionInputs<'_>,
) -> Option<NetworkDiagnostics> {
    let network = snapshot.network?;
    let (channels_sample, channels_sample_overflow) = sample(&snapshot.channels);
    let (detached_sample, detached_sample_overflow) = sample(&snapshot.detached_channels);
    let (advertisement_sample, advertisement_overflow) = sample(&snapshot.advertisement);
    let (pending_joins_sample, pending_joins_overflow) = sample(&snapshot.pending_joins);
    let rejected_overflow = snapshot
        .rejected_joins
        .len()
        .saturating_sub(MAX_REPORTED_REJECTIONS);
    let rejected_joins_sample = snapshot
        .rejected_joins
        .iter()
        .take(MAX_REPORTED_REJECTIONS)
        .cloned()
        .collect();
    Some(NetworkDiagnostics {
        network,
        display_name: inputs.display_name.to_owned(),
        phase: snapshot.phase.map(|phase| phase.as_str().to_owned()),
        generation: snapshot.generation.map(|generation| generation.0),
        nick: snapshot.nick.clone(),
        away: away_class(snapshot.away.as_deref(), away_origin),
        channels_visible: snapshot.channels.len() as u64,
        channels_detached: snapshot.detached_channels.len() as u64,
        channels_sample,
        channels_sample_overflow: channels_sample_overflow as u64,
        detached_sample,
        detached_sample_overflow: detached_sample_overflow as u64,
        sessions_attached: snapshot.attached_sessions as u64,
        sessions_active: snapshot.active_sessions as u64,
        sessions_passive: (snapshot.attached_sessions as i64 - snapshot.active_sessions as i64)
            .max(0) as u64,
        reconnect_attempt: snapshot.reconnect_attempt,
        reconnect_in_flight: inputs.upstream.in_flight as u64,
        reconnect_waiters: inputs.upstream.pending_waiters as u64,
        next_retry_delay: snapshot.next_retry_delay,
        upstream_normal_queue_depth: snapshot.upstream_normal_queue_depth as u64,
        upstream_control_queue_depth: snapshot.upstream_control_queue_depth as u64,
        desired_reconcile_pending: snapshot.desired_reconcile_pending as u64,
        response_routes: snapshot.response_routes as u64,
        open_batches: snapshot.open_batches as u64,
        history_recorded: snapshot.history_recorded,
        history_skipped: snapshot.history_skipped,
        history_dropped: snapshot.history_dropped,
        upstream_rejected: snapshot.upstream_rejected,
        fanout_detached: snapshot.fanout_detached,
        sessions_accepted: snapshot.sessions_accepted,
        sessions_ended: snapshot.sessions_ended,
        last_session_disposition: snapshot.last_session_disposition,
        last_error: snapshot.last_error,
        upstream_capabilities: snapshot.upstream_capabilities.clone(),
        advertisement_sample,
        advertisement_overflow: advertisement_overflow as u64,
        pending_joins_sample,
        pending_joins_overflow: pending_joins_overflow as u64,
        rejected_joins_sample,
        rejected_joins_overflow: rejected_overflow as u64,
    })
}

/// Projects the whole process.
pub fn project_process(
    networks: Vec<NetworkDiagnostics>,
    inputs: &ProjectionInputs<'_>,
) -> ProcessDiagnostics {
    ProcessDiagnostics {
        controller_revision: inputs.controller_revision,
        networks,
        current: inputs.resources.current,
        peak: inputs.resources.peak,
        networks_tracked: inputs.resources.networks as u64,
        ledger_refused: inputs.resources.refused,
        in_flight_connects: inputs.upstream.in_flight as u64,
        peak_in_flight_connects: inputs.upstream.peak_in_flight as u64,
        reconnect_waiters: inputs.upstream.pending_waiters as u64,
        store_queue_depth: inputs.store_queue_depth,
    }
}

/// The away class actually reported for one Network.
///
/// An away with no recorded origin is [`AwayClass::Unattributed`] rather than guessed at.
/// This generation decided every away it applied, so an unlabelled one is either from
/// before this process started or was observed from upstream -- and both are findings.
fn away_class(away: Option<&str>, origin: AwayClass) -> AwayClass {
    match away {
        None => AwayClass::Present,
        Some(_) => origin,
    }
}

/// A bounded sample of a possibly-unbounded list, plus how many were left out.
fn sample(values: &[String]) -> (Vec<String>, usize) {
    (
        values.iter().take(MAX_REPORTED_CHANNELS).cloned().collect(),
        values.len().saturating_sub(MAX_REPORTED_CHANNELS),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bounded_sample_reports_what_it_left_out() {
        let many: Vec<String> = (0..MAX_REPORTED_CHANNELS + 7)
            .map(|index| format!("#{index}"))
            .collect();
        let (taken, overflow) = sample(&many);
        assert_eq!(taken.len(), MAX_REPORTED_CHANNELS);
        assert_eq!(overflow, 7, "a truncated list must say so");
        let (exact, none) = sample(&taken);
        assert_eq!(none, 0);
        assert_eq!(exact, taken);
    }

    #[test]
    fn an_away_with_no_recorded_origin_is_unattributed() {
        assert_eq!(away_class(None, AwayClass::Manual), AwayClass::Present);
        assert_eq!(
            away_class(Some("lunch"), AwayClass::Manual),
            AwayClass::Manual
        );
        assert_eq!(
            away_class(Some("lunch"), AwayClass::Automatic),
            AwayClass::Automatic
        );
        assert_eq!(
            away_class(Some("lunch"), AwayClass::Unattributed),
            AwayClass::Unattributed,
            "an unlabelled away is a finding, not a default"
        );
    }
}
