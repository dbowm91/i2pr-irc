//! `soju.im/bouncer-networks`: parsing and rendering for the bouncer control plane.
//!
//! This module is a wire adapter and nothing else. It turns client text into typed
//! values and typed values into client text. It performs no I/O, holds no state, and
//! cannot reach a store, a supervisor, or a socket.
//!
//! Three decisions are load-bearing and are stated here rather than in the call sites.
//!
//! **Attribute maps are never a durable contract.** The draft is work-in-progress and its
//! attribute vocabulary changes. Everything it names is decoded into typed fields that
//! this codebase owns, so a draft revision that renames an attribute is a parsing change
//! and not a schema migration.
//!
//! **`host` is a typed `I2pEndpoint` or it is nothing.** There is no URL parsing here, no
//! port extraction, no TLS material, and no path to a resolver: an attribute that looks
//! like `irc.example.org:6697` or `https://example.org` is refused by `I2pEndpoint::parse`
//! on shape alone, before anything could treat it as a destination.
//!
//! **Every unsupported attribute is named.** A client that sends an attribute this
//! product generation cannot honour gets `FAIL BOUNCER` naming the attribute and why.
//! Silently dropping it would leave the client believing a Network was configured the way
//! it asked, which is the specific failure mode that makes a control plane untrustworthy.

use crate::controller::{ControlNetwork, ControlSnapshot};
use i2pr_irc_core::{I2pEndpoint, NetworkId};

/// The control-plane capability. Its presence is the whole opt-in: a client that does
/// not negotiate it never sends `BOUNCER` and never receives `BOUNCER NET`.
pub const BOUNCER_NETWORKS: &str = "soju.im/bouncer-networks";

/// The change-notification capability.
///
/// Advertised only because this build's initial batch *and* change notifications are both
/// complete. A build that could send the first and not the second would advertise nothing,
/// and a client would have no way to know it was missing an update rather than merely
/// idle.
pub const BOUNCER_NETWORKS_NOTIFY: &str = "soju.im/bouncer-networks-notify";

/// Longest accepted `BOUNCER` line.
///
/// This is the wire's own line ceiling, not a separate number. A larger constant here
/// would only describe a line the decoder has already refused to deliver, and would make
/// the adapter's documented bound a claim about something unreachable.
///
/// The consequence is a real limitation and is recorded as one: a canonical raw
/// `Destination` is 516 characters, which no `BOUNCER ADDNETWORK host=…` line can carry
/// inside a 512-byte IRC line. The `.b32.i2p` and `.i2p` forms both fit comfortably, and
/// those are what this control surface can configure.
pub const MAX_BOUNCER_LINE_BYTES: usize = i2pr_irc_wire::MAX_LINE_BYTES;

/// Ceiling on parameters in one `BOUNCER` line.
pub const MAX_BOUNCER_PARAMS: usize = 16;

/// Ceiling on Networks in one `LISTNETWORKS` reply or one notify batch.
///
/// The catalog's own ceiling is the real bound; this is the value a client can allocate
/// against without reading the store.
pub const MAX_BOUNCER_BATCH: usize = crate::catalog::MAX_SUPERVISED_NETWORKS;

/// The service identity this build answers to.
///
/// A fixed constant, never derived from the host, the release, the process, or the
/// username. It appears in `LISTNETWORKS` output and in refusals, so anything computed
/// would be a machine fingerprint in an IRC-visible field.
pub const SERVICE_NICK: &str = "BouncerServ";

/// Longest accepted attribute *value*, applied per attribute.
///
/// Attribute values reach durable records. A bound here is what keeps an unbounded client
/// string from being handed to storage as configuration.
pub const MAX_ATTRIBUTE_VALUE_BYTES: usize = 256;

/// Longest accepted network display name. Matches the durable field's own ceiling.
pub const MAX_NETWORK_NAME_BYTES: usize = i2pr_irc_store::MAX_DISPLAY_NAME_BYTES;

/// Longest accepted nickname, username, or realname.
pub const MAX_IDENTITY_BYTES: usize = 64;

// ---------------------------------------------------------------- netid

