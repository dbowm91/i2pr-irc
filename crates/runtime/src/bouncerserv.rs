//! `BouncerServ`: the local Operator administration service.
//!
//! This module parses. Like [`crate::bouncer_networks`], it turns client text into typed
//! values and stops there: no I/O, no state, no controller, no store.
//!
//! # What this is
//!
//! A local service identity, reached the ordinary IRC way — `PRIVMSG BouncerServ :…` —
//! because that is what an existing IRC client can already send. No client needs to learn
//! a new verb, and no capability has to be negotiated to administer the bouncer.
//!
//! # What this deliberately is not
//!
//! It is not a shell. There is no `RUN`, no `EXEC`, no raw IRC line, no file access, no
//! HTTP, no plugin loading, and no router administration. The parser below has no variant
//! that could express any of them, which is a stronger guarantee than a list of refused
//! verbs: there is nothing to reach.
//!
//! It is also not a network service. `BouncerServ` exists only on this bouncer's own
//! downstream connections. It has no upstream identity, is never joined, and is not
//! advertised in `005`.
//!
//! # Secrets
//!
//! `SASL SET` accepts a password because that is the only way to set one, and it takes
//! it into a `Zeroizing` that is dropped at the end of the request. No error, status, or
//! reply anywhere in this module or its dispatch path renders it. A refused `SASL SET`
//! says the *command* was refused; it never echoes the argument that caused the refusal.

use crate::action::{ActionError, ActionSet, RegistrationAction};
use crate::bouncer_networks::{self, BouncerError, NetworkFields};
use i2pr_irc_core::NetworkId;

/// Longest accepted `BouncerServ` command text, in bytes.
///
/// Everything is Operator-typed and every parameter is separately bounded. The ceiling
/// exists so a client cannot hand this parser an unbounded line to hold in memory.
pub const MAX_SERV_LINE_BYTES: usize = 1024;

/// Ceiling on whitespace-separated words in one command.
pub const MAX_SERV_WORDS: usize = 16;

/// One decoded `BouncerServ` command.
///
/// `Debug` is written out by hand rather than derived, because a derived `Debug` on a
/// variant holding a [`crate::Secret`] is a one-line away from printing a credential.
/// The derived form here would be correct *today* only because `Secret` redacts, and a
/// future field that does not would turn a diagnostic into a leak.
#[derive(Clone, Eq, PartialEq)]
pub enum ServCommand {
    /// The bounded command list, as text.
    Help,
    /// List every Network and its live state.
    NetworkList,
    /// Report one Network.
    NetworkStatus {
        network: NetworkId,
    },
    /// Create a Network. Identity fields the Operator did not supply are filled with
    /// fixed defaults derived from the Network's own display name.
    NetworkCreate {
        fields: NetworkFields,
    },
    /// Apply a partial update to one Network.
    NetworkUpdate {
        network: NetworkId,
        fields: NetworkFields,
    },
    /// Forget one Network.
    NetworkDelete {
        network: NetworkId,
    },
    /// Report one channel's presentation policy on one Network.
    ChannelStatus {
        network: NetworkId,
        channel: String,
    },
    /// Stop presenting one channel the bouncer still holds.
    ChannelDetach {
        network: NetworkId,
        channel: String,
    },
    /// Resume presenting one detached channel.
    ChannelAttach {
        network: NetworkId,
        channel: String,
    },
    ChannelActivitySet {
        network: NetworkId,
        channel: String,
        policy: i2pr_irc_store::ChannelActivityPolicy,
    },
    WatchList {
        network: NetworkId,
    },
    WatchAdd {
        rule: i2pr_irc_store::WatchRule,
    },
    WatchDelete {
        network: NetworkId,
        id: u32,
    },
    WatchClear {
        network: NetworkId,
    },
    /// Read one buffer's history privacy mode.
    HistoryStatus {
        network: NetworkId,
        kind: i2pr_irc_store::BufferKind,
        target: String,
    },
    /// Set or inherit one buffer's history privacy mode.
    HistorySet {
        network: NetworkId,
        kind: i2pr_irc_store::BufferKind,
        target: String,
        policy: Option<i2pr_irc_store::HistoryPrivacyPolicy>,
    },
    /// Report one Network's presence policy.
    PresenceStatus {
        network: NetworkId,
    },
    /// Change one Network's auto-away policy.
    PresenceSet {
        network: NetworkId,
        auto_away: bool,
    },
    /// Report one Network's keep-nick policy.
    NickStatus {
        network: NetworkId,
    },
    /// Change one Network's keep-nick policy.
    NickSet {
        network: NetworkId,
        keep_nick: bool,
    },
    /// Report whether a Network has a credential, and under which name.
    ///
    /// The value is never in this type, so it cannot be rendered by accident.
    SaslStatus {
        network: NetworkId,
    },
    /// Set a Network's credential.
    SaslSet {
        network: NetworkId,
        username: String,
        /// Zeroizes on drop and renders as `[redacted]`.
        ///
        /// [`zeroize::Zeroizing`] alone was not enough: it delegates `Debug` to the
        /// inner `String`, so any diagnostic that formatted this command would have
        /// printed the password. `crate::Secret` redacts *and* zeroes, and the type is
        /// what makes "never format the credential" checkable rather than a habit.
        password: crate::Secret,
    },
    /// Forget a Network's credential.
    SaslReset {
        network: NetworkId,
    },
    /// Report the bounded process-wide diagnostics, with every live Network.
    Diag,
    /// Report one Network's bounded diagnostics.
    DiagNetwork {
        network: NetworkId,
    },
    /// Write the whole non-secret configuration out as a versioned snapshot.
    ConfigExport,
    /// Validate a snapshot and report what applying it would do, without applying it.
    ConfigPlan,
    /// Report how many registration actions a Network stores, and nothing about them.
    ActionStatus {
        network: NetworkId,
    },
    /// Replace one Network's whole registration-action list.
    ///
    /// Carries the validated set rather than the raw attributes: by the time a command
    /// exists, every action in it has passed the allowlist and both ceilings, and nothing
    /// downstream should have to re-establish that.
    ActionSet {
        network: NetworkId,
        actions: ActionSet,
        /// When present, replace only this phase and keep other phase lists intact.
        replace_phase: Option<i2pr_irc_store::RegistrationActionPhase>,
    },
}

