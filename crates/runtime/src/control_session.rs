//! The control surface one local Operator session talks to.
//!
//! Both wire dialects land here, and neither decides anything on its own:
//!
//! * `soju.im/bouncer-networks` — the interop draft, gated on the capability, with a
//!   deliberately small command vocabulary.
//! * `BouncerServ` — the local administration service, reachable by ordinary `PRIVMSG`.
//!
//! The surface owns a [`RuntimeControlHandle`] and nothing else. It cannot reach a
//! `StoreHandle`, a `SupervisorHandle`, or an owner directly: every mutation it performs
//! is a typed request through the one bounded queue the controller owns. That is what
//! makes "all process mutation flows through the typed controller" a property of the type
//! rather than a review convention.
//!
//! # Authorization
//!
//! This build has one local Operator, and a `ControlSurface` is only ever constructed for
//! a session the local access boundary admitted with a trusted `ClientId`. There is no
//! second role and no per-command authority, and `BouncerServ` is not advertised in `005`
//! or joined anywhere — it exists only on this bouncer's own downstream connections.
//!
//! # Notifications
//!
//! A session that negotiated the notify capability gets a bounded initial batch and then
//! deltas derived from consecutive [`ControlSnapshot`]s. No event log exists: a session
//! that fell behind is reconciled against current state rather than replayed, which is
//! what keeps a slow client's memory bounded however long it stalls.

use crate::action::ActionSet;
use crate::bouncer_networks::{self, BouncerCommand, BouncerError, SERVICE_NICK};
use crate::bouncerserv::{self, ServCommand};
use crate::config_snapshot;
use crate::controller::{ControlSnapshot, RuntimeControlHandle};
use crate::diagnostics;
use crate::session::SessionHandle;
use i2pr_irc_core::NetworkId;
use i2pr_irc_store::NetworkRecord;
use i2pr_irc_wire::Message;

/// Longest accepted help entry. The whole help text is delivered as a bounded batch of
/// one-line `NOTICE`s rather than as one enormous frame a client would have to buffer.
const MAX_HELP_ENTRY_BYTES: usize = 96;

/// One session's control conversation.
pub struct ControlSurface {
    control: RuntimeControlHandle,
    handle: SessionHandle,
    /// The nick this session registered, for replies addressed to it.
    nick: String,
    /// The last revision this session was told about.
    ///
    /// Deltas are computed against this rather than against an event log, so the retained
    /// state is one bounded snapshot however many changes happen.
    ///
    /// This is *notification* state and nothing else. Every command answer reads fresh
    /// from the controller, so a client that creates a Network and immediately lists it
    /// sees the Network it just created rather than the state before it.
    published: ControlSnapshot,
}

impl ControlSurface {
    /// Builds a control surface for one already-registered session.
    pub fn new(control: RuntimeControlHandle, handle: SessionHandle, nick: String) -> Self {
        let published = control.subscribe_status().borrow().clone();
        Self {
            control,
            handle,
            nick,
            published,
        }
    }

    /// Whether this session asked for control-plane change notifications.
    pub fn wants_notifications(&self) -> bool {
        self.handle
            .capabilities()
            .negotiated_bouncer_networks_notify()
    }

    /// Sends the initial bounded Network batch, if the client negotiated the draft.
    pub async fn send_initial_batch(&self) {
        if !self.handle.capabilities().negotiated_bouncer_networks() {
            return;
        }
        let snapshot = self.current_snapshot().await;
        for line in bouncer_networks::render_batch(snapshot.networks.iter(), "") {
            self.write(&line);
        }
    }

    /// Announces every change since the last published revision.
    ///
    /// Driven from the owning task's select loop. A client that stops reading is bounded
    /// by its own control queue; nothing accumulates on the controller's behalf.
    pub async fn publish_changes(&mut self) {
        if !self.wants_notifications() {
            return;
        }
        let snapshot = self.current_snapshot().await;
        for line in bouncer_networks::render_delta(&self.published, &snapshot) {
            self.write(&line);
        }
        self.published = snapshot;
    }

    /// Reads the controller's current snapshot, falling back to the last known one.
    async fn current_snapshot(&self) -> ControlSnapshot {
        self.control
            .status()
            .await
            .unwrap_or_else(|_| self.published.clone())
    }