/// Parses a canonical `netid`.
///
/// The canonical spelling is the decimal `NetworkId` with no sign, no padding, and no
/// whitespace, because it must round-trip: a client that writes this back is asserting an
/// identity, and `+7`, `007`, and ` 7` are three spellings of one thing that would make
/// "which Network did you mean" ambiguous.
pub fn parse_netid(raw: &str) -> Result<NetworkId, BouncerError> {
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(BouncerError::InvalidNetid);
    }
    // A leading zero is a different spelling, not a padded one. Rejecting it keeps the
    // canonical form a bijection with `NetworkId`.
    if raw.len() > 1 && raw.starts_with('0') {
        return Err(BouncerError::InvalidNetid);
    }
    raw.parse::<u64>()
        .map(NetworkId)
        .map_err(|_| BouncerError::InvalidNetid)
}

/// The canonical netid spelling.
pub fn render_netid(network: NetworkId) -> String {
    network.0.to_string()
}

// ---------------------------------------------------------------- attributes

/// What one attribute may be used for.
///
/// The disposition is decided here, once, so the two commands that accept attributes
/// (`ADDNETWORK` and `CHANGENETWORK`) cannot disagree about the same name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Attribute {
    /// The operator-facing label. Durable.
    Name,
    /// Live connection phase. Read-only.
    State,
    /// Last live error. Read-only.
    Error,
    /// The I2P destination. Durable.
    Host,
    /// Preferred nickname. Durable.
    Nickname,
    /// IRC username. Durable.
    Username,
    /// IRC realname. Durable.
    Realname,
    /// Recognized, and refused because this product generation has no port.
    Port,
    /// Recognized, and refused because this product generation has no TLS to configure.
    Tls,
    /// Recognized, and refused until a separately reviewed upstream `PASS` exists.
    Pass,
}

/// The disposition of one attribute name.
pub fn classify_attribute(name: &str) -> Result<Attribute, BouncerError> {
    match name {
        "name" => Ok(Attribute::Name),
        "state" => Ok(Attribute::State),
        "error" => Ok(Attribute::Error),
        "host" => Ok(Attribute::Host),
        "nickname" => Ok(Attribute::Nickname),
        "username" => Ok(Attribute::Username),
        "realname" => Ok(Attribute::Realname),
        "port" => Ok(Attribute::Port),
        "tls" => Ok(Attribute::Tls),
        "pass" => Ok(Attribute::Pass),
        other => Err(BouncerError::UnknownAttribute(other.to_owned())),
    }
}

/// Whether an attribute may appear in an `ADDNETWORK`/`CHANGENETWORK` request.
///
/// Read-only attributes are refused rather than ignored. Silently accepting
/// `state=connected` would leave a client believing it had asked the bouncer to connect
/// a Network that is in fact disconnected, and the belief would survive until something
/// else happened to change it.
pub fn writable(attribute: Attribute) -> bool {
    matches!(
        attribute,
        Attribute::Name
            | Attribute::Host
            | Attribute::Nickname
            | Attribute::Username
            | Attribute::Realname
    )
}

/// One decoded, already-validated attribute value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttributeValue {
    pub attribute: Attribute,
    pub value: String,
}

/// Decodes `name=value` pairs into typed, bounded, validated attributes.
///
/// Every failure is an error the caller turns into a `FAIL` naming the attribute, so a
/// client is never told merely "invalid input".
pub fn decode_attributes(params: &[&str]) -> Result<Vec<AttributeValue>, BouncerError> {
    let mut decoded = Vec::new();
    for param in params {
        if decoded.len() >= MAX_BOUNCER_PARAMS {
            return Err(BouncerError::TooManyParameters);
        }
        let (name, value) = param
            .split_once('=')
            .ok_or_else(|| BouncerError::MalformedAttribute((*param).to_owned()))?;
        if name.is_empty() {
            return Err(BouncerError::MalformedAttribute((*param).to_owned()));
        }
        if value.len() > MAX_ATTRIBUTE_VALUE_BYTES {
            return Err(BouncerError::AttributeTooLong(name.to_owned()));
        }
        let attribute = classify_attribute(name)?;
        if !writable(attribute) {
            return Err(match attribute {
                Attribute::State => BouncerError::ReadOnlyAttribute(name.to_owned()),
                Attribute::Error => BouncerError::ReadOnlyAttribute(name.to_owned()),
                Attribute::Port | Attribute::Tls | Attribute::Pass => {
                    BouncerError::UnsupportedAttribute(name.to_owned())
                }
                // `writable` is the allow-list; this arm is unreachable in practice and
                // exists so a future attribute cannot fall through as silently accepted.
                _ => BouncerError::ReadOnlyAttribute(name.to_owned()),
            });
        }
        validate_value(attribute, value)?;
        decoded.push(AttributeValue {
            attribute,
            value: value.to_owned(),
        });
    }
    Ok(decoded)
}

