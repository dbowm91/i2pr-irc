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
    NetworkStatus { network: NetworkId },
    /// Create a Network. Identity fields the Operator did not supply are filled with
    /// fixed defaults derived from the Network's own display name.
    NetworkCreate { fields: NetworkFields },
    /// Apply a partial update to one Network.
    NetworkUpdate {
        network: NetworkId,
        fields: NetworkFields,
    },
    /// Forget one Network.
    NetworkDelete { network: NetworkId },
    /// Report one channel's presentation policy on one Network.
    ChannelStatus { network: NetworkId, channel: String },
    /// Stop presenting one channel the bouncer still holds.
    ChannelDetach { network: NetworkId, channel: String },
    /// Resume presenting one detached channel.
    ChannelAttach { network: NetworkId, channel: String },
    /// Report one Network's presence policy.
    PresenceStatus { network: NetworkId },
    /// Change one Network's auto-away policy.
    PresenceSet { network: NetworkId, auto_away: bool },
    /// Report one Network's keep-nick policy.
    NickStatus { network: NetworkId },
    /// Change one Network's keep-nick policy.
    NickSet { network: NetworkId, keep_nick: bool },
    /// Report whether a Network has a credential, and under which name.
    ///
    /// The value is never in this type, so it cannot be rendered by accident.
    SaslStatus { network: NetworkId },
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
    SaslReset { network: NetworkId },
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
    " | presence status <netid>",
    " | presence set <netid> auto_away=on|off",
    " | nick status <netid>",
    " | nick set <netid> keep_nick=on|off",
    " | sasl status <netid>",
    " | sasl set <netid> user=<username> pass=<password>",
    " | sasl reset <netid>",
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
        other => Err(BouncerError::UnknownSubcommand(other.to_owned())),
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
            "presence status 1",
            "presence set 1 auto_away=on",
            "nick status 1",
            "nick set 1 keep_nick=off",
            "sasl status 1",
            "sasl set 1 user=bob pass=hunter2",
            "sasl reset 1",
        ] {
            parse(line).unwrap_or_else(|error| panic!("{line:?} must parse: {error}"));
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