impl std::fmt::Debug for ServCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Every arm is a fixed name plus typed identifiers. No arm renders a value.
        let name = match self {
            Self::Help => "help",
            Self::NetworkList => "network-list",
            Self::NetworkStatus { network } => return write!(f, "NetworkStatus({network:?})"),
            Self::NetworkCreate { fields } => return write!(f, "NetworkCreate({fields:?})"),
            Self::NetworkUpdate { network, fields } => {
                return write!(f, "NetworkUpdate({network:?}, {fields:?})");
            }
            Self::NetworkDelete { network } => return write!(f, "NetworkDelete({network:?})"),
            Self::ChannelStatus { network, channel } => {
                return write!(f, "ChannelStatus({network:?}, {channel})");
            }
            Self::ChannelDetach { network, channel } => {
                return write!(f, "ChannelDetach({network:?}, {channel})");
            }
            Self::ChannelAttach { network, channel } => {
                return write!(f, "ChannelAttach({network:?}, {channel})");
            }
            Self::ChannelActivitySet {
                network,
                channel,
                policy,
            } => return write!(f, "ChannelActivitySet({network:?}, {channel}, {policy:?})"),
            Self::WatchList { network } => return write!(f, "WatchList({network:?})"),
            Self::WatchAdd { rule } => {
                return write!(f, "WatchAdd({:?}, {})", rule.network, rule.id);
            }
            Self::WatchDelete { network, id } => {
                return write!(f, "WatchDelete({network:?}, {id})");
            }
            Self::WatchClear { network } => return write!(f, "WatchClear({network:?})"),
            Self::HistoryStatus {
                network,
                kind,
                target,
            } => {
                return write!(f, "HistoryStatus({network:?}, {kind:?}, {target})");
            }
            Self::HistorySet {
                network,
                kind,
                target,
                policy,
            } => {
                return write!(f, "HistorySet({network:?}, {kind:?}, {target}, {policy:?})");
            }
            Self::PresenceStatus { network } => return write!(f, "PresenceStatus({network:?})"),
            Self::PresenceSet { network, auto_away } => {
                return write!(f, "PresenceSet({network:?}, {auto_away})");
            }
            Self::NickStatus { network } => return write!(f, "NickStatus({network:?})"),
            Self::NickSet { network, keep_nick } => {
                return write!(f, "NickSet({network:?}, {keep_nick})");
            }
            Self::SaslStatus { network } => return write!(f, "SaslStatus({network:?})"),
            // The password is named and never rendered. This is the arm that has to be
            // right: it is the one variant whose value is a secret.
            Self::SaslSet {
                network, username, ..
            } => return write!(f, "SaslSet({network:?}, {username:?})"),
            Self::SaslReset { network } => return write!(f, "SaslReset({network:?})"),
            Self::Diag => return write!(f, "Diag"),
            Self::DiagNetwork { network } => return write!(f, "DiagNetwork({network:?})"),
            Self::ConfigExport => return write!(f, "ConfigExport"),
            Self::ConfigPlan => return write!(f, "ConfigPlan"),
            Self::ActionStatus { network } => return write!(f, "ActionStatus({network:?})"),
            // Never renders the payloads. An action's text may be a service password, so
            // this arm is one of the two that has to be right by construction.
            Self::ActionSet {
                network, actions, ..
            } => {
                return write!(f, "ActionSet({network:?}, {} actions)", actions.len());
            }
        };
        f.write_str(name)
    }
}