/// One field of a Network, as a bouncer-networks request describes it.
///
/// `None` means "the client did not mention this", which is different from "set it to
/// empty" and is what lets `CHANGENETWORK` be a partial update.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NetworkFields {
    pub name: Option<String>,
    pub host: Option<I2pEndpoint>,
    pub nickname: Option<String>,
    pub username: Option<String>,
    pub realname: Option<String>,
}

impl NetworkFields {
    /// Folds decoded attributes into fields.
    pub fn absorb(&mut self, value: &AttributeValue) -> Result<(), BouncerError> {
        let bounded = |text: &str, ceiling: usize| {
            if text.len() > ceiling {
                return Err(BouncerError::ValueOutOfRange);
            }
            Ok(text.to_owned())
        };
        match value.attribute {
            Attribute::Name => self.name = Some(bounded(&value.value, MAX_NETWORK_NAME_BYTES)?),
            // The whole point: the endpoint is a typed I2P destination or the request is
            // refused. Nothing downstream of this line can treat the text as a host.
            Attribute::Host => {
                self.host = Some(
                    I2pEndpoint::parse(&value.value)
                        .map_err(|_| BouncerError::NotAnI2pEndpoint(value.value.clone()))?,
                );
            }
            Attribute::Nickname => {
                let nick = bounded(&value.value, MAX_IDENTITY_BYTES)?;
                if !crate::valid_client_nick(nick.as_bytes()) {
                    return Err(BouncerError::ValueOutOfRange);
                }
                self.nickname = Some(nick);
            }
            Attribute::Username => self.username = Some(bounded(&value.value, MAX_IDENTITY_BYTES)?),
            Attribute::Realname => self.realname = Some(bounded(&value.value, MAX_IDENTITY_BYTES)?),
            // `decode_attributes` already refused every other attribute.
            other => return Err(BouncerError::ReadOnlyAttribute(format!("{other:?}"))),
        }
        Ok(())
    }
}

/// Validates one already-classified writable attribute.
fn validate_value(attribute: Attribute, value: &str) -> Result<(), BouncerError> {
    match attribute {
        Attribute::Name => {
            if value.is_empty() || value.len() > MAX_NETWORK_NAME_BYTES {
                return Err(BouncerError::ValueOutOfRange);
            }
            // A display name is interpolated into operator-facing numeric replies, so it
            // must be one protocol token: a name carrying a space could change how the
            // surrounding reply parses.
            if !i2pr_irc_store::model::valid_display_name(value) {
                return Err(BouncerError::ValueOutOfRange);
            }
            Ok(())
        }
        Attribute::Host => I2pEndpoint::parse(value)
            .map(|_| ())
            .map_err(|_| BouncerError::NotAnI2pEndpoint(value.to_owned())),
        Attribute::Nickname => {
            if value.len() <= MAX_IDENTITY_BYTES && crate::valid_client_nick(value.as_bytes()) {
                Ok(())
            } else {
                Err(BouncerError::ValueOutOfRange)
            }
        }
        // The username is the IRC user token: bounded, and free of separators so it
        // cannot add a parameter to the registration it reaches. The realname is a
        // trailing free-text field and is bounded only, which is what the protocol
        // allows for it.
        Attribute::Username => {
            if value.len() > MAX_IDENTITY_BYTES
                || value.is_empty()
                || value.bytes().any(|b| !b.is_ascii_graphic())
            {
                return Err(BouncerError::ValueOutOfRange);
            }
            Ok(())
        }
        Attribute::Realname => {
            if value.len() > MAX_IDENTITY_BYTES {
                return Err(BouncerError::ValueOutOfRange);
            }
            Ok(())
        }
        // Reached only if `classify_attribute` grows an attribute `writable` accepts but
        // `validate_value` has not been taught; refusing is the safe default.
        other => Err(BouncerError::ReadOnlyAttribute(format!("{other:?}"))),
    }
}