    /// Handles one client control request.
    pub async fn dispatch(&mut self, wire: &[u8]) {
        let Ok(message) = Message::parse(wire) else {
            return;
        };
        let command = String::from_utf8_lossy(&message.command).to_ascii_uppercase();
        let params: Vec<String> = message
            .params
            .iter()
            .map(|param| String::from_utf8_lossy(param).into_owned())
            .collect();
        match command.as_str() {
            "BOUNCER" => self.dispatch_bouncer(&params).await,
            "PRIVMSG" => self.dispatch_serv(&params).await,
            _ => {}
        }
    }

    /// The bouncer-networks draft's command vocabulary.
    async fn dispatch_bouncer(&mut self, params: &[String]) {
        let subcommand = params
            .first()
            .map(|value| value.to_ascii_uppercase())
            .unwrap_or_default();
        let rest: Vec<&str> = params.iter().skip(1).map(String::as_str).collect();
        let command = match bouncer_networks::decode_command(&subcommand, &rest) {
            Ok(command) => command,
            Err(error) => return self.fail(&subcommand, &error),
        };
        match command {
            // Registration refuses the first `BIND` from ever reaching this surface; this
            // arm exists so a second one is an explicit refusal rather than a silent
            // no-op that would leave the client believing it had joined.
            BouncerCommand::Bind { .. } => self.fail("BIND", &BouncerError::BindTooLate),
            BouncerCommand::ListNetworks => {
                let snapshot = self.current_snapshot().await;
                for line in bouncer_networks::render_batch(snapshot.networks.iter(), "") {
                    self.write(&line);
                }
            }
            BouncerCommand::AddNetwork { fields } => {
                let candidate = match new_record(fields) {
                    Ok(candidate) => candidate,
                    Err(error) => return self.fail(&subcommand, &error),
                };
                match self.control.create_next(candidate).await {
                    Ok(network) => self.notice(&format!(
                        "Added network {}",
                        bouncer_networks::render_netid(network)
                    )),
                    Err(error) => self.fail(&subcommand, &map_control(error)),
                }
            }
            BouncerCommand::ChangeNetwork { network, fields } => {
                let Some(mut record) = self.record(network, &subcommand).await else {
                    return;
                };
                merge_fields(&mut record, fields);
                self.finish_unit(
                    self.control.change(record).await,
                    &subcommand,
                    "Updated network",
                )
            }
            BouncerCommand::DeleteNetwork { network } => {
                if self.require_network(network, &subcommand).await {
                    // `false` means the row was already gone. Either way the Network is
                    // not there afterwards, so the answer is the same.
                    self.finish(
                        self.control.delete(network).await,
                        &subcommand,
                        "Deleted network",
                    )
                }
            }
        }
    }