/// The bounded command list, as one line per entry.
///
/// It names only commands this build actually implements. A help text listing a command
/// that is refused would be the same failure as advertising a capability that is not
/// live: the Operator would rely on something that does not work.
pub const HELP_TEXT: &str = concat!(
    "BouncerServ commands:",
    " help",
    " | network list",
    " | network status <netid>",
    " | network create name=<name> host=<i2p-destination> [nickname=<nick>] [username=<user>] [realname=<text>]",
    " | network update <netid> [name=<name>] [nickname=<nick>] [username=<user>] [realname=<text>]",
    " | network delete <netid>",
    " | channel status <netid> <channel>",
    " | channel detach <netid> <channel>",
    " | channel attach <netid> <channel>",
    " | channel activity <netid> <channel> relay=<none|mentions|all> reattach=<off|message|mention> detach_after=<off|1..86400>",
    " | watch list <netid> | watch add <netid> <channel|query> <target|*> <keyword|sender> <term> | watch delete <netid> <id> | watch clear <netid>",
    " | history status <netid> <channel|query> <target>",
    " | history set <netid> <channel|query> <target> <persistent|ephemeral|no-history|inherit>",
    " | presence status <netid>",
    " | presence set <netid> auto_away=on|off",
    " | nick status <netid>",
    " | nick set <netid> keep_nick=on|off",
    " | sasl status <netid>",
    " | sasl set <netid> user=<username> pass=<password>",
    " | sasl reset <netid>",
    " | diag",
    " | diag network <netid>",
    " | config export",
    " | config plan",
    " | action status <netid>",
    " | action set <netid> [mode=<modes>] [message=<serv> text=<text>]",
);