// ---------------------------------------------------------------- commands

/// One decoded `BOUNCER` command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BouncerCommand {
    /// Claim this connection for a Network. Accepted only before registration completes.
    Bind { network: NetworkId },
    /// List every Network with its current state.
    ListNetworks,
    /// Create a Network from the given attributes.
    AddNetwork { fields: NetworkFields },
    /// Apply a partial update to one Network.
    ChangeNetwork {
        network: NetworkId,
        fields: NetworkFields,
    },
    /// Forget one Network, quiescing its owner first.
    DeleteNetwork { network: NetworkId },
}

/// Decodes one `BOUNCER` command from its already-split parameters.
///
/// `subcommand` is `BOUNCER`'s first parameter; `params` are the rest. The caller has
/// already bounded the line.
pub fn decode_command(subcommand: &str, params: &[&str]) -> Result<BouncerCommand, BouncerError> {
    if params.len() > MAX_BOUNCER_PARAMS {
        return Err(BouncerError::TooManyParameters);
    }
    match subcommand.to_ascii_uppercase().as_str() {
        "BIND" => {
            let raw = params.first().ok_or(BouncerError::Usage)?;
            Ok(BouncerCommand::Bind {
                network: parse_netid(raw)?,
            })
        }
        "LISTNETWORKS" => {
            if !params.is_empty() {
                return Err(BouncerError::Usage);
            }
            Ok(BouncerCommand::ListNetworks)
        }
        "ADDNETWORK" => {
            if params.is_empty() {
                return Err(BouncerError::Usage);
            }
            Ok(BouncerCommand::AddNetwork {
                fields: fields_from(params)?,
            })
        }
        "CHANGENETWORK" => {
            let raw = params.first().ok_or(BouncerError::Usage)?;
            let network = parse_netid(raw)?;
            Ok(BouncerCommand::ChangeNetwork {
                network,
                fields: fields_from(&params[1..])?,
            })
        }
        "DELNETWORK" => {
            let raw = params.first().ok_or(BouncerError::Usage)?;
            let network = parse_netid(raw)?;
            if params.len() != 1 {
                return Err(BouncerError::Usage);
            }
            Ok(BouncerCommand::DeleteNetwork { network })
        }
        other => Err(BouncerError::UnknownSubcommand(other.to_owned())),
    }
}

fn fields_from(params: &[&str]) -> Result<NetworkFields, BouncerError> {
    let mut fields = NetworkFields::default();
    for value in decode_attributes(params)? {
        fields.absorb(&value)?;
    }
    if fields.host.is_none() {
        // A Network with no endpoint is not a Network this bouncer can ever connect.
        // Requiring it here means the durable record is never created in a state that
        // only fails later, at connect time, with a less useful message.
        return Err(BouncerError::Usage);
    }
    Ok(fields)
}

// ---------------------------------------------------------------- rendering

/// Renders one Network entry as a `BOUNCER NET` line.
///
/// The rendered line carries the operator-chosen name, the live state, and the read-only
/// error classification. It deliberately does **not** carry the endpoint: a Network list
/// is shown to every session that negotiated the capability, and an I2P destination is an
/// identity the Operator did not ask to publish.
///
/// `prefix` is the draft's add/remove marker (`+`, `-`) or empty for a plain listing.
pub fn render_network(network: &ControlNetwork, prefix: &str) -> String {
    format!(
        ":{SERVICE_NICK} BOUNCER NET {prefix}{}\r\n",
        render_fields(network)
    )
}