    /// The local administration service.
    async fn dispatch_serv(&mut self, params: &[String]) {
        // `PRIVMSG <target> :<text>`; anything else is an ordinary message the owner
        // forwards upstream and this surface never sees.
        if params.len() != 2
            || i2pr_irc_core::Casemapping::Rfc1459.fold(params[0].as_bytes())
                != i2pr_irc_core::Casemapping::Rfc1459.fold(SERVICE_NICK.as_bytes())
        {
            return;
        }
        // The verb the client typed, so a refusal names what was refused. `FAIL BOUNCER`
        // with an empty subcommand is technically well-formed and practically useless.
        let verb = params[1]
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        let command = match bouncerserv::parse(&params[1]) {
            Ok(command) => command,
            Err(error) => return self.fail(&verb, &error),
        };
        match command {
            ServCommand::Help => self.reply_help(),
            ServCommand::NetworkList => {
                let snapshot = self.current_snapshot().await;
                for line in bouncer_networks::render_batch(snapshot.networks.iter(), "") {
                    self.write(&line);
                }
            }
            ServCommand::NetworkStatus { network } => match self
                .current_snapshot()
                .await
                .networks
                .iter()
                .find(|entry| entry.network == network)
            {
                Some(entry) => self.notice(&bouncer_networks::render_fields(entry)),
                None => self.fail(&verb, &BouncerError::NoSuchNetwork(network)),
            },
            ServCommand::NetworkCreate { fields } => {
                let candidate = match new_record(fields) {
                    Ok(candidate) => candidate,
                    Err(error) => return self.fail(&verb, &error),
                };
                match self.control.create_next(candidate).await {
                    Ok(network) => self.notice(&format!(
                        "Added network {}",
                        bouncer_networks::render_netid(network)
                    )),
                    Err(error) => self.fail("", &map_control(error)),
                }
            }
            ServCommand::NetworkUpdate { network, fields } => {
                let Some(mut record) = self.record(network, &verb).await else {
                    return;
                };
                merge_fields(&mut record, fields);
                self.finish_unit(self.control.change(record).await, "", "Updated network")
            }
            ServCommand::NetworkDelete { network } => {
                if self.require_network(network, &verb).await {
                    self.finish(self.control.delete(network).await, &verb, "Deleted network")
                }
            }
            ServCommand::ChannelStatus { network, channel } => {
                let Some(record) = self.record(network, &verb).await else {
                    return;
                };
                match record
                    .desired_channels
                    .iter()
                    .find(|entry| entry.target.eq_ignore_ascii_case(&channel))
                {
                    // Per-channel presentation is durable intent, so it is read from the
                    // record rather than from the snapshot, which carries no per-channel
                    // detail on purpose.
                    Some(entry) => self.notice(&format!(
                        "{channel} {}",
                        if entry.detached {
                            "detached"
                        } else {
                            "attached"
                        }
                    )),
                    None => self.fail(&verb, &BouncerError::NoSuchChannel(channel)),
                }
            }
            ServCommand::ChannelDetach { network, channel } => {
                if self.require_network(network, &verb).await {
                    self.finish_unit(
                        self.control
                            .set_channel_detached(network, channel, true)
                            .await,
                        &verb,
                        "Detached channel",
                    )
                }
            }
            ServCommand::ChannelAttach { network, channel } => {
                if self.require_network(network, &verb).await {
                    self.finish_unit(
                        self.control
                            .set_channel_detached(network, channel, false)
                            .await,
                        &verb,
                        "Attached channel",
                    )
                }
            }
            ServCommand::HistoryStatus {
                network,
                kind,
                target,
            } => {
                match self
                    .control
                    .buffer_retention(network, kind, target.clone(), None)
                    .await
                {
                    Ok(policy) => {
                        let name = match policy.policy {
                            Some(i2pr_irc_store::HistoryPrivacyPolicy::Persistent) => "persistent",
                            Some(i2pr_irc_store::HistoryPrivacyPolicy::Ephemeral) => "ephemeral",
                            Some(i2pr_irc_store::HistoryPrivacyPolicy::NoHistory) => "no-history",
                            None => "inherit (persistent default)",
                        };
                        self.notice(&format!("history {target}: {name}"));
                    }
                    Err(error) => self.fail(&verb, &map_control(error)),
                }
            }
            ServCommand::HistorySet {
                network,
                kind,
                target,
                policy,
            } => {
                let requested = i2pr_irc_store::BufferRetentionPolicy {
                    policy,
                    ..Default::default()
                };
                match self
                    .control
                    .buffer_retention(network, kind, target.clone(), Some(Some(requested)))
                    .await
                {
                    Ok(_) => self.notice(&format!("history policy updated for {target}")),
                    Err(error) => self.fail(&verb, &map_control(error)),
                }
            }
            ServCommand::PresenceStatus { network } => {
                self.policy_status(network, "auto_away", &verb).await
            }
            ServCommand::PresenceSet { network, auto_away } => {
                if self.require_network(network, &verb).await {
                    self.finish_unit(
                        self.control
                            .set_presence_policy(network, Some(auto_away), None)
                            .await,
                        &verb,
                        "Updated presence policy",
                    )
                }
            }
            ServCommand::NickStatus { network } => {
                self.policy_status(network, "keep_nick", &verb).await
            }
            ServCommand::NickSet { network, keep_nick } => {
                if self.require_network(network, &verb).await {
                    self.finish_unit(
                        self.control
                            .set_presence_policy(network, None, Some(keep_nick))
                            .await,
                        &verb,
                        "Updated nick policy",
                    )
                }
            }
            ServCommand::SaslStatus { network } => {
                let Some(record) = self.record(network, &verb).await else {
                    return;
                };
                // Whether a credential exists, and whose it is. Never what it is: the
                // value is not in this scope, is not in the reply, and is not reachable
                // from here.
                let configured = if record.sasl.is_some() {
                    "set"
                } else {
                    "unset"
                };
                let who = record
                    .sasl
                    .as_ref()
                    .map(|(user, _)| user.as_str())
                    .unwrap_or("");
                self.notice(format!("sasl {configured} {who}").trim_end());
            }
            ServCommand::SaslSet {
                network,
                username,
                password,
            } => {
                let Some(mut record) = self.record(network, &verb).await else {
                    return;
                };
                record.sasl = Some((username, password.into_stored()));
                // `password` is zeroized when this arm ends. It is never formatted,
                // never logged, and never becomes part of a refusal — a refused `SASL SET`
                // names the command, not the argument that caused the refusal.
                self.finish_unit(self.control.change(record).await, "", "Updated credential")
            }
            ServCommand::SaslReset { network } => {
                let Some(mut record) = self.record(network, &verb).await else {
                    return;
                };
                record.sasl = None;
                self.finish_unit(self.control.change(record).await, "", "Cleared credential")
            }
            ServCommand::Diag => self.report_diagnostics(None).await,
            ServCommand::DiagNetwork { network } => self.report_diagnostics(Some(network)).await,
            ServCommand::ConfigExport => self.export_config().await,
            ServCommand::ConfigPlan => self.plan_config().await,
            ServCommand::ActionStatus { network } => self.action_status(network).await,
            ServCommand::ActionSet {
                network,
                actions,
                replace_phase,
            } => self.set_actions(network, actions, replace_phase).await,
        }
    }

