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
use crate::presence::AwayOrigin;
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

/// The process-wide inputs the projection needs; every Network reads the same values.
///
/// Owned rather than borrowed: the scheduler and the ledger each produce their reading by
/// value from a lock, and a projection that held a borrow across both locks would be two
/// locks held at once for the sake of a struct that is three numbers wide.
#[derive(Clone, Debug)]
pub struct ProcessInputs {
    pub upstream: SchedulerDiagnostics,
    pub resources: ResourceSnapshot,
    pub store_queue_depth: u64,
    pub controller_revision: u64,
}

/// The [`ProcessDiagnostics`] half of a report, rendered from the live process values.
///
/// Split from the Networks so a caller that renders only the process half does not have to
/// borrow every owner's snapshot to get it.
pub fn project_process(inputs: &ProcessInputs) -> ProcessDiagnostics {
    ProcessDiagnostics {
        controller_revision: inputs.controller_revision,
        networks: Vec::new(),
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

/// Projects one Network's live snapshot into the bounded report.
///
/// `None` for a Network whose owner has not published an identity yet, which a caller must
/// distinguish from an idle Network: the first is "not started", the second is a report.
pub fn project_network(
    snapshot: &NetworkSnapshot,
    display_name: &str,
    inputs: &ProcessInputs,
) -> Option<NetworkDiagnostics> {
    let network = snapshot.network?;
    let (channels_sample, channels_sample_overflow) =
        sample(&snapshot.channels, MAX_REPORTED_CHANNELS);
    let (detached_sample, detached_sample_overflow) =
        sample(&snapshot.detached_channels, MAX_REPORTED_CHANNELS);
    let (advertisement_sample, advertisement_overflow) =
        sample(&snapshot.advertisement, MAX_REPORTED_CAPABILITIES);
    let (pending_joins_sample, pending_joins_overflow) =
        sample(&snapshot.pending_joins, MAX_REPORTED_CHANNELS);
    let rejected_joins_sample = snapshot
        .rejected_joins
        .iter()
        .take(MAX_REPORTED_REJECTIONS)
        .cloned()
        .collect();
    let rejected_overflow = snapshot
        .rejected_joins
        .len()
        .saturating_sub(MAX_REPORTED_REJECTIONS);
    Some(NetworkDiagnostics {
        network,
        display_name: display_name.to_owned(),
        phase: snapshot.phase.map(|phase| phase.as_str().to_owned()),
        generation: snapshot.generation.map(|generation| generation.0),
        nick: snapshot.nick.clone(),
        away: away_class(snapshot.away.as_deref(), snapshot.away_origin),
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

/// The away class actually reported for one Network.
///
/// An away with no recorded origin is [`AwayClass::Unattributed`] rather than guessed at.
/// This generation decided every away it applied, so an unlabelled one is either from
/// before this process started or was observed from upstream -- and both are findings.
fn away_class(away: Option<&str>, origin: Option<AwayOrigin>) -> AwayClass {
    if away.is_none() {
        return AwayClass::Present;
    }
    match origin {
        Some(AwayOrigin::Manual) => AwayClass::Manual,
        Some(AwayOrigin::Automatic) => AwayClass::Automatic,
        None => AwayClass::Unattributed,
    }
}

/// The fixed diagnostic spelling of an away class.
///
/// A closed set, like every other classification in this module: a reader parses it.
impl AwayClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Automatic => "automatic",
            Self::Manual => "manual",
            Self::Unattributed => "unattributed",
        }
    }
}

/// A bounded sample of a possibly-unbounded list, plus how many were left out.
///
/// The ceiling is a parameter rather than a constant because one list length standing for
/// two very different lists -- channel names and capability names -- would mean tightening
/// one silently loosened the other.
fn sample(values: &[String], ceiling: usize) -> (Vec<String>, usize) {
    (
        values.iter().take(ceiling).cloned().collect(),
        values.len().saturating_sub(ceiling),
    )
}

/// Ceiling on lines one `DIAG` reply may contain.
///
/// The binding constraint is not the wire and not taste: a session's *control* queue holds
/// [`crate::CONTROL_QUEUE_CAPACITY`] frames and is written with `try_send`, so a reply
/// longer than that loses its tail silently -- `ControlSurface::write` discards the
/// refusal. A report that arrives half-delivered is worse than a shorter complete one,
/// because an Operator cannot tell which half is missing. Every bound below is chosen so
/// one reply always fits one queue, and a reply that had to leave Networks out says so.
pub const MAX_DIAGNOSTIC_LINES: usize = crate::CONTROL_QUEUE_CAPACITY;

/// Lines the process half occupies in a reply, leaving the rest for Networks.
const PROCESS_DIAGNOSTIC_LINES: usize = 2;

/// Lines one Network occupies in a reply.
const NETWORK_DIAGNOSTIC_LINES: usize = 3;

/// Ceiling on bytes in one rendered diagnostics attribute list.
///
/// Below the wire's 512-byte line ceiling with room for the `NOTICE` prefix and the tag, so
/// a diagnostic can never be the thing that splits a message.
pub const MAX_DIAGNOSTIC_LINE_BYTES: usize = 380;

/// Marker appended to a line whose attribute list was cut to fit.
///
/// Inside the line rather than as a separate frame, because a separate frame is exactly
/// the thing that does not fit. A reader that sees this knows the tail of that line is
/// absent, which is different from the line having had nothing more to say.
const TRUNCATION_MARKER: &str = "truncated=1";

/// One `NOTICE` line: a tag and an attribute list, with no prefix and no terminator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticLine {
    /// The tag carrying the reply, so a client can filter this traffic out.
    pub tag: &'static str,
    /// `key=value` attributes, already bounded and marked if cut.
    pub fields: String,
}

/// Renders the process half, followed by as many Networks as one reply can carry.
///
/// Every line carries [`DIAG_TAG`], including the process half, so a client filters by tag
/// alone and never has to know which half a line came from. When Networks are left out the
/// count is stated on the process line, because "three Networks reported" and "three
/// Networks exist" are different claims.
pub fn render_process(report: &ProcessDiagnostics) -> Vec<DiagnosticLine> {
    let mut lines = vec![
        tagged(
            "process",
            &format!(
                "revision={} networks={} networks_reported={} owner_tasks={} \
                 session_tasks={} history_ingest={} store_queue={} reconnect_waiters={} \
                 in_flight_connects={} peak_in_flight={} refused={}",
                report.controller_revision,
                report.networks_tracked,
                report.networks.len(),
                report.current.owner_tasks,
                report.current.session_tasks,
                report.current.history_ingest,
                report.store_queue_depth,
                report.reconnect_waiters,
                report.in_flight_connects,
                report.peak_in_flight_connects,
                report.ledger_refused,
            ),
        ),
        tagged(
            "peaks",
            &format!(
                "owner_tasks={} session_tasks={} session_normal={} session_control={} \
                 upstream_normal={} upstream_control={} response_routes={} open_batches={} \
                 desired_reconcile={} history_ingest={} store_queue={} reconnect_waiters={} \
                 in_flight_connects={}",
                report.peak.owner_tasks,
                report.peak.session_tasks,
                report.peak.session_normal,
                report.peak.session_control,
                report.peak.upstream_normal,
                report.peak.upstream_control,
                report.peak.response_routes,
                report.peak.open_batches,
                report.peak.desired_reconcile,
                report.peak.history_ingest,
                report.peak.store_queue,
                report.peak.reconnect_waiters,
                report.peak.in_flight_connects,
            ),
        ),
    ];
    // Room is what is left of one reply's queue, divided by what a Network costs. Integer
    // division is the point: a partial Network is not a thing that can be reported.
    let room =
        MAX_DIAGNOSTIC_LINES.saturating_sub(PROCESS_DIAGNOSTIC_LINES) / NETWORK_DIAGNOSTIC_LINES;
    let shown = report.networks.len().min(room);
    for network in report.networks.iter().take(shown) {
        lines.extend(render_network(network));
    }
    let omitted = report.networks.len().saturating_sub(shown);
    if omitted > 0 {
        // Folded into the process line rather than added as a frame: the queue has no room
        // for a frame that exists only to explain frames that are missing.
        lines[0]
            .fields
            .push_str(&format!(" networks_omitted={omitted}"));
    }
    lines.truncate(MAX_DIAGNOSTIC_LINES);
    lines
}

/// Renders one Network as exactly [`NETWORK_DIAGNOSTIC_LINES`] lines.
///
/// Split in two because the whole of one Network does not fit one line, and split along the
/// seam that separates "what state is this in" from "what is in it": an Operator scanning
/// an incident reads the state line of every Network and descends to the detail line only
/// when the state line told them something was wrong.
pub fn render_network(report: &NetworkDiagnostics) -> Vec<DiagnosticLine> {
    let netid = crate::bouncer_networks::render_netid(report.network);
    vec![
        tagged(
            "state",
            &format!(
                "netid={netid} name={} phase={} generation={} away={} attached={} active={} \
                 passive={} attempt={} in_flight={} waiters={} next_retry_delay={} \
                 last_disposition={} last_error={}",
                report.display_name,
                report.phase.as_deref().unwrap_or("unknown"),
                report
                    .generation
                    .map_or_else(|| "none".to_owned(), |generation| generation.to_string()),
                report.away.as_str(),
                report.sessions_attached,
                report.sessions_active,
                report.sessions_passive,
                report.reconnect_attempt,
                report.reconnect_in_flight,
                report.reconnect_waiters,
                report.next_retry_delay.map_or_else(
                    || "none".to_owned(),
                    |delay| format!("{}ms", delay.as_millis())
                ),
                report.last_session_disposition.unwrap_or("none"),
                report.last_error.unwrap_or("none"),
            ),
        ),
        tagged(
            "counts",
            &format!(
                "visible={} detached={} overflow={} detached_overflow={} upstream_normal={} \
                 upstream_control={} desired_pending={} routes={} batches={} recorded={} \
                 skipped={} dropped={} fanout_detached={} accepted={} ended={} rejected={} \
                 pending_joins={} pending_overflow={} rejected_joins={} rejected_overflow={} \
                 advertised={} advertised_overflow={}",
                report.channels_visible,
                report.channels_detached,
                report.channels_sample_overflow,
                report.detached_sample_overflow,
                report.upstream_normal_queue_depth,
                report.upstream_control_queue_depth,
                report.desired_reconcile_pending,
                report.response_routes,
                report.open_batches,
                report.history_recorded,
                report.history_skipped,
                report.history_dropped,
                report.fanout_detached,
                report.sessions_accepted,
                report.sessions_ended,
                report.upstream_rejected,
                report.pending_joins_sample.len(),
                report.pending_joins_overflow,
                report.rejected_joins_sample.len(),
                report.rejected_joins_overflow,
                report.advertisement_sample.len(),
                report.advertisement_overflow,
            ),
        ),
        // The lists get a line of their own rather than the tail of the counts line. Two
        // reasons, and both of them are about not losing the thing a reader opened
        // diagnostics to see: a Network with a hundred joined channels pushes the counts off
        // a shared line, and the counts are the numbers an Operator is actually reading.
        tagged(
            "lists",
            &format!(
                "acknowledged={} sample={} detached_sample={} reasons={}",
                fingerprint_or_none(&report.upstream_capabilities),
                join_or_none(&report.channels_sample),
                join_or_none(&report.detached_sample),
                // Each rejection carries a fixed reason classification and never the
                // server's own text, so the reasons are rendered: an Operator needs to
                // tell "the bouncer is banned" from "the channel is invite-only", and both
                // reasons are already non-secret classifications.
                if report.rejected_joins_sample.is_empty() {
                    "none".to_owned()
                } else {
                    report
                        .rejected_joins_sample
                        .iter()
                        .map(|(channel, reason)| format!("{channel}={reason}"))
                        .collect::<Vec<_>>()
                        .join(",")
                },
            ),
        ),
    ]
}

/// The tag name a rendered diagnostics line carries.
pub const DIAG_TAG: &str = "bouncer-diag";

/// A report line carrying the [`DIAG_TAG`] tag.
///
/// Every diagnostics line is tagged so a client can separate them from conversational and
/// administrative traffic without parsing the payload, and so this reply cannot be
/// mistaken for a `BOUNCER NET` line.
fn tagged(section: &str, fields: &str) -> DiagnosticLine {
    DiagnosticLine {
        tag: DIAG_TAG,
        fields: format!("{section}={}", bounded(fields)),
    }
}

/// Joins a bounded sample, or `none` when it is empty.
fn join_or_none(values: &[String]) -> String {
    if values.is_empty() {
        return "none".to_owned();
    }
    values.join(",")
}

/// The acknowledged-capability fingerprint, or `none`.
fn fingerprint_or_none(fingerprint: &str) -> String {
    if fingerprint.is_empty() {
        return "none".to_owned();
    }
    fingerprint.to_owned()
}

/// Truncates one attribute list to the per-line byte ceiling, marking that it did.
///
/// Truncation rather than refusal: a diagnostic is a best-effort read, and failing the
/// whole report because one attribute was long would make the short attributes -- the ones
/// that are always safe -- unavailable precisely when they are most wanted. The marker
/// reserves its own bytes, so the marker is never the thing that gets cut.
fn bounded(fields: &str) -> String {
    if fields.len() <= MAX_DIAGNOSTIC_LINE_BYTES {
        return fields.to_owned();
    }
    let room = MAX_DIAGNOSTIC_LINE_BYTES - TRUNCATION_MARKER.len() - 1;
    let mut end = room;
    while end > 0 && !fields.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} {TRUNCATION_MARKER}", &fields[..end])
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bounded_sample_reports_what_it_left_out() {
        let many: Vec<String> = (0..MAX_REPORTED_CHANNELS + 7)
            .map(|index| format!("#{index}"))
            .collect();
        let (taken, overflow) = sample(&many, MAX_REPORTED_CHANNELS);
        assert_eq!(taken.len(), MAX_REPORTED_CHANNELS);
        assert_eq!(overflow, 7, "a truncated list must say so");
        let (exact, none) = sample(&taken, MAX_REPORTED_CHANNELS);
        assert_eq!(none, 0);
        assert_eq!(exact, taken);
    }

    #[test]
    fn each_list_honours_its_own_ceiling() {
        let values: Vec<String> = (0..MAX_REPORTED_REJECTIONS)
            .map(|i| i.to_string())
            .collect();
        // A rejection list at its own ceiling of 16 must not be sampled at the channel
        // ceiling of 64, and must not inherit the channel ceiling as its overflow either.
        let (taken, overflow) = sample(&values, MAX_REPORTED_REJECTIONS);
        assert_eq!(taken.len(), MAX_REPORTED_REJECTIONS);
        assert_eq!(overflow, 0);
    }

    #[test]
    fn an_away_with_no_recorded_origin_is_unattributed() {
        assert_eq!(
            away_class(None, Some(AwayOrigin::Manual)),
            AwayClass::Present
        );
        assert_eq!(
            away_class(Some("lunch"), Some(AwayOrigin::Manual)),
            AwayClass::Manual
        );
        assert_eq!(
            away_class(Some("lunch"), Some(AwayOrigin::Automatic)),
            AwayClass::Automatic
        );
        assert_eq!(
            away_class(Some("lunch"), None),
            AwayClass::Unattributed,
            "an unlabelled away is a finding, not a default"
        );
    }

    #[test]
    fn every_away_class_has_a_distinct_fixed_spelling() {
        let all = [
            AwayClass::Present,
            AwayClass::Automatic,
            AwayClass::Manual,
            AwayClass::Unattributed,
        ];
        let mut seen: Vec<&str> = all.iter().map(|class| class.as_str()).collect();
        let count = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            seen.len(),
            count,
            "a reader parses these; they must not collide"
        );
    }

    /// A report with no Networks at all, which is what a fresh process looks like.
    fn empty_process() -> ProcessDiagnostics {
        project_process(&ProcessInputs {
            upstream: SchedulerDiagnostics::default(),
            resources: ResourceSnapshot::default(),
            store_queue_depth: 0,
            controller_revision: 0,
        })
    }

    /// `count` Networks, each joined to `count` channels, which is what makes a line
    /// overflow. Every field the tests do not vary is spelled out rather than defaulted:
    /// a projection with a defaulted field would quietly stop being the thing under test.
    fn one_network(count: usize) -> ProcessDiagnostics {
        let mut process = empty_process();
        process.networks_tracked = count as u64;
        for index in 0..count {
            let channels: Vec<String> = (0..count).map(|n| format!("#room{n}")).collect();
            process.networks.push(NetworkDiagnostics {
                network: NetworkId(index as u64 + 1),
                display_name: format!("net-{index}"),
                phase: Some("online".to_owned()),
                generation: Some(1),
                nick: Some("bot".to_owned()),
                away: AwayClass::Present,
                channels_visible: count as u64,
                channels_detached: 0,
                channels_sample: channels.clone(),
                channels_sample_overflow: 0,
                detached_sample: Vec::new(),
                detached_sample_overflow: 0,
                sessions_attached: 1,
                sessions_active: 1,
                sessions_passive: 0,
                reconnect_attempt: 0,
                reconnect_in_flight: 0,
                reconnect_waiters: 0,
                next_retry_delay: None,
                upstream_normal_queue_depth: 0,
                upstream_control_queue_depth: 0,
                desired_reconcile_pending: 0,
                response_routes: 0,
                open_batches: 0,
                history_recorded: 0,
                history_skipped: 0,
                history_dropped: 0,
                upstream_rejected: 0,
                fanout_detached: 0,
                sessions_accepted: 1,
                sessions_ended: 0,
                last_session_disposition: None,
                last_error: None,
                upstream_capabilities: "ack batch".to_owned(),
                advertisement_sample: Vec::new(),
                advertisement_overflow: 0,
                pending_joins_sample: Vec::new(),
                pending_joins_overflow: 0,
                rejected_joins_sample: Vec::new(),
                rejected_joins_overflow: 0,
            });
        }
        process
    }

    #[test]
    fn a_reply_never_exceeds_the_control_queue_it_is_written_to() {
        // The reason this constant is what it is. A reply that cannot fit would have its
        // tail dropped by `try_send` with nothing written anywhere to say so.
        for networks in [0, 1, 3, 8, 64] {
            let lines = render_process(&one_network(networks));
            assert!(
                lines.len() <= crate::CONTROL_QUEUE_CAPACITY,
                "{networks} networks rendered {} lines into an {}-frame queue",
                lines.len(),
                crate::CONTROL_QUEUE_CAPACITY
            );
        }
    }

    #[test]
    fn networks_left_out_of_a_reply_are_counted_on_the_process_line() {
        let room = (MAX_DIAGNOSTIC_LINES - PROCESS_DIAGNOSTIC_LINES) / NETWORK_DIAGNOSTIC_LINES;
        let lines = render_process(&one_network(room * 3));
        assert_eq!(lines.len(), MAX_DIAGNOSTIC_LINES);
        assert!(
            lines[0]
                .fields
                .contains(&format!("networks_omitted={}", room * 2)),
            "a reply that left Networks out must say so: {}",
            lines[0].fields
        );
        assert!(
            lines[0].fields.contains(&format!("networks={}", room * 3)),
            "and must distinguish how many exist from how many it showed: {}",
            lines[0].fields
        );
    }

    #[test]
    fn a_reply_that_fits_reports_no_omission() {
        let room = (MAX_DIAGNOSTIC_LINES - PROCESS_DIAGNOSTIC_LINES) / NETWORK_DIAGNOSTIC_LINES;
        let lines = render_process(&one_network(room));
        assert_eq!(lines.len(), MAX_DIAGNOSTIC_LINES);
        assert!(
            !lines[0].fields.contains("networks_omitted"),
            "an omission that did not happen must not be reported: {}",
            lines[0].fields
        );
    }

    #[test]
    fn every_line_is_tagged_and_bounded() {
        for line in render_process(&one_network(3)) {
            assert_eq!(line.tag, DIAG_TAG, "one tag for the whole report");
            assert!(
                line.fields.len() <= MAX_DIAGNOSTIC_LINE_BYTES,
                "a line longer than the ceiling would have been split by the wire: {}",
                line.fields
            );
        }
    }

    #[test]
    fn a_line_that_had_to_be_cut_says_so() {
        // The counts precede the lists precisely so that a Network with a thousand channels
        // loses channel names rather than losing the history counters. This test is what
        // stops that ordering from being undone by a later edit.
        let mut report = one_network(1);
        report.networks[0].channels_sample = (0..400)
            .map(|index| format!("#a-very-long-channel-name-{index}"))
            .collect();
        let lines = render_network(&report.networks[0]);
        let detail = lines
            .iter()
            .find(|line| line.fields.starts_with("counts="))
            .expect("the counts line");
        assert!(
            !detail.fields.contains("truncated=1"),
            "every count must survive: {}",
            detail.fields
        );
        let lists = lines
            .iter()
            .find(|line| line.fields.starts_with("lists="))
            .expect("the lists line");
        assert!(
            lists.fields.contains("truncated=1"),
            "the sample was cut and the line must admit it: {}",
            lists.fields
        );
    }

    #[test]
    fn truncation_never_splits_a_character_or_eats_the_marker() {
        // Multi-byte text in a diagnostic is legitimate -- a Network's display name is
        // Operator-chosen -- so the cut has to land on a character boundary.
        let long = "#\u{1f600}".repeat(200);
        let cut = bounded(&long);
        assert!(cut.ends_with(TRUNCATION_MARKER));
        assert!(cut.len() <= MAX_DIAGNOSTIC_LINE_BYTES);
        assert!(long.starts_with(cut.trim_end_matches(TRUNCATION_MARKER).trim_end()));
    }

    #[test]
    fn a_short_line_is_never_marked_as_truncated() {
        assert_eq!(bounded("netid=1 name=short"), "netid=1 name=short");
    }
}