/// The attribute half of a `BOUNCER NET` line, with no prefix and no terminator.
///
/// Rendering fields separately from framing is what lets a notify delta compare two
/// revisions as text rather than inventing a per-field diff format the draft does not
/// define.
pub fn render_fields(network: &ControlNetwork) -> String {
    let state = if network.live {
        network.phase.as_deref().unwrap_or("connecting")
    } else {
        "disconnected"
    };
    let mut fields = format!(
        "netid={} name={} state={state}",
        render_netid(network.network),
        network.display_name,
    );
    if let Some(error) = network.last_session_disposition {
        fields.push_str(&format!(" error={error}"));
    }
    fields
}

/// Renders a whole bounded batch of Networks.
pub fn render_batch<'a>(
    networks: impl Iterator<Item = &'a ControlNetwork>,
    prefix: &str,
) -> Vec<String> {
    networks
        .take(MAX_BOUNCER_BATCH)
        .map(|network| render_network(network, prefix))
        .collect()
}

/// Renders the `FAIL` line for a `BOUNCER` error.
///
/// The draft's required failure form is `FAIL BOUNCER <subcommand> :<reason>`. This build
/// uses it without advertising `standard-replies`, because it does not implement that
/// capability's full semantics; Plan 025 promotes the advertisement once it does.
pub fn render_failure(subcommand: &str, error: &BouncerError) -> String {
    let subcommand = if subcommand.is_empty() {
        error.subcommand()
    } else {
        subcommand
    };
    format!(
        ":{SERVICE_NICK} FAIL BOUNCER {subcommand} :{}\r\n",
        error.reason()
    )
}

/// Renders the notify deltas between two bounded snapshots.
///
/// Deltas are derived from the current bounded snapshot rather than from an event log,
/// so a session that fell behind is reconciled rather than replayed: it is told what is
/// true now, not everything that happened. A Network present in both revisions whose
/// fields render identically produces nothing, and one that changed is re-announced whole
/// rather than diffed field by field — the draft defines no partial-update frame, and
/// inventing one would be a second, private protocol.
///
/// Comparing rendered text is not a shortcut. It is the only comparison available, and it
/// is exact: two entries that render identically are indistinguishable to the client, so
/// announcing one of them would be noise it cannot act on.
pub fn render_delta(previous: &ControlSnapshot, current: &ControlSnapshot) -> Vec<String> {
    if previous.revision == current.revision {
        return Vec::new();
    }
    let before: std::collections::BTreeMap<NetworkId, String> = previous
        .networks
        .iter()
        .map(|entry| (entry.network, render_fields(entry)))
        .collect();
    let after: std::collections::BTreeMap<NetworkId, String> = current
        .networks
        .iter()
        .map(|entry| (entry.network, render_fields(entry)))
        .collect();
    let mut lines = Vec::new();
    for (network, fields) in &after {
        if before.get(network) != Some(fields) {
            lines.push(format!(":{SERVICE_NICK} BOUNCER NET +{fields}\r\n"));
        }
    }
    for (network, fields) in &before {
        if !after.contains_key(network) {
            lines.push(format!(":{SERVICE_NICK} BOUNCER NET -{fields}\r\n"));
        }
    }
    lines
}

// ---------------------------------------------------------------- errors

/// Everything that can be wrong with a `BOUNCER` request.
///
/// Each variant names *what* was wrong rather than only that something was, because the
/// draft's failure form carries the reason to the client and a generic message would make
/// a control plane something a user has to guess at.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BouncerError {
    InvalidNetid,
    UnknownSubcommand(String),
    MalformedAttribute(String),
    UnknownAttribute(String),
    ReadOnlyAttribute(String),
    UnsupportedAttribute(String),
    AttributeTooLong(String),
    ValueOutOfRange,
    NotAnI2pEndpoint(String),
    TooManyParameters,
    Usage,
    /// A requested Network does not exist.
    NoSuchNetwork(NetworkId),
    /// The catalog already holds its ceiling of Networks.
    TooManyNetworks,
    /// The durable layer refused or could not confirm the change.
    NotPersisted,
    /// The Network has no live owner, so it cannot accept a channel or policy request.
    NotLive(NetworkId),
    /// A registered session asked to bind. It stays unbound for the rest of its life.
    BindTooLate,
    /// The Network does not hold that channel as desired intent.
    NoSuchChannel(String),
    /// The controller's own bounded queue refused the request.
    Overloaded,
}