    /// Reports how many registration actions a Network stores.
    ///
    /// A count and nothing else. An action's text is expected to be a service password, and
    /// there is no Operator-facing reason to print it -- `ACTION SET` is how it is written,
    /// and a status answer that echoed it would put it in a terminal scrollback buffer that
    /// survives the session.
    async fn action_status(&mut self, network: NetworkId) {
        if !self.require_network(network, "ACTION").await {
            return;
        }
        match self.control.action_list(network).await {
            Ok(actions) => {
                let pre_join = actions
                    .actions_in_phase(i2pr_irc_store::RegistrationActionPhase::PreJoin)
                    .count();
                let post_join = actions
                    .actions_in_phase(i2pr_irc_store::RegistrationActionPhase::PostJoin)
                    .count();
                let fallback_recovery = actions
                    .actions_in_phase(i2pr_irc_store::RegistrationActionPhase::FallbackRecovery)
                    .count();
                self.notice(&format!(
                    "actions {} pre-join={pre_join} post-join={post_join} fallback-recovery={fallback_recovery}",
                    actions.len()
                ));
            }
            Err(error) => self.fail("ACTION", &map_control(error)),
        }
    }

    /// Replaces a Network's whole registration-action list.
    ///
    /// The parsed, validated, bounded set travels with the command rather than being rebuilt
    /// at dispatch. The control surface is constructed afresh for each request, so there is
    /// nothing to carry it in, and re-parsing here would be a second implementation of the
    /// validation that could disagree with the first.
    ///
    /// An empty set is a real request -- "this Network has no actions" -- and is written
    /// durably as one, so the store is never a Network's real answer while the runtime
    /// believes otherwise.
    async fn set_actions(
        &mut self,
        network: NetworkId,
        actions: ActionSet,
        replace_phase: Option<i2pr_irc_store::RegistrationActionPhase>,
    ) {
        if !self.require_network(network, "ACTION").await {
            return;
        }
        let result = match replace_phase {
            Some(phase) => self.control.set_action_phase(network, phase, actions).await,
            None => self.control.set_actions(network, actions).await,
        };
        match result {
            Ok(count) => self.notice(&format!("Set {count} actions")),
            Err(error) => self.fail("ACTION", &map_control(error)),
        }
    }

    /// Writes the non-secret configuration as a versioned snapshot.
    ///
    /// One `NOTICE` per snapshot line, and never a chunk that spans two of them. The help
    /// text is delivered in arbitrary byte chunks because a human is reading it; a snapshot
    /// is not, and a client cannot reassemble a line it cannot see the boundaries of. An
    /// export that came back as indistinguishable 96-byte fragments would be an export
    /// nobody could paste anywhere.
    ///
    /// Each line is bounded so the `NOTICE` prefix plus the line still fits the wire, which
    /// is why the ceiling lives in [`config_snapshot`] rather than here.
    async fn export_config(&mut self) {
        let snapshot = match self.control.export_config().await {
            Ok(snapshot) => snapshot,
            Err(error) => return self.fail("CONFIG", &map_control(error)),
        };
        for line in config_snapshot::render(&snapshot).lines() {
            self.notice(line);
        }
    }