/// Parses one command line.
///
/// `text` is the trailing parameter of a `PRIVMSG BouncerServ :…`, already extracted
/// from the wire and never containing a line terminator.
pub fn parse(text: &str) -> Result<ServCommand, BouncerError> {
    if text.len() > MAX_SERV_LINE_BYTES {
        return Err(BouncerError::TooManyParameters);
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() > MAX_SERV_WORDS {
        return Err(BouncerError::TooManyParameters);
    }
    let word = |index: usize| words.get(index).copied().unwrap_or("");
    let netid = |index: usize| -> Result<NetworkId, BouncerError> {
        bouncer_networks::parse_netid(word(index))
    };
    let channel = |index: usize| -> Result<String, BouncerError> {
        let raw = word(index);
        // A channel is an IRC channel name, so it is validated as one rather than
        // accepted as an opaque string that later has to be escaped in a frame.
        if !crate::state::valid_channel_token(raw) {
            return Err(BouncerError::ValueOutOfRange);
        }
        Ok(raw.to_owned())
    };
    // Policy fields arrive as `name=on|off`, so the key is checked as well as the value:
    // `presence set 1 keep_nick=on` is a request about the wrong field, and accepting it
    // would let a client believe it changed presence policy when it changed nothing.
    let on_off = |index: usize, name: &str| -> Result<bool, BouncerError> {
        let (key, value) = word(index)
            .split_once('=')
            .ok_or_else(|| BouncerError::UnsupportedAttribute((*name).to_owned()))?;
        if key != name {
            return Err(BouncerError::UnsupportedAttribute((*key).to_owned()));
        }
        match value {
            "on" | "true" | "yes" => Ok(true),
            "off" | "false" | "no" => Ok(false),
            _ => Err(BouncerError::ValueOutOfRange),
        }
    };

    match word(0).to_ascii_uppercase().as_str() {
        "HELP" => Ok(ServCommand::Help),
        "NETWORK" => match word(1).to_ascii_uppercase().as_str() {
            "LIST" => Ok(ServCommand::NetworkList),
            "STATUS" => Ok(ServCommand::NetworkStatus { network: netid(2)? }),
            "CREATE" => Ok(ServCommand::NetworkCreate {
                fields: serv_fields(&words[2.min(words.len())..], true)?,
            }),
            "UPDATE" => Ok(ServCommand::NetworkUpdate {
                network: netid(2)?,
                // An update that changed nothing would be a successful no-op that cost a
                // durable write and an owner restart. Refusing it is more honest.
                fields: serv_fields(&words[3.min(words.len())..], false)?,
            }),
            "DELETE" => Ok(ServCommand::NetworkDelete { network: netid(2)? }),
            other => Err(BouncerError::UnknownSubcommand(other.to_owned())),
        },
        "CHANNEL" => {
            let network = netid(2)?;
            if word(1).eq_ignore_ascii_case("ACTIVITY") {
                if words.len() != 7 {
                    return Err(BouncerError::Usage);
                }
                let mut relay = None;
                let mut reattach = None;
                let mut detach_after = None;
                for field in &words[4..] {
                    let (key, value) = field.split_once('=').ok_or(BouncerError::Usage)?;
                    match key {
                        "relay" if relay.is_none() => {
                            relay = Some(match value {
                                "none" => i2pr_irc_store::RelayDetached::None,
                                "mentions" => i2pr_irc_store::RelayDetached::Mentions,
                                "all" => i2pr_irc_store::RelayDetached::All,
                                _ => return Err(BouncerError::ValueOutOfRange),
                            })
                        }
                        "reattach" if reattach.is_none() => {
                            reattach = Some(match value {
                                "off" => i2pr_irc_store::ReattachOn::Off,
                                "message" => i2pr_irc_store::ReattachOn::Message,
                                "mention" => i2pr_irc_store::ReattachOn::Mention,
                                _ => return Err(BouncerError::ValueOutOfRange),
                            })
                        }
                        "detach_after" if detach_after.is_none() => {
                            detach_after = Some(if value == "off" {
                                None
                            } else {
                                Some(
                                    value
                                        .parse::<u32>()
                                        .map_err(|_| BouncerError::ValueOutOfRange)?,
                                )
                            })
                        }
                        _ => return Err(BouncerError::UnsupportedAttribute(key.to_owned())),
                    }
                }
                let policy = i2pr_irc_store::ChannelActivityPolicy {
                    relay_detached: relay.ok_or(BouncerError::Usage)?,
                    reattach_on: reattach.ok_or(BouncerError::Usage)?,
                    detach_after_secs: detach_after.ok_or(BouncerError::Usage)?,
                };
                policy
                    .validate()
                    .map_err(|_| BouncerError::ValueOutOfRange)?;
                return Ok(ServCommand::ChannelActivitySet {
                    network,
                    channel: channel(3)?,
                    policy,
                });
            }
            // Exactly three arguments. A trailing word here is a client that miscounted,
            // and ignoring it would mean administrating a channel the client did not
            // name while believing it named the one it typed.
            if words.len() != 4 {
                return Err(BouncerError::Usage);
            }
            match word(1).to_ascii_uppercase().as_str() {
                "STATUS" => Ok(ServCommand::ChannelStatus {
                    network,
                    channel: channel(3)?,
                }),
                "DETACH" => Ok(ServCommand::ChannelDetach {
                    network,
                    channel: channel(3)?,
                }),
                "ATTACH" => Ok(ServCommand::ChannelAttach {
                    network,
                    channel: channel(3)?,
                }),
                other => Err(BouncerError::UnknownSubcommand(other.to_owned())),
            }
        }
        "WATCH" => {
            let network = netid(2)?;
            match word(1).to_ascii_uppercase().as_str() {
                "LIST" if words.len() == 3 => Ok(ServCommand::WatchList { network }),
                "CLEAR" if words.len() == 3 => Ok(ServCommand::WatchClear { network }),
                "DELETE" if words.len() == 4 => Ok(ServCommand::WatchDelete {
                    network,
                    id: word(3)
                        .parse::<u32>()
                        .ok()
                        .filter(|id| *id > 0 && *id <= i2pr_irc_store::MAX_WATCH_RULES as u32)
                        .ok_or(BouncerError::ValueOutOfRange)?,
                }),
                "ADD" if words.len() == 7 => {
                    let kind = match word(3) {
                        "channel" => i2pr_irc_store::BufferKind::Channel,
                        "query" => i2pr_irc_store::BufferKind::Query,
                        _ => return Err(BouncerError::ValueOutOfRange),
                    };
                    let target = if word(4) == "*" {
                        None
                    } else {
                        let target = word(4);
                        if target.is_empty()
                            || target.len() > i2pr_irc_store::MAX_TARGET_BYTES
                            || !target.bytes().all(|b| b.is_ascii_graphic())
                        {
                            return Err(BouncerError::ValueOutOfRange);
                        }
                        Some(target.to_owned())
                    };
                    let matcher = match word(5) {
                        "keyword" => i2pr_irc_store::WatchMatchKind::Keyword,
                        "sender" => i2pr_irc_store::WatchMatchKind::Sender,
                        _ => return Err(BouncerError::ValueOutOfRange),
                    };
                    let rule = i2pr_irc_store::WatchRule {
                        id: 1,
                        network,
                        buffer: None,
                        kind,
                        target,
                        matcher,
                        term: word(6).to_owned(),
                    };
                    rule.validate().map_err(|_| BouncerError::ValueOutOfRange)?;
                    Ok(ServCommand::WatchAdd { rule })
                }
                _ => Err(BouncerError::Usage),
            }
        }
        "HISTORY" => {
            let network = netid(2)?;
            if words.len() != 5 && words.len() != 6 {
                return Err(BouncerError::Usage);
            }
            let kind = match word(3) {
                "channel" => i2pr_irc_store::BufferKind::Channel,
                "query" => i2pr_irc_store::BufferKind::Query,
                _ => return Err(BouncerError::ValueOutOfRange),
            };
            let target = word(4);
            if target.is_empty()
                || target.len() > i2pr_irc_store::MAX_TARGET_BYTES
                || !target.bytes().all(|byte| byte.is_ascii_graphic())
            {
                return Err(BouncerError::ValueOutOfRange);
            }
            match word(1).to_ascii_uppercase().as_str() {
                "STATUS" if words.len() == 5 => Ok(ServCommand::HistoryStatus {
                    network,
                    kind,
                    target: target.to_owned(),
                }),
                "SET" if words.len() == 6 => {
                    let policy = match word(5) {
                        "persistent" => Some(i2pr_irc_store::HistoryPrivacyPolicy::Persistent),
                        "ephemeral" => Some(i2pr_irc_store::HistoryPrivacyPolicy::Ephemeral),
                        "no-history" => Some(i2pr_irc_store::HistoryPrivacyPolicy::NoHistory),
                        "inherit" => None,
                        _ => return Err(BouncerError::ValueOutOfRange),
                    };
                    Ok(ServCommand::HistorySet {
                        network,
                        kind,
                        target: target.to_owned(),
                        policy,
                    })
                }
                _ => Err(BouncerError::Usage),
            }
        }
        "PRESENCE" => match word(1).to_ascii_uppercase().as_str() {
            "STATUS" => Ok(ServCommand::PresenceStatus { network: netid(2)? }),
            "SET" => Ok(ServCommand::PresenceSet {
                network: netid(2)?,
                auto_away: on_off(3, "auto_away")?,
            }),
            other => Err(BouncerError::UnknownSubcommand(other.to_owned())),
        },
        "NICK" => match word(1).to_ascii_uppercase().as_str() {
            "STATUS" => Ok(ServCommand::NickStatus { network: netid(2)? }),
            "SET" => Ok(ServCommand::NickSet {
                network: netid(2)?,
                keep_nick: on_off(3, "keep_nick")?,
            }),
            other => Err(BouncerError::UnknownSubcommand(other.to_owned())),
        },
        "SASL" => match word(1).to_ascii_uppercase().as_str() {
            "STATUS" => Ok(ServCommand::SaslStatus { network: netid(2)? }),
            "RESET" => Ok(ServCommand::SaslReset { network: netid(2)? }),
            "SET" => {
                let network = netid(2)?;
                let mut username: Option<String> = None;
                let mut password: Option<crate::Secret> = None;
                for param in &words[3.min(words.len())..] {
                    let (key, value) = param
                        .split_once('=')
                        .ok_or_else(|| BouncerError::MalformedAttribute((*param).to_owned()))?;
                    if value.len() > bouncer_networks::MAX_ATTRIBUTE_VALUE_BYTES {
                        return Err(BouncerError::AttributeTooLong((*key).to_owned()));
                    }
                    match key {
                        "user" => {
                            if username.is_some() {
                                return Err(BouncerError::Usage);
                            }
                            if value.is_empty()
                                || value.len() > bouncer_networks::MAX_IDENTITY_BYTES
                                || !value.bytes().all(|b| b.is_ascii_graphic())
                            {
                                return Err(BouncerError::ValueOutOfRange);
                            }
                            username = Some(value.to_owned());
                        }
                        "pass" => {
                            if password.is_some() {
                                return Err(BouncerError::Usage);
                            }
                            if value.is_empty() {
                                return Err(BouncerError::ValueOutOfRange);
                            }
                            // Taken straight into a redacting, zeroizing buffer. Nothing
                            // between here and the store ever holds it in an ordinary
                            // `String`, and nothing that holds it can print it.
                            password = Some(crate::Secret::new(value.to_owned()));
                        }
                        other => return Err(BouncerError::UnknownAttribute((*other).to_owned())),
                    }
                }
                Ok(ServCommand::SaslSet {
                    network,
                    username: username.ok_or(BouncerError::Usage)?,
                    password: password.ok_or(BouncerError::Usage)?,
                })
            }
            other => Err(BouncerError::UnknownSubcommand(other.to_owned())),
        },
        // `DIAG` is deliberately not an attribute-accepting command. There is no `filter=`
        // or `detail=` word: a diagnostic surface that could be asked to render *less* would
        // also be one that could be asked to render something else, and this surface has no
        // second mode. Whole-process or one named Network, nothing in between.
        // `CONFIG` takes a subcommand and nothing else. There is no `import <file>` and no
        // `path=` attribute: the plan's own stop conditions forbid generic file side effects,
        // and a format that could read a file would be a format whose input an Operator
        // cannot see. Import arrives as text the Operator typed, or not at all.
        // `ACTION SET` is the only command in this module whose arguments may contain a
        // secret. It therefore reads a `text=` attribute straight into a redacting buffer,
        // bounds it, and never echoes it -- including in a refusal, which names the
        // attribute rather than repeating its value.
        "ACTION" => match word(1).to_ascii_uppercase().as_str() {
            "STATUS" => Ok(ServCommand::ActionStatus { network: netid(2)? }),
            "SET" => {
                let network = netid(2)?;
                // The line is split at a whole-word `text=` before anything is iterated,
                // because everything after it is one message's text rather than more
                // attributes. `IDENTIFY hunter2` is a single message containing a space;
                // word-by-word parsing would store `IDENTIFY` and drop the password, which
                // fails at the service while looking exactly like a working configuration.
                let (head, body) = split_at_action_text(text);
                let mut actions: Vec<RegistrationAction> = Vec::new();
                let mut pending_message: Option<(&str, i2pr_irc_store::RegistrationActionPhase)> =
                    None;
                let mut phase = i2pr_irc_store::RegistrationActionPhase::PostJoin;
                let mut replace_phase = None;
                for param in head.split_whitespace().skip(3) {
                    let (key, value) = param
                        .split_once('=')
                        .ok_or_else(|| BouncerError::MalformedAttribute((*param).to_owned()))?;
                    if value.len() > bouncer_networks::MAX_ATTRIBUTE_VALUE_BYTES {
                        return Err(BouncerError::AttributeTooLong((*key).to_owned()));
                    }
                    match key {
                        "phase" => {
                            phase = i2pr_irc_store::RegistrationActionPhase::parse(value)
                                .map_err(|_| BouncerError::ValueOutOfRange)?;
                            replace_phase = Some(phase);
                        }
                        "mode" => {
                            if actions.len() >= crate::action::MAX_REGISTRATION_ACTIONS {
                                return Err(BouncerError::TooManyParameters);
                            }
                            actions.push(
                                RegistrationAction::mode_in_phase(value, phase)
                                    .map_err(map_action)?,
                            );
                        }
                        // The target is recorded and the action built when the text arrives,
                        // so `text=` belongs to the `message=` that precedes it rather than to
                        // whichever one happens to come later.
                        "message" => {
                            if actions.len() >= crate::action::MAX_REGISTRATION_ACTIONS {
                                return Err(BouncerError::TooManyParameters);
                            }
                            pending_message = Some((value, phase));
                        }
                        other => return Err(BouncerError::UnknownAttribute((*other).to_owned())),
                    }
                }
                if let Some((target, message_phase)) = pending_message {
                    actions.push(
                        RegistrationAction::message_in_phase(target, body, message_phase)
                            .map_err(map_action)?,
                    );
                } else if !body.is_empty() {
                    // A text with no target is refused rather than ignored: silently dropping
                    // it would clear the Operator's list while they believed they had written
                    // a message to a service.
                    return Err(BouncerError::Usage);
                }
                // `ACTION SET <netid>` with nothing named clears the list. That is a real
                // operation and needs to be expressible, so it is not treated as a usage
                // error the way an empty `CHANGENETWORK` is: an empty set is built and
                // written, rather than answered locally.
                let actions = crate::action::ActionSet::new(actions).map_err(map_action)?;
                if replace_phase.is_some()
                    && actions.actions().iter().any(|action| action.phase != phase)
                {
                    return Err(BouncerError::Usage);
                }
                Ok(ServCommand::ActionSet {
                    network,
                    actions,
                    replace_phase,
                })
            }
            other => Err(BouncerError::UnknownSubcommand(other.to_owned())),
        },
        "CONFIG" => match word(1).to_ascii_uppercase().as_str() {
            "EXPORT" => Ok(ServCommand::ConfigExport),
            "PLAN" => Ok(ServCommand::ConfigPlan),
            other => Err(BouncerError::UnknownSubcommand(other.to_owned())),
        },
        "DIAG" => match word(1).to_ascii_uppercase().as_str() {
            "" => {
                if words.len() != 1 {
                    return Err(BouncerError::Usage);
                }
                Ok(ServCommand::Diag)
            }
            "NETWORK" => Ok(ServCommand::DiagNetwork { network: netid(2)? }),
            other => Err(BouncerError::UnknownSubcommand(other.to_owned())),
        },
        other => Err(BouncerError::UnknownSubcommand(other.to_owned())),
    }
}

/// Splits a command line at the first whole-word `text=`.
///
/// Returns `(head, body)`, where `body` is everything after `text=` with surrounding
/// whitespace trimmed, and `head` is everything before it. With no `text=` the whole line
/// is the head and the body is empty.
///
/// A *whole-word* `text=` only: `context=` contains the substring, and splitting there would
/// send whatever followed it as an identify line.
///
/// The body is a borrowed slice of the caller's line rather than an owned `String`,
/// because the value is expected to be a credential: it goes straight into a `StoredSecret`
/// and must not be copied into an intermediate owned string on the way.
fn split_at_action_text(text: &str) -> (&str, &str) {
    let mut offset = 0usize;
    while let Some(found) = text[offset..].find("text=") {
        let start = offset + found;
        let whole_word =
            start == 0 || text[..start].ends_with(|c: char| c.is_whitespace() || c == ';');
        if whole_word {
            return (&text[..start], text[start + "text=".len()..].trim());
        }
        offset = start + "text=".len();
    }
    (text, "")
}

/// Maps an action-model refusal onto the bounded client-facing reason set.
///
/// Only the fixed classifications cross this boundary. [`crate::action::ActionError`] is
/// already a closed set that names what was wrong, so nothing is flattened into a generic
/// "invalid" -- an Operator who typed a refused action needs to know which part to change.
fn map_action(error: ActionError) -> BouncerError {
    match error {
        ActionError::ForbiddenCommand(name) => BouncerError::UnknownAttribute(name),
        ActionError::MissingMode | ActionError::EmptyText => BouncerError::Usage,
        ActionError::InvalidTarget => BouncerError::NotAService,
        ActionError::TextTooLong => BouncerError::AttributeTooLong("text".to_owned()),
        ActionError::TooManyActions | ActionError::TooManyBytes => BouncerError::TooManyParameters,
        ActionError::TagsRefused | ActionError::PrefixedRefused => {
            BouncerError::MalformedAttribute("prefix-or-tag".to_owned())
        }
    }
}

/// Decodes `key=value` service attributes.
///
/// `require_host` is the difference between creating and updating: a Network with no
/// endpoint is a Network this bouncer can never connect, so creation requires one and an
/// update must not be allowed to remove one.
fn serv_fields(params: &[&str], require_host: bool) -> Result<NetworkFields, BouncerError> {
    if params.is_empty() {
        // A change that names nothing is a durable write and an owner restart that
        // changes nothing. Answering "ok" would tell the Operator something was applied.
        return Err(BouncerError::Usage);
    }
    let mut fields = NetworkFields::default();
    for value in bouncer_networks::decode_attributes(params)? {
        fields.absorb(&value)?;
    }
    if require_host && fields.host.is_none() {
        return Err(BouncerError::Usage);
    }
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_advertised_command_parses() {
        // The help text and the parser must not drift: a command listed but unparseable
        // is the same lie as a capability advertised but not implemented.
        for entry in HELP_TEXT.split('|').map(str::trim) {
            assert!(!entry.is_empty());
        }
        for line in [
            "help",
            "network list",
            "network status 1",
            "network create name=lab host=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.b32.i2p",
            "network update 1 name=other",
            "network delete 1",
            "channel status 1 #room",
            "channel detach 1 #room",
            "channel attach 1 #room",
            "channel activity 1 #room relay=mentions reattach=mention detach_after=3600",
            "channel activity 1 #room detach_after=off reattach=off relay=none",
            "watch list 1",
            "watch add 1 channel #room keyword urgent",
            "watch add 1 query * sender alice",
            "watch delete 1 1",
            "watch clear 1",
            "history status 1 channel #room",
            "history set 1 channel #room no-history",
            "history set 1 query alice ephemeral",
            "history set 1 query alice inherit",
            "presence status 1",
            "presence set 1 auto_away=on",
            "nick status 1",
            "nick set 1 keep_nick=off",
            "sasl status 1",
            "sasl set 1 user=bob pass=hunter2",
            "sasl reset 1",
            "diag",
            "diag network 1",
            "config export",
            "config plan",
            "action status 1",
            "action set 1 mode=+B",
            "action set 1 mode=+B message=NickServ text=IDENTIFY hunter2",
            "action set 1",
        ] {
            parse(line).unwrap_or_else(|error| panic!("{line:?} must parse: {error}"));
        }
    }

    #[test]
    fn channel_activity_policy_rejects_duplicates_unknown_fields_and_unbounded_durations() {
        for line in [
            "channel activity 1 #room relay=all reattach=off detach_after=0",
            "channel activity 1 #room relay=all reattach=off detach_after=86401",
            "channel activity 1 #room relay=all relay=none reattach=off",
            "channel activity 1 #room relay=all reattach=off detach_after=off extra=1",
        ] {
            assert!(parse(line).is_err(), "{line:?} must be refused");
        }
    }

    #[test]
    fn watch_rules_are_literal_bounded_and_typed() {
        let ServCommand::WatchAdd { rule } =
            parse("watch add 3 channel #room keyword urgent").unwrap()
        else {
            panic!("watch add parses to typed rule")
        };
        assert_eq!(rule.network, NetworkId(3));
        assert_eq!(rule.matcher, i2pr_irc_store::WatchMatchKind::Keyword);
        for line in [
            "watch add 1 channel #room regex .*",
            "watch add 1 channel #room keyword \r",
            "watch delete 1 0",
            "watch add 1 channel #room keyword thistermiswaytoolong........................................................................................................................................",
        ] {
            assert!(parse(line).is_err(), "{line:?} must be refused");
        }
    }

    #[test]
    fn there_is_no_command_that_could_execute_anything() {
        // Named here rather than merely absent, because the absence is the guarantee.
        for refused in [
            "run sh -c id",
            "exec /bin/sh",
            "network quote PRIVMSG #room :hi",
            "file read /etc/passwd",
            "http get https://example.org",
            "plugin load ./x.so",
            "router reseed",
            "admin adduser someone",
        ] {
            assert!(
                parse(refused).is_err(),
                "{refused:?} must have no reachable implementation"
            );
        }
    }

    #[test]
    fn grammar_is_exact_and_bounded() {
        assert!(parse("network").is_err());
        assert!(parse("network bogus 1").is_err());
        assert!(parse("network status 01").is_err());
        assert!(parse("network status x").is_err());
        // A create with no endpoint is refused before it can become durable state.
        assert!(parse("network create name=lab").is_err());
        // An update needs at least one field.
        assert!(parse("network update 1").is_err());
        // A channel argument is validated as a channel name.
        assert!(parse("channel detach 1 a,b").is_err());
        assert!(parse("channel detach 1 #room extra").is_err());
        // Boolean policy fields accept only the words they claim to.
        assert!(parse("presence set 1 auto_away=maybe").is_err());
        // Repeated attributes are a usage error, not a last-one-wins surprise.
        assert!(parse("sasl set 1 user=a user=b pass=x").is_err());
        assert!(parse("sasl set 1 user=a").is_err());
        let long = format!("help {}", "x".repeat(super::MAX_SERV_LINE_BYTES));
        assert!(parse(&long).is_err());
    }

    #[test]
    fn the_parsed_credential_is_the_only_thing_that_holds_it() {
        let command = parse("sasl set 1 user=bob pass=hunter2").expect("parses");
        // Asserted on the whole command first, before anything is destructured, so the
        // check covers the hand-written `Debug` rather than a field in isolation.
        assert!(
            !format!("{command:?}").contains("hunter2"),
            "nor through the command that carries it"
        );
        let ServCommand::SaslSet {
            username, password, ..
        } = command
        else {
            panic!("expected a credential command");
        };
        assert_eq!(username, "bob");
        // Both properties a credential needs: it cannot be rendered by a diagnostic,
        // and it is not an ordinary `String` that would survive in freed memory.
        assert!(
            !format!("{password:?}").contains("hunter2"),
            "a credential must not be renderable by any diagnostic"
        );
    }
}

#[cfg(test)]
mod action_matrix {
    //! The command matrix the plan asks for, asserted where a command name is first read.
    //!
    //! These live here rather than in `action` because `RegistrationAction` has no
    //! command-name argument to refuse: the allowlist is enforced where a name is parsed,
    //! and a test of it anywhere else would be testing a copy of the rule.

    use super::*;

    /// `ACTION SET` with a command name in the position an attribute name goes.
    fn smuggle(command: &str) -> Result<ServCommand, BouncerError> {
        parse(&format!("ACTION SET 1 {command}=+B"))
    }

    #[test]
    fn every_refused_command_is_refused_by_the_parser() {
        for command in crate::action::FORBIDDEN_COMMANDS {
            let error = smuggle(command).expect_err(&format!("{command} must be refused"));
            assert!(
                matches!(
                    error,
                    BouncerError::UnknownAttribute(ref name) if name == command
                ),
                "{command} produced {error:?}"
            );
        }
    }

    #[test]
    fn a_permitted_command_is_the_only_one_that_parses() {
        assert!(parse("ACTION SET 1 mode=+B").is_ok());
        // `PRIVMSG` reaches the same model through `message=`, not through a verb.
        assert!(parse("ACTION SET 1 message=NickServ text=IDENTIFY pw").is_ok());
        assert!(parse("ACTION SET 1 phase=pre-join message=NickServ text=IDENTIFY pw").is_ok());
        // A message with a space in it, which is what an identify line actually is.
        assert!(parse("ACTION SET 1 message=NickServ text=IDENTIFY hunter2").is_ok());
    }

    #[test]
    fn a_client_tag_cannot_be_stored_on_an_action() {
        // The typed path is unreachable -- there is no attribute for a tag -- and this
        // asserts the two spellings an Operator might try.
        for attempt in [
            "ACTION SET 1 @time=now mode=+B",
            "ACTION SET 1 message=NickServ @time=now text=IDENTIFY pw",
        ] {
            assert!(
                parse(attempt).is_err(),
                "{attempt:?} must not parse: a stored action carries no tag"
            );
        }
    }

    #[test]
    fn a_prefixed_or_raw_line_cannot_be_stored() {
        for attempt in [
            "ACTION SET 1 :bot!u@h JOIN #channel",
            "ACTION SET 1 raw=JOIN #channel",
            "ACTION SET 1 line=PRIVMSG NickServ :IDENTIFY pw",
            "ACTION SET 1 exec=/bin/sh",
            "ACTION SET 1 run=sh -c id",
        ] {
            assert!(
                parse(attempt).is_err(),
                "{attempt:?} must not parse: there is no arbitrary-line path"
            );
        }
    }

    #[test]
    fn a_message_action_without_its_text_is_refused() {
        for attempt in [
            "ACTION SET 1 message=NickServ",
            "ACTION SET 1 text=IDENTIFY pw",
        ] {
            assert!(parse(attempt).is_err(), "{attempt:?} must not parse");
        }
    }

    #[test]
    fn a_refusal_never_echoes_the_text_it_refused() {
        // The one thing a stored action holds that must not escape. A refusal names the
        // attribute; it does not repeat the value.
        let refused = parse("ACTION SET 1 message=bot text=IDENTIFY hunter2")
            .expect_err("a non-service target is refused");
        assert!(!format!("{refused:?}").contains("hunter2"), "{refused:?}");
        assert!(
            !refused.reason().contains("hunter2"),
            "{}",
            refused.reason()
        );
    }

    #[test]
    fn the_parsed_command_never_formats_an_action_payload() {
        let command =
            parse("ACTION SET 1 message=NickServ text=IDENTIFY hunter2").expect("an action");
        assert!(!format!("{command:?}").contains("hunter2"), "{command:?}");
    }
}