impl BouncerError {
    /// The client-visible reason.
    ///
    /// Every string is fixed and bounded. The one dynamic case, `NoSuchNetwork`, renders
    /// a canonical netid, which is an Operator-chosen integer rather than anything the
    /// network said.
    pub fn reason(&self) -> String {
        match self {
            Self::InvalidNetid => "malformed network id".to_owned(),
            Self::UnknownSubcommand(name) => format!("unknown subcommand {name}"),
            Self::MalformedAttribute(name) => format!("malformed attribute {name}"),
            Self::UnknownAttribute(name) => format!("unknown attribute {name}"),
            Self::ReadOnlyAttribute(name) => format!("{name} is read-only"),
            Self::UnsupportedAttribute(name) => format!("{name} is not supported"),
            Self::AttributeTooLong(name) => format!("{name} is too long"),
            Self::ValueOutOfRange => "attribute value out of range".to_owned(),
            Self::NotAnI2pEndpoint(_) => "host must be an I2P destination".to_owned(),
            Self::TooManyParameters => "too many parameters".to_owned(),
            Self::Usage => "wrong parameters for this subcommand".to_owned(),
            Self::NoSuchNetwork(network) => {
                format!("no network with id {}", render_netid(*network))
            }
            Self::TooManyNetworks => "network limit reached".to_owned(),
            Self::NotPersisted => "change could not be persisted".to_owned(),
            Self::NotLive(network) => {
                format!("network {} is not connected", render_netid(*network))
            }
            Self::BindTooLate => "a registered session cannot bind to a network".to_owned(),
            Self::NoSuchChannel(channel) => format!("{channel} is not a desired channel"),
            Self::Overloaded => "bouncer is busy".to_owned(),
        }
    }

    /// The `BOUNCER` subcommand a `FAIL` for this error names.
    pub fn subcommand(&self) -> &'static str {
        match self {
            Self::NoSuchNetwork(_) | Self::NotLive(_) | Self::TooManyNetworks => "NET",
            Self::BindTooLate => "BIND",
            Self::NoSuchChannel(_) => "CHANNEL",
            _ => "",
        }
    }
}

impl std::fmt::Display for BouncerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason())
    }
}