    /// Validates the live configuration by round-tripping it through the snapshot format.
    ///
    /// `CONFIG PLAN` reports whether the durable configuration still parses and validates as
    /// a snapshot, and what the plan for it would be. It mutates nothing: the point is to
    /// prove the format can represent this bouncer's own state before an Operator relies on
    /// an export, and to surface a durable record that the format cannot express.
    async fn plan_config(&mut self) {
        let snapshot = match self.control.export_config().await {
            Ok(snapshot) => snapshot,
            Err(error) => return self.fail("CONFIG", &map_control(error)),
        };
        let rendered = config_snapshot::render(&snapshot);
        // Re-parsing what was just rendered is the whole test. A renderer that emits
        // something its own parser refuses would be a format that cannot round trip, and
        // that is exactly the defect this command exists to make visible.
        match config_snapshot::parse(&rendered) {
            Ok(back) => {
                let re_rendered = config_snapshot::render(&back);
                if re_rendered == rendered {
                    self.notice(&format!(
                        "config plan ok networks={} bytes={}",
                        snapshot.networks.len(),
                        rendered.len()
                    ));
                } else {
                    self.notice("config plan diverged: the snapshot did not round trip");
                }
            }
            Err(error) => self.notice(&format!("config plan invalid: {error:?}")),
        }
    }

    /// Writes the bounded diagnostics report as tagged `NOTICE` lines.
    ///
    /// Sent as `NOTICE` rather than as `BOUNCER NET` lines on purpose: this reply is a
    /// reading of live state, not an administration result, and reusing the administration
    /// frame would make a client that watches `BOUNCER NET` treat a diagnostic as a network
    /// change. The `bouncer-diag` tag is what actually separates them on the wire, and the
    /// line is truncated upstream in `diagnostics` so it cannot split a message here.
    async fn report_diagnostics(&mut self, selected: Option<NetworkId>) {
        let report = match self.control.diagnostics(selected).await {
            Ok(report) => report,
            Err(error) => {
                let reason = match (error, selected) {
                    // A typo is the overwhelmingly likely cause and deserves a specific
                    // answer; collapsing it into "not persisted" would send the Operator
                    // looking at their storage instead of at their netid.
                    (crate::RuntimeError::UnknownNetwork, Some(network)) => {
                        BouncerError::NoSuchNetwork(network)
                    }
                    (crate::RuntimeError::QueueOverloaded, _) => BouncerError::Overloaded,
                    _ => BouncerError::NotPersisted,
                };
                self.fail("DIAG", &reason);
                return;
            }
        };
        for line in diagnostics::render_process(&report) {
            // The CRLF is part of the frame, not a decoration: `queue_line` refuses any
            // line without one, and `write` discards the refusal, so a reply built without
            // it would vanish without a trace.
            self.write(&format!(
                "@{tag} :{SERVICE_NICK} NOTICE {nick} :{fields}\r\n",
                tag = line.tag,
                nick = self.nick,
                fields = line.fields
            ));
        }
    }

    fn reply_help(&self) {
        for entry in bouncerserv::HELP_TEXT.split('|').map(str::trim) {
            // The parser guarantees an entry fits; the chunking is here so a future
            // longer entry cannot silently produce an oversized frame.
            for chunk in entry.as_bytes().chunks(MAX_HELP_ENTRY_BYTES) {
                self.notice(&String::from_utf8_lossy(chunk));
            }
        }
    }

    async fn policy_status(&mut self, network: NetworkId, field: &str, verb: &str) {
        let Some(record) = self.record(network, verb).await else {
            return;
        };
        let value = match field {
            "auto_away" => record.auto_away,
            _ => record.keep_nick,
        };
        self.notice(&format!("{field}={}", if value { "on" } else { "off" }));
    }

    /// Reports one typed request's outcome.
    ///
    /// The mapping is total: an administrative command never leaves a client without an
    /// answer, and a failure never claims a change happened when the controller does not
    /// know that it did.
    fn finish(&mut self, outcome: Result<bool, crate::RuntimeError>, subcommand: &str, ok: &str) {
        self.finish_inner(outcome.map(|_| ()), subcommand, ok)
    }

    /// The `Result<bool>` deletion answer carries no extra meaning for the client: a row
    /// that was already gone leaves the Network absent either way, so reporting the
    /// difference would tell a client something it cannot act on.
    fn finish_unit(
        &mut self,
        outcome: Result<(), crate::RuntimeError>,
        subcommand: &str,
        ok: &str,
    ) {
        self.finish_inner(outcome, subcommand, ok)
    }

    fn finish_inner(
        &mut self,
        outcome: Result<(), crate::RuntimeError>,
        subcommand: &str,
        ok: &str,
    ) {
        match outcome {
            Ok(()) => self.notice(ok),
            Err(error) => self.fail(subcommand, &map_control(error)),
        }
    }

    /// Confirms a Network exists before a mutation is attempted.
    ///
    /// Checking first turns "no such network" into an explicit `FAIL` rather than an
    /// ambiguous refusal from deeper in the controller.
    async fn require_network(&mut self, network: NetworkId, subcommand: &str) -> bool {
        if self
            .current_snapshot()
            .await
            .networks
            .iter()
            .any(|e| e.network == network)
        {
            return true;
        }
        self.fail(subcommand, &BouncerError::NoSuchNetwork(network));
        false
    }

    /// The complete durable record for one Network, or `None` after answering the client.
    async fn record(&mut self, network: NetworkId, subcommand: &str) -> Option<NetworkRecord> {
        match self.control.network_record(network).await {
            Ok(Some(record)) => Some(record),
            Ok(None) => {
                self.fail(subcommand, &BouncerError::NoSuchNetwork(network));
                None
            }
            Err(error) => {
                self.fail(subcommand, &map_control(error));
                None
            }
        }
    }

    fn notice(&self, text: &str) {
        self.write(&format!(
            ":{SERVICE_NICK} NOTICE {nick} :{text}\r\n",
            nick = self.nick
        ));
    }

    fn fail(&self, subcommand: &str, error: &BouncerError) {
        self.write(&bouncer_networks::render_failure(subcommand, error));
    }

    fn write(&self, line: &str) {
        let _ = self.handle.queue_control(line);
    }
}

/// Builds the complete record for a new Network.
///
/// The identity defaults are fixed strings, never derived from the host, the release, or
/// the Operator's login: a bouncer that named itself after the machine it runs on would
/// publish that machine to every network it joins.
///
/// The zero `NetworkId` is a placeholder the controller replaces: it allocates the real
/// identity, so a client can never choose one and so the canonical netid is assigned in
/// exactly one place.
fn new_record(fields: bouncer_networks::NetworkFields) -> Result<NetworkRecord, BouncerError> {
    // Every creation path already requires a host, so this is unreachable in practice.
    // It is handled as a refusal rather than a panic because a panic in the middle of an
    // administrative request would end a session over a malformed argument.
    let Some(endpoint) = fields.host else {
        return Err(BouncerError::Usage);
    };
    Ok(NetworkRecord {
        network: NetworkId(0),
        display_name: fields
            .name
            .unwrap_or_else(|| i2pr_irc_store::fallback_display_name(NetworkId(0))),
        endpoint,
        nick: fields.nickname.unwrap_or_else(|| "bouncer".to_owned()),
        username: fields.username.unwrap_or_else(|| "bouncer".to_owned()),
        realname: fields.realname.unwrap_or_else(|| "bouncer".to_owned()),
        sasl: None,
        desired_channels: Vec::new(),
        auto_away: false,
        keep_nick: false,
    })
}

/// Applies decoded fields onto an existing record, leaving everything else untouched.
///
/// Everything not named is carried through byte-for-byte, including the credential: an
/// administrative update that quietly dropped a Network's SASL configuration would be a
/// far worse failure than one that refused.
fn merge_fields(record: &mut NetworkRecord, fields: bouncer_networks::NetworkFields) {
    if let Some(name) = fields.name {
        record.display_name = name;
    }
    if let Some(endpoint) = fields.host {
        record.endpoint = endpoint;
    }
    if let Some(nick) = fields.nickname {
        record.nick = nick;
    }
    if let Some(username) = fields.username {
        record.username = username;
    }
    if let Some(realname) = fields.realname {
        record.realname = realname;
    }
}

/// Maps a runtime error onto the bounded client-facing reason set.
fn map_control(error: crate::RuntimeError) -> BouncerError {
    match error {
        crate::RuntimeError::QueueOverloaded => BouncerError::Overloaded,
        // Everything else — a refused commit, an unknown commit, a validation refusal —
        // is reported as "not persisted". The controller never claims a change happened
        // when it does not know that it did, and the client is told the one fact that
        // matters: nothing was confirmed.
        _ => BouncerError::NotPersisted,
    }
}