impl std::error::Error for BouncerError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_netid_round_trips_and_rejects_every_other_spelling() {
        assert_eq!(parse_netid("7"), Ok(NetworkId(7)));
        assert_eq!(render_netid(NetworkId(7)), "7");
        // `+7`, `007`, and ` 7` are three spellings of one identity. A control plane that
        // accepted them could not answer "which Network did you mean".
        assert_eq!(parse_netid("+7"), Err(BouncerError::InvalidNetid));
        assert_eq!(parse_netid("007"), Err(BouncerError::InvalidNetid));
        assert_eq!(parse_netid(" 7"), Err(BouncerError::InvalidNetid));
        assert_eq!(parse_netid(""), Err(BouncerError::InvalidNetid));
        assert_eq!(parse_netid("7x"), Err(BouncerError::InvalidNetid));
        assert_eq!(
            parse_netid("99999999999999999999999"),
            Err(BouncerError::InvalidNetid)
        );
    }

    #[test]
    fn only_i2p_destinations_are_accepted_as_a_host() {
        // A canonical b32 destination: 52 base32 characters before the suffix.
        let destination = format!("{}.b32.i2p", "a".repeat(52));
        let ok = fields_from(&[&format!("host={destination}")]).expect("a b32 destination");
        assert!(ok.host.is_some());
        // A `.b32.i2p` of the wrong length is not a destination either.
        assert!(fields_from(&["host=example.b32.i2p"]).is_err());
        assert!(fields_from(&["host=irc.example.org"]).is_err());
        // The shapes that would invite generic networking are refused on form alone.
        assert!(fields_from(&["host=irc.example.org:6697"]).is_err());
        assert!(fields_from(&["host=https://irc.example.org"]).is_err());
        assert!(fields_from(&["host=user@irc.example.org"]).is_err());
        assert!(fields_from(&["host="]).is_err());
    }

    #[test]
    fn read_only_and_unsupported_attributes_are_named_not_dropped() {
        assert_eq!(classify_attribute("state"), Ok(Attribute::State));
        assert!(!writable(Attribute::State));
        for name in ["port", "tls", "pass"] {
            assert!(matches!(
                classify_attribute(name),
                Ok(Attribute::Port) | Ok(Attribute::Tls) | Ok(Attribute::Pass)
            ));
        }
        assert!(matches!(
            classify_attribute("nonsense"),
            Err(BouncerError::UnknownAttribute(_))
        ));
        // A missing `=` is malformed, not an attribute with an empty value.
        assert!(matches!(
            decode_attributes(&["host"]),
            Err(BouncerError::MalformedAttribute(_))
        ));
    }

    #[test]
    fn command_grammar_is_exact() {
        assert_eq!(
            decode_command("LISTNETWORKS", &[]),
            Ok(BouncerCommand::ListNetworks)
        );
        assert_eq!(
            decode_command("bind", &["3"]),
            Ok(BouncerCommand::Bind {
                network: NetworkId(3)
            })
        );
        assert_eq!(
            decode_command("DELNETWORK", &["3"]),
            Ok(BouncerCommand::DeleteNetwork {
                network: NetworkId(3)
            })
        );
        // Extra parameters are a usage error rather than something quietly ignored.
        assert_eq!(
            decode_command("DELNETWORK", &["3", "extra"]),
            Err(BouncerError::Usage)
        );
        assert_eq!(
            decode_command("LISTNETWORKS", &["3"]),
            Err(BouncerError::Usage)
        );
        assert!(matches!(
            decode_command("NETWORK", &[]),
            Err(BouncerError::UnknownSubcommand(_))
        ));
    }

    #[test]
    fn a_rendered_network_never_carries_the_endpoint() {
        let snapshot = ControlSnapshot::default();
        let _ = snapshot;
        // The rendering function only has a `ControlNetwork`, which has no endpoint
        // field at all. That is the structural guarantee: there is nothing to leak.
        let entry = ControlNetwork {
            network: NetworkId(1),
            display_name: "lab".to_owned(),
            live: true,
            phase: Some("connected".to_owned()),
            attached_sessions: 2,
            last_session_disposition: Some("accepted"),
        };
        let rendered = render_network(&entry, "");
        assert!(rendered.contains("netid=1"));
        assert!(rendered.contains("name=lab"));
        assert!(rendered.contains("state=connected"));
        assert!(rendered.contains("error=accepted"));
    }

    #[test]
    fn delta_is_derived_from_bounded_state_not_an_event_log() {
        let base = ControlSnapshot {
            revision: 1,
            networks: vec![ControlNetwork {
                network: NetworkId(1),
                display_name: "lab".to_owned(),
                live: false,
                phase: None,
                attached_sessions: 0,
                last_session_disposition: None,
            }],
        };
        // The same revision is nothing at all, however many times it is asked.
        assert!(render_delta(&base, &base).is_empty());
        let gained = ControlSnapshot {
            revision: 2,
            networks: vec![
                base.networks[0].clone(),
                ControlNetwork {
                    network: NetworkId(2),
                    display_name: "other".to_owned(),
                    live: false,
                    phase: None,
                    attached_sessions: 0,
                    last_session_disposition: None,
                },
            ],
        };
        let added = render_delta(&base, &gained).join("");
        assert!(added.contains("+netid=2"), "{added}");
        let emptied = ControlSnapshot {
            revision: 3,
            networks: Vec::new(),
        };
        let removed = render_delta(&gained, &emptied).join("");
        assert!(removed.contains("netid=1"), "{removed}");
        assert!(removed.contains("netid=2"), "{removed}");
    }
}
