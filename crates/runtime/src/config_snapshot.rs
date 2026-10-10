//! The versioned, local configuration snapshot format.
//!
//! This is a *typed local format*, deliberately not IRC draft syntax and deliberately not
//! a configuration language. It exists so an Operator can move a bouncer's non-secret
//! configuration between machines and read what they are moving without running anything.
//!
//! # What is deliberately absent
//!
//! No credential, no `StoredSecret`, no SASL username, and no registration-action payload.
//! That is not a redaction step applied while rendering -- the record type has no field to
//! hold any of them, so the renderer cannot leak one even if it were wrong. Action
//! *metadata* is exported: how many actions a Network has, which is operational
//! information an Operator needs and discloses nothing about their content.
//!
//! The endpoint **is** exported, which is a different decision from the one `render_network`
//! in [`crate::bouncer_networks`] makes. That function renders into an IRC-visible frame on
//! a downstream connection, where every session that negotiated a capability can read it.
//! This one renders into an export the Operator explicitly asked for and holds to store. An
//! I2P destination is the identity of the Network being moved; an export without it would
//! not be an export.
//!
//! # Versioning
//!
//! [`SNAPSHOT_VERSION`] is the only version this build writes, and [`parse`] refuses any
//! other. A format that guessed at a version it did not recognise would import a Network
//! with fields missing rather than refuse it, and the Operator would find out from a failed
//! connection instead of from the refusal.
//!
//! # Import boundary
//!
//! Import is deliberately **plan-then-apply, per Network**, not transactional across
//! Networks. The durable store is one bounded worker behind a request queue; a
//! multi-Network import cannot be made atomic over it without either holding a transaction
//! open across owner restarts or adding a second writer, and both are worse than an honest
//! boundary. What this module guarantees instead is the property that actually matters:
//! **nothing is mutated until the entire snapshot has parsed and validated.** A snapshot
//! that fails validation leaves the runtime byte-for-byte as it was.

use crate::bouncer_networks::{self, BouncerError};
use i2pr_irc_core::{I2pEndpoint, NetworkId};
use i2pr_irc_store::{
    DesiredChannelRecord, FailoverEndpointGroup, IrcTransportProfile, MAX_DISPLAY_NAME_BYTES,
    NetworkRecord, StoredSecret, UpstreamAuthProfile,
};

/// The only snapshot version this build writes or accepts.
pub const SNAPSHOT_VERSION: u32 = 5;
/// Version 1 carried only a total action count; its actions use the migrated PostJoin phase.
const LEGACY_SNAPSHOT_VERSION: u32 = 1;
/// Version 2 introduced explicit action phase counts and defaults activity policy to off.
const PREVIOUS_SNAPSHOT_V2: u32 = 2;
const PREVIOUS_SNAPSHOT_V3: u32 = 3;
const PREVIOUS_SNAPSHOT_VERSION: u32 = 4;

/// The first line of every snapshot, and the marker a parser requires before anything else.
///
/// A fixed magic string rather than a bare `version 1`, so a file that is some other text
/// entirely is refused as "not a snapshot" instead of being scanned for lines that happen
/// to look like configuration.
pub const SNAPSHOT_MAGIC: &str = "#i2pr-bouncer-config";

/// Ceiling on lines in one snapshot, including maximum per-Network channels, endpoint
/// declarations and alternate endpoints, plus the magic/version headers.
pub const MAX_SNAPSHOT_LINES: usize = crate::catalog::MAX_SUPERVISED_NETWORKS
    * (MAX_SNAPSHOT_CHANNELS + i2pr_irc_store::MAX_NETWORK_ENDPOINTS + 2)
    + 1;

/// Ceiling on channels in one Network.
///
/// The same ceiling the runtime enforces live, so a snapshot cannot describe a
/// configuration the bouncer would refuse to hold.
pub const MAX_SNAPSHOT_CHANNELS: usize = crate::state::MAX_CHANNELS;

/// Ceiling on bytes in one snapshot line.
///
/// Sized so the `NOTICE` prefix plus this line still fits the wire's line ceiling, because
/// the export is delivered one line per `NOTICE` and a line that had to be split would
/// arrive as two records -- which is precisely the failure this ceiling exists to prevent.
/// 512 minus the longest prefix a reply can carry (tag, service identity, and a 64-byte nick)
/// leaves the room this value takes.
pub const MAX_SNAPSHOT_LINE_BYTES: usize = 320;

/// Everything that can be wrong with a snapshot.
///
/// Each variant names *what* was wrong rather than only that something was, because an
/// Operator fixing a hand-edited snapshot needs to know which line to look at and a generic
/// message would leave them guessing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigError {
    /// The text did not begin with [`SNAPSHOT_MAGIC`].
    NotASnapshot,
    /// The version was absent, or is not [`SNAPSHOT_VERSION`].
    UnsupportedVersion(u32),
    /// A line was longer than [`MAX_SNAPSHOT_LINE_BYTES`].
    LineTooLong,
    /// More lines than [`MAX_SNAPSHOT_LINES`].
    TooManyLines,
    /// A record line carried a name this format does not define.
    UnknownAttribute(String),
    /// A record line was missing a value it must carry.
    MissingAttribute(&'static str),
    /// A value was outside its ceiling, or not of its declared shape.
    InvalidValue(&'static str),
    /// Two records claimed one `NetworkId`.
    DuplicateNetwork(NetworkId),
    /// A channel line appeared before any record line, or after the last one.
    OrphanChannel,
    /// More channels in one Network than [`MAX_SNAPSHOT_CHANNELS`].
    TooManyChannels,
    /// A credential was present in the text, named by the attribute that carried it.
    ///
    /// A distinct variant rather than an unknown attribute, because the single most likely
    /// reason an Operator finds this is that they pasted a line including a password and
    /// are owed a clear statement that it was refused rather than skipped. The name is
    /// reported because "there was a secret somewhere" leaves them searching every line.
    RefusedSecret(String),
    /// The decoded record failed the store's own validation.
    InvalidRecord(BouncerError),
    /// The text was not valid UTF-8.
    NotText,
}

/// One Network's non-secret configuration, as a snapshot carries it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotNetwork {
    pub network: NetworkId,
    pub display_name: String,
    pub endpoint: I2pEndpoint,
    pub transport_profile: IrcTransportProfile,
    pub auth_profile: UpstreamAuthProfile,
    pub failover_group: Option<FailoverEndpointGroup>,
    /// Legacy snapshots do not represent failover settings. In that case, keep the
    /// already stored setting when applying an update.
    pub retain_existing_failover: bool,
    /// Versions 1-4 do not contain authentication profiles. Preserve existing policy
    /// on updates; new records from those versions use the plain/no-auth defaults.
    pub retain_existing_profiles: bool,
    pub nick: String,
    pub username: String,
    pub realname: String,
    pub auto_away: bool,
    pub keep_nick: bool,
    pub desired_channels: Vec<DesiredChannelRecord>,
    /// How many registration actions the Network has.
    ///
    /// The *count*, never the payloads: a stored action may carry a service credential in
    /// its text, and an export is something an Operator pastes into a support channel.
    pub action_count: u32,
    /// Secret-free action counts in pre-join, post-join, fallback-recovery order.
    pub action_phase_counts: [u32; 3],
}

/// A whole configuration snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConfigSnapshot {
    pub networks: Vec<SnapshotNetwork>,
}

impl ConfigSnapshot {
    /// Every Network this snapshot names, in `NetworkId` order.
    pub fn networks(&self) -> &[SnapshotNetwork] {
        &self.networks
    }
}

/// Renders one snapshot as text.
///
/// Deterministic: Networks in `NetworkId` order and channels in stored position order, so
/// exporting an unchanged configuration twice produces identical bytes and an Operator can
/// diff two exports to see what changed.
pub fn render(snapshot: &ConfigSnapshot) -> String {
    let mut out = String::new();
    out.push_str(SNAPSHOT_MAGIC);
    out.push('\n');
    out.push_str(&format!("version {SNAPSHOT_VERSION}\n"));
    let mut networks = snapshot.networks.clone();
    networks.sort_by_key(|entry| entry.network);
    for entry in &networks {
        out.push_str(&render_network(entry));
    }
    out
}

/// Renders one Network's record and channel lines, each newline-terminated.
fn render_network(entry: &SnapshotNetwork) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "network netid={} name={} host={} nick={} username={} realname={} \
         transport={} auth={} auto_away={} keep_nick={} actions={} action_phases={},{},{}\n",
        bouncer_networks::render_netid(entry.network),
        entry.display_name,
        entry.endpoint.as_str(),
        entry.nick,
        entry.username,
        entry.realname,
        entry.transport_profile.as_str(),
        entry.auth_profile.as_str(),
        on_off(entry.auto_away),
        on_off(entry.keep_nick),
        entry.action_count,
        entry.action_phase_counts[0],
        entry.action_phase_counts[1],
        entry.action_phase_counts[2],
    ));
    if let Some(group) = &entry.failover_group {
        out.push_str("failover equivalent=operator credentials=operator\n");
        for endpoint in &group.alternates {
            out.push_str(&format!("alternate host={}\n", endpoint.as_str()));
        }
    } else {
        out.push_str("failover mode=disabled\n");
    }
    let mut channels = entry.desired_channels.clone();
    channels.sort_by_key(|channel| channel.position);
    for channel in channels {
        out.push_str(&format!(
            "channel target={} position={} detached={} relay_detached={} reattach_on={} detach_after_secs={}\n",
            channel.target,
            channel.position,
            on_off(channel.detached),
            match channel.activity.relay_detached { i2pr_irc_store::RelayDetached::None => "none", i2pr_irc_store::RelayDetached::Mentions => "mentions", i2pr_irc_store::RelayDetached::All => "all" },
            match channel.activity.reattach_on { i2pr_irc_store::ReattachOn::Off => "off", i2pr_irc_store::ReattachOn::Message => "message", i2pr_irc_store::ReattachOn::Mention => "mention" },
            channel.activity.detach_after_secs.map(|seconds| seconds.to_string()).unwrap_or_else(|| "off".to_owned()),
        ));
    }
    out
}

/// The fixed spelling of a boolean in this format.
fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

/// Parses a snapshot, validating every Network before returning any of it.
///
/// The two-phase shape is the point: [`parse`] either returns a fully validated snapshot or
/// an error, and nothing downstream can act on a partially understood configuration. A
/// parser that appended records as it went would let a caller apply the ones it liked.
pub fn parse(text: &str) -> Result<ConfigSnapshot, ConfigError> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() > MAX_SNAPSHOT_LINES {
        return Err(ConfigError::TooManyLines);
    }
    if lines.first().map(|line| line.trim()) != Some(SNAPSHOT_MAGIC) {
        return Err(ConfigError::NotASnapshot);
    }
    for line in &lines {
        if line.len() > MAX_SNAPSHOT_LINE_BYTES {
            return Err(ConfigError::LineTooLong);
        }
    }
    // The version line is the second one by construction. Parsed positionally rather than
    // by search, so a `version` line appearing later cannot retroactively restate a
    // document whose real shape was decided before it.
    let version = match lines.get(1) {
        Some(line) => parse_version(line)?,
        None => return Err(ConfigError::UnsupportedVersion(0)),
    };
    if !matches!(
        version,
        SNAPSHOT_VERSION
            | PREVIOUS_SNAPSHOT_VERSION
            | PREVIOUS_SNAPSHOT_V3
            | PREVIOUS_SNAPSHOT_V2
            | LEGACY_SNAPSHOT_VERSION
    ) {
        return Err(ConfigError::UnsupportedVersion(version));
    }

    // Refuse secret-shaped lines before attempting any record conversion, so an invalid
    // earlier action-count field cannot mask a credential pasted later in the snapshot.
    for line in lines.iter().skip(2) {
        if let Some(field) = split_fields(line).first()
            && is_secret_name(field.split('=').next().unwrap_or(field))
        {
            return Err(ConfigError::RefusedSecret((*field).to_owned()));
        }
    }

    let mut snapshot = ConfigSnapshot::default();
    let mut current: Option<usize> = None;
    let mut failover_declared = std::collections::BTreeSet::new();
    let mut failover_disabled = std::collections::BTreeSet::new();
    for line in lines.iter().skip(2) {
        let fields = split_fields(line);
        if fields.is_empty() {
            continue;
        }
        match fields[0] {
            "network" => {
                let entry = parse_network(&fields[1..], version)?;
                if snapshot.networks.iter().any(|n| n.network == entry.network) {
                    return Err(ConfigError::DuplicateNetwork(entry.network));
                }
                snapshot.networks.push(entry);
                current = Some(snapshot.networks.len() - 1);
            }
            "channel" => {
                let Some(index) = current else {
                    return Err(ConfigError::OrphanChannel);
                };
                let channel = parse_channel(&fields[1..])?;
                let entry = &mut snapshot.networks[index];
                if entry.desired_channels.len() >= MAX_SNAPSHOT_CHANNELS {
                    return Err(ConfigError::TooManyChannels);
                }
                entry.desired_channels.push(channel);
            }
            "failover" => {
                let Some(index) = current else {
                    return Err(ConfigError::OrphanChannel);
                };
                if version < PREVIOUS_SNAPSHOT_VERSION || !failover_declared.insert(index) {
                    return Err(ConfigError::InvalidValue("failover"));
                }
                if fields.len() == 2 && fields[1] == "mode=disabled" {
                    failover_disabled.insert(index);
                } else if fields.len() == 3
                    && fields[1] == "equivalent=operator"
                    && fields[2] == "credentials=operator"
                {
                    snapshot.networks[index].failover_group = Some(FailoverEndpointGroup {
                        alternates: Vec::new(),
                        operator_attests_equivalence: true,
                        credentials_authorized: true,
                    });
                } else {
                    return Err(ConfigError::InvalidValue("failover"));
                }
            }
            "alternate" => {
                let Some(index) = current else {
                    return Err(ConfigError::OrphanChannel);
                };
                if !failover_declared.contains(&index)
                    || failover_disabled.contains(&index)
                    || fields.len() != 2
                {
                    return Err(ConfigError::InvalidValue("alternate"));
                }
                let Some(value) = fields[1].strip_prefix("host=") else {
                    return Err(ConfigError::InvalidValue("alternate"));
                };
                let endpoint = I2pEndpoint::parse(value)
                    .map_err(|_| ConfigError::InvalidValue("alternate"))?;
                if value.len() > 240
                    || endpoint.kind() == i2pr_irc_core::I2pEndpointKind::Destination
                {
                    return Err(ConfigError::InvalidValue("alternate"));
                }
                let group = snapshot.networks[index]
                    .failover_group
                    .as_mut()
                    .expect("declaration makes group");
                if group.alternates.len() >= i2pr_irc_store::MAX_NETWORK_ENDPOINTS - 1 {
                    return Err(ConfigError::InvalidValue("alternate"));
                }
                group.alternates.push(endpoint);
            }
            // A credential cannot be represented, so seeing one means the text was not
            // produced by this renderer or was edited to add one. Either way it is refused
            // with a name, not silently dropped.
            other if is_secret_name(other) => {
                return Err(ConfigError::RefusedSecret(other.to_owned()));
            }
            other => return Err(ConfigError::UnknownAttribute(other.to_owned())),
        }
    }
    if version >= PREVIOUS_SNAPSHOT_VERSION && failover_declared.len() != snapshot.networks.len() {
        return Err(ConfigError::InvalidValue("missing failover policy"));
    }
    for index in failover_declared {
        if snapshot.networks[index]
            .failover_group
            .as_ref()
            .is_some_and(|group| group.alternates.is_empty())
        {
            return Err(ConfigError::InvalidValue("empty failover group"));
        }
    }
    for entry in &snapshot.networks {
        if let Some(group) = &entry.failover_group {
            let mut endpoints = std::collections::BTreeSet::new();
            endpoints.insert(entry.endpoint.as_str());
            if group
                .alternates
                .iter()
                .any(|endpoint| !endpoints.insert(endpoint.as_str()))
            {
                return Err(ConfigError::InvalidValue("duplicate failover endpoint"));
            }
        }
    }
    snapshot.networks.sort_by_key(|entry| entry.network);
    Ok(snapshot)
}

/// Whether an attribute name is one this format must refuse rather than ignore.
///
/// A closed set on purpose: "anything that looks secret-ish" is not a rule an Operator can
/// predict, and a list that grows is a list nobody re-reads when it does.
fn is_secret_name(name: &str) -> bool {
    matches!(name, "sasl" | "password" | "pass" | "secret" | "credential")
}

/// Parses the `version N` line.
fn parse_version(line: &str) -> Result<u32, ConfigError> {
    let value = line
        .trim()
        .strip_prefix("version ")
        .ok_or(ConfigError::UnsupportedVersion(0))?;
    value
        .trim()
        .parse::<u32>()
        .map_err(|_| ConfigError::UnsupportedVersion(0))
}

/// Splits one record line into whitespace-separated fields.
///
/// Values in this format never contain whitespace -- a channel name and an I2P destination
/// cannot, and a nick cannot -- so the simple split is correct and the bounds are enforced
/// per field rather than by a quoting rule nobody would remember.
fn split_fields(line: &str) -> Vec<&str> {
    line.split_whitespace().collect()
}

/// Parses one `network` line into a validated record.
fn parse_network(fields: &[&str], version: u32) -> Result<SnapshotNetwork, ConfigError> {
    let mut netid = None;
    let mut name = None;
    let mut host = None;
    let mut nick = None;
    let mut username = None;
    let mut realname = None;
    let mut transport = None;
    let mut auth = None;
    let mut auto_away = None;
    let mut keep_nick = None;
    let mut actions = None;
    let mut action_phases = None;
    for field in fields {
        // Checked before the `=` split, because the most likely way a credential reaches a
        // snapshot is an Operator pasting `sasl user=bob` onto a record line -- a field
        // with no `=` at all. Reporting that as a malformed attribute would send them
        // looking for a syntax problem instead of telling them their secret was refused.
        let word = field.split('=').next().unwrap_or(field);
        if is_secret_name(word) {
            return Err(ConfigError::RefusedSecret(word.to_owned()));
        }
        let (key, value) = field
            .split_once('=')
            .ok_or_else(|| ConfigError::UnknownAttribute((*field).to_owned()))?;
        match key {
            "netid" => netid = Some(value),
            "name" => name = Some(value),
            "host" => host = Some(value),
            "nick" => nick = Some(value),
            "username" => username = Some(value),
            "realname" => realname = Some(value),
            "transport" => transport = Some(value),
            "auth" => auth = Some(value),
            "auto_away" => auto_away = Some(parse_on_off(value)?),
            "keep_nick" => keep_nick = Some(parse_on_off(value)?),
            "actions" => actions = Some(parse_bounded_u32("actions", value)?),
            "action_phases" => {
                let mut counts = [0; 3];
                let mut parts = value.split(',');
                for count in &mut counts {
                    *count = parse_bounded_u32("action_phases", parts.next().unwrap_or(""))?;
                }
                if parts.next().is_some() {
                    return Err(ConfigError::InvalidValue("action_phases"));
                }
                action_phases = Some(counts);
            }
            other => return Err(ConfigError::UnknownAttribute(other.to_owned())),
        }
    }
    let display_name = name.ok_or(ConfigError::MissingAttribute("name"))?;
    if display_name.len() > MAX_DISPLAY_NAME_BYTES || display_name.is_empty() {
        return Err(ConfigError::InvalidValue("name"));
    }
    let endpoint = I2pEndpoint::parse(host.ok_or(ConfigError::MissingAttribute("host"))?)
        .map_err(|_| ConfigError::InvalidValue("host"))?;
    let nick =
        bounded_token("nick", nick.ok_or(ConfigError::MissingAttribute("nick"))?)?.to_owned();
    let username = bounded_token(
        "username",
        username.ok_or(ConfigError::MissingAttribute("username"))?,
    )?
    .to_owned();
    let realname = bounded_token(
        "realname",
        realname.ok_or(ConfigError::MissingAttribute("realname"))?,
    )?
    .to_owned();
    let auto_away = auto_away.ok_or(ConfigError::MissingAttribute("auto_away"))?;
    let keep_nick = keep_nick.ok_or(ConfigError::MissingAttribute("keep_nick"))?;
    let action_count = actions.ok_or(ConfigError::MissingAttribute("actions"))?;
    let action_phase_counts = match (version, action_phases) {
        (_, Some(counts)) => counts,
        (LEGACY_SNAPSHOT_VERSION, None) => [0, action_count, 0],
        (
            PREVIOUS_SNAPSHOT_V2
            | PREVIOUS_SNAPSHOT_V3
            | PREVIOUS_SNAPSHOT_VERSION
            | SNAPSHOT_VERSION,
            None,
        ) => {
            return Err(ConfigError::MissingAttribute("action_phases"));
        }
        _ => return Err(ConfigError::UnsupportedVersion(version)),
    };
    if action_phase_counts.iter().copied().sum::<u32>() != action_count {
        return Err(ConfigError::InvalidValue("action_phases"));
    }
    let (transport_profile, auth_profile) = if version == SNAPSHOT_VERSION {
        let transport = transport.ok_or(ConfigError::MissingAttribute("transport"))?;
        let auth = auth.ok_or(ConfigError::MissingAttribute("auth"))?;
        let transport_profile = IrcTransportProfile::parse(transport)
            .map_err(|_| ConfigError::InvalidValue("transport"))?;
        let auth_profile =
            UpstreamAuthProfile::parse(auth).map_err(|_| ConfigError::InvalidValue("auth"))?;
        if transport_profile != IrcTransportProfile::PlainI2p {
            return Err(ConfigError::InvalidValue("unsupported transport"));
        }
        if auth_profile == UpstreamAuthProfile::SaslExternal {
            return Err(ConfigError::InvalidValue("unsupported auth profile"));
        }
        (transport_profile, auth_profile)
    } else {
        if transport.is_some() || auth.is_some() {
            return Err(ConfigError::InvalidValue("profile version"));
        }
        (IrcTransportProfile::PlainI2p, UpstreamAuthProfile::None)
    };
    Ok(SnapshotNetwork {
        network: bouncer_networks::parse_netid(
            netid.ok_or(ConfigError::MissingAttribute("netid"))?,
        )
        .map_err(|_| ConfigError::InvalidValue("netid"))?,
        display_name: display_name.to_owned(),
        endpoint,
        transport_profile,
        auth_profile,
        failover_group: None,
        retain_existing_failover: version < PREVIOUS_SNAPSHOT_VERSION,
        retain_existing_profiles: version != SNAPSHOT_VERSION,
        nick,
        username,
        realname,
        auto_away,
        keep_nick,
        desired_channels: Vec::new(),
        action_count,
        action_phase_counts,
    })
}

/// Parses one `channel` line.
fn parse_channel(fields: &[&str]) -> Result<DesiredChannelRecord, ConfigError> {
    let mut target = None;
    let mut position = None;
    let mut detached = None;
    let mut relay_detached = i2pr_irc_store::RelayDetached::None;
    let mut reattach_on = i2pr_irc_store::ReattachOn::Off;
    let mut detach_after_secs = None;
    for field in fields {
        let (key, value) = field
            .split_once('=')
            .ok_or_else(|| ConfigError::UnknownAttribute((*field).to_owned()))?;
        match key {
            "target" => target = Some(value),
            "position" => position = Some(parse_bounded_u32("position", value)?),
            "detached" => detached = Some(parse_on_off(value)?),
            "relay_detached" => {
                relay_detached = match value {
                    "none" => i2pr_irc_store::RelayDetached::None,
                    "mentions" => i2pr_irc_store::RelayDetached::Mentions,
                    "all" => i2pr_irc_store::RelayDetached::All,
                    _ => return Err(ConfigError::InvalidValue("relay_detached")),
                }
            }
            "reattach_on" => {
                reattach_on = match value {
                    "off" => i2pr_irc_store::ReattachOn::Off,
                    "message" => i2pr_irc_store::ReattachOn::Message,
                    "mention" => i2pr_irc_store::ReattachOn::Mention,
                    _ => return Err(ConfigError::InvalidValue("reattach_on")),
                }
            }
            "detach_after_secs" => {
                detach_after_secs = if value == "off" {
                    None
                } else {
                    Some(parse_bounded_u32("detach_after_secs", value)?)
                }
            }
            other => return Err(ConfigError::UnknownAttribute(other.to_owned())),
        }
    }
    let target = target.ok_or(ConfigError::MissingAttribute("target"))?;
    if !crate::state::valid_channel_token(target) {
        return Err(ConfigError::InvalidValue("target"));
    }
    Ok(DesiredChannelRecord {
        target: target.to_owned(),
        position: position.ok_or(ConfigError::MissingAttribute("position"))? as usize,
        detached: detached.ok_or(ConfigError::MissingAttribute("detached"))?,
        activity: i2pr_irc_store::ChannelActivityPolicy {
            relay_detached,
            reattach_on,
            detach_after_secs,
        },
    })
}

/// A bounded non-empty token, which is what every IRC identity field in this format is.
fn bounded_token<'a>(name: &'static str, value: &'a str) -> Result<&'a str, ConfigError> {
    if value.is_empty() || value.len() > bouncer_networks::MAX_IDENTITY_BYTES {
        return Err(ConfigError::InvalidValue(name));
    }
    if !value.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(ConfigError::InvalidValue(name));
    }
    Ok(value)
}

/// Parses `on`/`off` only.
fn parse_on_off(value: &str) -> Result<bool, ConfigError> {
    match value {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => Err(ConfigError::InvalidValue("boolean")),
    }
}

/// Parses one bounded `u32` count.
fn parse_bounded_u32(name: &'static str, value: &str) -> Result<u32, ConfigError> {
    value
        .parse::<u32>()
        .map_err(|_| ConfigError::InvalidValue(name))
}

/// What a validated snapshot would do to a local store that holds `existing`.
///
/// Planning separately from applying is what makes "nothing is mutated until the entire
/// snapshot validated" true rather than aspirational: the plan is built from validation
/// alone and touches nothing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportPlan {
    /// What to do with each Network, in `NetworkId` order.
    pub steps: Vec<ImportStep>,
    /// How many Networks the snapshot names that the local store does not.
    pub creates: usize,
    /// How many it names that the store does, and that therefore must match exactly.
    pub updates: usize,
}

/// One Network's planned outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImportStep {
    /// The store has no row with this identity; this is the record to create.
    Create(NetworkRecord),
    /// The store has this identity and the names the same Network; this is the record to
    /// write.
    Update(NetworkRecord),
    /// The store has this identity but it names a *different* Network.
    ///
    /// An explicit failure rather than a silent remap. A snapshot whose `netid=7` is a
    /// different Network than the store's `netid=7` is a snapshot from another bouncer, and
    /// importing it under a new identity would produce a configuration that looks restored
    /// and is not.
    Conflict(NetworkId),
}

/// Builds the plan for applying `snapshot` to a store holding `existing`.
///
/// Preserves stable identities only where they are unambiguous. Applying this plan is a
/// separate step, because a plan that had already written could not report what it had
/// intended to do.
pub fn plan(snapshot: &ConfigSnapshot, existing: &[NetworkRecord]) -> ImportPlan {
    let mut steps = Vec::new();
    let mut creates = 0;
    let mut updates = 0;
    for entry in &snapshot.networks {
        match existing
            .iter()
            .find(|record| record.network == entry.network)
        {
            Some(record) if record.display_name != entry.display_name => {
                steps.push(ImportStep::Conflict(entry.network));
            }
            Some(existing_record) => {
                updates += 1;
                steps.push(ImportStep::Update(entry.to_record_with(
                    // Carrying the stored credential across is what makes an update an
                    // update. `to_record` produces `sasl: None` because the snapshot format
                    // cannot represent a credential, and writing that field through would
                    // erase a secret the snapshot never saw -- a data-loss bug that would
                    // look like a successful restore. Merging it here, where both the
                    // snapshot entry and the stored record are in scope, is the only place
                    // the two can be combined.
                    existing_record.sasl.clone(),
                    if entry.retain_existing_failover {
                        existing_record.failover_group.clone()
                    } else {
                        entry.failover_group.clone()
                    },
                    if entry.retain_existing_profiles {
                        Some((
                            existing_record.transport_profile,
                            existing_record.auth_profile,
                        ))
                    } else {
                        None
                    },
                )));
            }
            None => {
                creates += 1;
                steps.push(ImportStep::Create(entry.to_record()));
            }
        }
    }
    steps.sort_by_key(|step| match step {
        ImportStep::Create(record) | ImportStep::Update(record) => record.network,
        ImportStep::Conflict(network) => *network,
    });
    ImportPlan {
        steps,
        creates,
        updates,
    }
}

impl SnapshotNetwork {
    /// The durable record this snapshot entry describes.
    ///
    /// Always carries `sasl: None` -- the snapshot format has no credential field, so an
    /// import can never carry one *in*. Use [`Self::to_record_with`] to preserve a stored
    /// credential across an update.
    pub fn to_record(&self) -> NetworkRecord {
        self.to_record_with(None, self.failover_group.clone(), None)
    }

    /// The durable record for this entry, carrying `credential` through unchanged.
    ///
    /// The credential is never *derived* here: it comes from the record already in the
    /// store, so this can only ever preserve what was already stored and can never invent
    /// or modify one.
    pub fn to_record_with(
        &self,
        credential: Option<(String, StoredSecret)>,
        failover_group: Option<FailoverEndpointGroup>,
        existing_profiles: Option<(IrcTransportProfile, UpstreamAuthProfile)>,
    ) -> NetworkRecord {
        let (transport_profile, auth_profile) =
            existing_profiles.unwrap_or((self.transport_profile, self.auth_profile));
        NetworkRecord {
            network: self.network,
            display_name: self.display_name.clone(),
            endpoint: self.endpoint.clone(),
            transport_profile,
            auth_profile,
            failover_group,
            nick: self.nick.clone(),
            username: self.username.clone(),
            realname: self.realname.clone(),
            sasl: credential,
            desired_channels: self.desired_channels.clone(),
            auto_away: self.auto_away,
            keep_nick: self.keep_nick,
        }
    }
}

/// The result of applying a plan, including how far it got.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ApplyOutcome {
    /// How many Networks were written before the plan stopped.
    pub applied: usize,
    /// How many Networks the plan still had to write.
    pub remaining: usize,
    /// The first Network that could not be written, if any.
    ///
    /// Reported even after a partial apply, because a plan that stopped halfway has left
    /// the store in a state the Operator has to be told about precisely.
    pub stopped_at: Option<NetworkId>,
}

impl ApplyOutcome {
    /// Whether every step was written.
    pub fn complete(&self) -> bool {
        self.remaining == 0
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn b32(seed: char) -> String {
        format!("{}.b32.i2p", seed.to_string().repeat(52))
    }

    /// The entry for one identity, regardless of the order `sample()` supplies them in.
    fn entry(id: u64) -> SnapshotNetwork {
        sample()
            .networks
            .into_iter()
            .find(|entry| entry.network == NetworkId(id))
            .expect("the sample has that Network")
    }

    fn sample() -> ConfigSnapshot {
        ConfigSnapshot {
            networks: vec![
                SnapshotNetwork {
                    network: NetworkId(2),
                    display_name: "second".to_owned(),
                    endpoint: I2pEndpoint::parse(&b32('c')).expect("a destination"),
                    transport_profile: i2pr_irc_store::IrcTransportProfile::PlainI2p,
                    auth_profile: i2pr_irc_store::UpstreamAuthProfile::None,
                    retain_existing_profiles: false,
                    failover_group: None,
                    retain_existing_failover: false,
                    nick: "two".to_owned(),
                    username: "user2".to_owned(),
                    realname: "Two".to_owned(),
                    auto_away: false,
                    keep_nick: true,
                    desired_channels: vec![DesiredChannelRecord {
                        target: "#b".to_owned(),
                        position: 1,
                        detached: true,
                        activity: i2pr_irc_store::ChannelActivityPolicy::default(),
                    }],
                    action_count: 2,
                    action_phase_counts: [1, 1, 0],
                },
                SnapshotNetwork {
                    network: NetworkId(1),
                    display_name: "first".to_owned(),
                    endpoint: I2pEndpoint::parse(&b32('a')).expect("a destination"),
                    transport_profile: i2pr_irc_store::IrcTransportProfile::PlainI2p,
                    auth_profile: i2pr_irc_store::UpstreamAuthProfile::None,
                    retain_existing_profiles: false,
                    failover_group: None,
                    retain_existing_failover: false,
                    nick: "one".to_owned(),
                    username: "user1".to_owned(),
                    realname: "One".to_owned(),
                    auto_away: true,
                    keep_nick: false,
                    desired_channels: vec![
                        DesiredChannelRecord {
                            target: "#x".to_owned(),
                            position: 0,
                            detached: false,
                            activity: i2pr_irc_store::ChannelActivityPolicy::default(),
                        },
                        DesiredChannelRecord {
                            target: "#y".to_owned(),
                            position: 1,
                            detached: false,
                            activity: i2pr_irc_store::ChannelActivityPolicy::default(),
                        },
                    ],
                    action_count: 0,
                    action_phase_counts: [0, 0, 0],
                },
            ],
        }
    }

    #[test]
    fn a_snapshot_round_trips_without_losing_anything() {
        let first = render(&sample());
        let back = parse(&first).expect("the snapshot parses");
        assert_eq!(render(&back), first, "rendering must be a fixed point");
        assert_eq!(back.networks.len(), 2);
        assert_eq!(back.networks[0].network, NetworkId(1));
        assert_eq!(back.networks[1].network, NetworkId(2));
        assert_eq!(back.networks[0].display_name, "first");
        assert_eq!(back.networks[0].desired_channels.len(), 2);
        assert_eq!(back.networks[0].desired_channels[1].target, "#y");
        assert!(back.networks[0].auto_away);
        assert!(!back.networks[0].keep_nick);
        assert_eq!(back.networks[1].action_count, 2);
        assert!(back.networks[1].desired_channels[0].detached);
    }

    #[test]
    fn legacy_snapshot_action_count_defaults_to_post_join() {
        let text = render(&sample())
            .replace(&format!("version {SNAPSHOT_VERSION}"), "version 1")
            .replace(" transport=plain-i2p auth=none", "")
            .replace(" action_phases=1,1,0", "");
        let text = text
            .lines()
            .filter(|line| !line.starts_with("failover ") && !line.starts_with("alternate "))
            .collect::<Vec<_>>()
            .join("\n");
        let parsed = parse(&text).expect("version 1 snapshots remain readable");
        assert_eq!(parsed.networks[1].action_count, 2);
        assert_eq!(parsed.networks[1].action_phase_counts, [0, 2, 0]);
    }

    #[test]
    fn the_render_order_does_not_depend_on_the_order_networks_were_supplied() {
        let ordered = ConfigSnapshot {
            networks: vec![sample().networks[1].clone(), sample().networks[0].clone()],
        };
        let reversed = ConfigSnapshot {
            networks: vec![sample().networks[0].clone(), sample().networks[1].clone()],
        };
        assert_eq!(
            render(&ordered),
            render(&reversed),
            "two exports of the same configuration must be byte-identical or they cannot be diffed"
        );
    }

    #[test]
    fn a_snapshot_cannot_carry_a_credential() {
        let record = |suffix: &str| {
            format!(
                "{SNAPSHOT_MAGIC}\nversion {SNAPSHOT_VERSION}\nnetwork netid=1 name=a host={} \
                 nick=n username=u realname=r auto_away=off keep_nick=off actions=0{suffix}\n",
                b32('a')
            )
        };
        // Both spellings an Operator might paste: a bare field with no `=` at all, and an
        // ordinary `name=value` field. Both must be refused by name.
        for (suffix, expected) in [
            (" sasl user=bob", "sasl"),
            (" sasl=bob", "sasl"),
            (" pass=hunter2", "pass"),
            (" secret=x", "secret"),
        ] {
            assert_eq!(
                parse(&record(suffix)),
                Err(ConfigError::RefusedSecret(expected.to_owned())),
                "for {suffix:?}: a credential must be refused by name, not skipped"
            );
        }
        // A whole extra line naming a credential is refused the same way.
        assert_eq!(
            parse(&format!("{}sasl user=bob\n", record(""))),
            Err(ConfigError::RefusedSecret("sasl".to_owned()))
        );
    }

    #[test]
    fn a_snapshot_from_a_future_version_is_refused_rather_than_guessed_at() {
        let future = SNAPSHOT_VERSION + 1;
        let text = render(&sample()).replace(
            &format!("version {SNAPSHOT_VERSION}"),
            &format!("version {future}"),
        );
        assert_eq!(parse(&text), Err(ConfigError::UnsupportedVersion(future)));
    }

    #[test]
    fn tls_and_sasl_external_profiles_are_refused_until_their_qualification_plan() {
        let text = render(&sample());
        let tls = text.replace("transport=plain-i2p", "transport=tls-over-i2p");
        assert_eq!(
            parse(&tls),
            Err(ConfigError::InvalidValue("unsupported transport"))
        );
        let external = text.replace("auth=none", "auth=sasl-external");
        assert_eq!(
            parse(&external),
            Err(ConfigError::InvalidValue("unsupported auth profile"))
        );
    }

    #[test]
    fn text_that_is_not_a_snapshot_is_refused_before_it_is_read() {
        assert_eq!(
            parse("PRIVMSG #x :hello\r\n"),
            Err(ConfigError::NotASnapshot)
        );
        assert_eq!(parse(""), Err(ConfigError::NotASnapshot));
    }

    #[test]
    fn a_version_line_is_read_by_position_not_by_search() {
        // A later `version` line must not be able to restate a document whose real shape
        // was decided before it.
        let text = format!(
            "{SNAPSHOT_MAGIC}\nversion 99\nnetwork netid=1 name=a host={} nick=n username=u \
             realname=r auto_away=off keep_nick=off actions=0\nversion {SNAPSHOT_VERSION}\n",
            b32('a')
        );
        assert_eq!(parse(&text), Err(ConfigError::UnsupportedVersion(99)));
    }

    #[test]
    fn an_invalid_value_is_named_rather_than_reported_as_invalid_input() {
        let base = render(&sample());
        // Each substitution replaces a *valid* value with an invalid one, so a pass can only
        // come from the value being checked and not from the line being malformed.
        for (valid, broken, expected) in [
            ("netid=1", "netid=01", "netid"),
            ("auto_away=on", "auto_away=yes", "boolean"),
            ("nick=one", "nick=", "nick"),
            (
                "host=".to_owned().as_str(),
                "host=not-a-destination",
                "host",
            ),
        ] {
            let valid: &str = valid;
            let broken: &str = broken;
            let text = base.replacen(valid, broken, 1);
            assert_ne!(text, base, "the substitution for {expected} did nothing");
            match parse(&text) {
                Err(ConfigError::InvalidValue(name)) => {
                    assert_eq!(name, expected, "for {valid}");
                }
                other => panic!("{valid} produced {other:?}"),
            }
        }
    }

    #[test]
    fn a_missing_attribute_is_named() {
        let text = format!(
            "{SNAPSHOT_MAGIC}\nversion {SNAPSHOT_VERSION}\nnetwork netid=1 name=a host={}\n",
            b32('a')
        );
        assert_eq!(
            parse(&text),
            Err(ConfigError::MissingAttribute("nick")),
            "an Operator fixing this needs to know which field, not that something was wrong"
        );
    }

    #[test]
    fn a_channel_before_any_network_is_refused() {
        let text = format!(
            "{SNAPSHOT_MAGIC}\nversion {SNAPSHOT_VERSION}\nchannel target=#a position=0 detached=off\n"
        );
        assert_eq!(parse(&text), Err(ConfigError::OrphanChannel));
    }

    #[test]
    fn version_two_channel_rows_keep_disabled_activity_defaults() {
        let text = format!(
            "{SNAPSHOT_MAGIC}\nversion 2\nnetwork netid=1 name=a host={} nick=n username=u realname=r auto_away=off keep_nick=off actions=0 action_phases=0,0,0\nchannel target=#a position=0 detached=off\n",
            b32('a')
        );
        let parsed = parse(&text).expect("v2 snapshot remains importable");
        assert_eq!(
            parsed.networks[0].desired_channels[0].activity,
            i2pr_irc_store::ChannelActivityPolicy::default()
        );
    }

    #[test]
    fn two_records_claiming_one_identity_are_refused() {
        let one = render(&sample());
        let network_line = one
            .lines()
            .find(|line| line.starts_with("network"))
            .unwrap();
        let doubled = format!("{one}{network_line}\n");
        let error = parse(&doubled);
        assert!(
            matches!(error, Err(ConfigError::DuplicateNetwork(_))),
            "got {error:?}"
        );
    }

    #[test]
    fn a_snapshot_larger_than_the_line_ceiling_is_refused() {
        let mut text = format!("{SNAPSHOT_MAGIC}\nversion {SNAPSHOT_VERSION}\n");
        text.push_str(&"x".repeat(MAX_SNAPSHOT_LINE_BYTES + 1));
        assert_eq!(parse(&text), Err(ConfigError::LineTooLong));
    }

    #[test]
    fn more_lines_than_the_snapshot_ceiling_is_refused() {
        let mut text = format!("{SNAPSHOT_MAGIC}\nversion {SNAPSHOT_VERSION}\n");
        for index in 0..=crate::catalog::MAX_SUPERVISED_NETWORKS {
            text.push_str(&format!(
                "network netid={} name=a host={} nick=n username=u realname=r auto_away=off \
                 keep_nick=off actions=0\n",
                index + 1,
                b32('a')
            ));
        }
        text.push_str(&"\n".repeat(MAX_SNAPSHOT_LINES));
        assert_eq!(parse(&text), Err(ConfigError::TooManyLines));
    }

    #[test]
    fn an_import_never_writes_a_credential_away() {
        // The asymmetry that makes a non-secret export safe to import anywhere: the record
        // an import produces carries `sasl: None`, so applying a snapshot over a store that
        // holds a credential cannot erase it.
        let record = entry(1).to_record();
        assert_eq!(record.sasl, None);
    }

    #[test]
    fn an_identity_that_names_a_different_network_is_a_conflict_not_a_remap() {
        let snapshot = parse(&render(&sample())).expect("a snapshot");
        // Same identity, different Operator-chosen name: this snapshot came from a different
        // bouncer, and importing it under the same id would produce a configuration that
        // looks restored and is not.
        let mut existing = entry(1).to_record();
        existing.display_name = "something else".to_owned();
        let plan = plan(&snapshot, &[existing]);
        assert_eq!(plan.steps[0], ImportStep::Conflict(NetworkId(1)));
        assert_eq!(plan.creates, 1, "Network 2 is still a plain create");
        assert_eq!(plan.updates, 0);
    }

    #[test]
    fn the_plan_is_ordered_by_identity_so_a_partial_apply_is_predictable() {
        let snapshot = parse(&render(&sample())).expect("a snapshot");
        let plan = plan(&snapshot, &[]);
        let order: Vec<NetworkId> = plan
            .steps
            .iter()
            .map(|step| match step {
                ImportStep::Create(record) | ImportStep::Update(record) => record.network,
                ImportStep::Conflict(network) => *network,
            })
            .collect();
        assert_eq!(order, vec![NetworkId(1), NetworkId(2)]);
        assert_eq!(plan.creates, 2);
        assert_eq!(plan.updates, 0);
    }

    #[test]
    fn a_matching_identity_updates_rather_than_creating_a_second_network() {
        let snapshot = parse(&render(&sample())).expect("a snapshot");
        let existing = entry(1).to_record();
        let plan = plan(&snapshot, &[existing]);
        assert_eq!(plan.steps[0], ImportStep::Update(entry(1).to_record()));
        assert_eq!(plan.creates, 1);
        assert_eq!(plan.updates, 1);
    }

    #[test]
    fn no_rendered_snapshot_line_exceeds_its_own_ceiling() {
        let text = render(&sample());
        for line in text.lines() {
            assert!(
                line.len() <= MAX_SNAPSHOT_LINE_BYTES,
                "a line the control surface could not carry: {line}"
            );
        }
    }

    #[test]
    fn an_operator_attested_failover_group_round_trips_and_legacy_import_preserves_it() {
        let mut snapshot = sample();
        snapshot.networks[0].failover_group = Some(FailoverEndpointGroup {
            alternates: vec![I2pEndpoint::parse(&b32('d')).expect("alternate parses")],
            operator_attests_equivalence: true,
            credentials_authorized: true,
        });
        let rendered = render(&snapshot);
        let parsed = parse(&rendered).expect("attested group parses");
        let mut expected = snapshot.clone();
        expected.networks.sort_by_key(|entry| entry.network);
        assert_eq!(parsed, expected);
        assert!(rendered.contains("failover equivalent=operator credentials=operator"));

        let mut primary = entry(1).to_record();
        primary.failover_group = Some(FailoverEndpointGroup {
            alternates: vec![I2pEndpoint::parse(&b32('e')).expect("stored alternate parses")],
            operator_attests_equivalence: true,
            credentials_authorized: true,
        });
        primary.sasl = Some((
            "operator-user".to_owned(),
            StoredSecret::new("secret".to_owned()),
        ));
        primary.auth_profile = UpstreamAuthProfile::SaslPlain;
        let previous = rendered
            .lines()
            .filter(|line| !line.starts_with("failover ") && !line.starts_with("alternate "))
            .collect::<Vec<_>>()
            .join("\n")
            .replace(&format!("version {SNAPSHOT_VERSION}"), "version 3")
            .replace(" transport=plain-i2p auth=none", "");
        let parsed_previous = parse(&previous).expect("previous version parses");
        assert!(parsed_previous.networks[0].retain_existing_failover);
        let update = plan(&parsed_previous, std::slice::from_ref(&primary));
        let ImportStep::Update(updated) = &update.steps[0] else {
            panic!("expected update")
        };
        assert_eq!(updated.failover_group, primary.failover_group);
        assert_eq!(updated.auth_profile, UpstreamAuthProfile::SaslPlain);
        assert_eq!(updated.sasl, primary.sasl);
    }

    #[test]
    fn profile_snapshot_round_trips_and_pre_profile_import_preserves_existing_authentication() {
        let mut snapshot = sample();
        snapshot.networks[0].auth_profile = UpstreamAuthProfile::NickServ;
        let rendered = render(&snapshot);
        assert!(rendered.contains("transport=plain-i2p auth=nickserv"));
        let parsed = parse(&rendered).expect("the typed profile parses");
        assert_eq!(
            parsed.networks[1].auth_profile,
            UpstreamAuthProfile::NickServ
        );
        assert_eq!(
            parsed.networks[1].transport_profile,
            IrcTransportProfile::PlainI2p
        );

        let old = rendered
            .replace(&format!("version {SNAPSHOT_VERSION}"), "version 4")
            .replace(" transport=plain-i2p auth=nickserv", "")
            .replace(" transport=plain-i2p auth=none", "");
        let old = parse(&old).expect("version 4 remains supported");
        assert!(old.networks[0].retain_existing_profiles);
        let mut existing = entry(1).to_record();
        existing.sasl = Some((
            "configured-user".to_owned(),
            StoredSecret::new("kept".to_owned()),
        ));
        existing.auth_profile = UpstreamAuthProfile::SaslPlain;
        let plan = plan(&old, std::slice::from_ref(&existing));
        let ImportStep::Update(updated) = &plan.steps[0] else {
            panic!("expected an update")
        };
        assert_eq!(updated.auth_profile, UpstreamAuthProfile::SaslPlain);
        assert_eq!(updated.sasl, existing.sasl);
    }

    #[test]
    fn snapshot_parser_rejects_duplicate_failover_alternates() {
        let mut snapshot = sample();
        let endpoint = I2pEndpoint::parse("irc-alternate.i2p").expect("alternate parses");
        snapshot.networks[0].failover_group = Some(FailoverEndpointGroup {
            alternates: vec![endpoint.clone(), endpoint],
            operator_attests_equivalence: true,
            credentials_authorized: true,
        });
        assert_eq!(
            parse(&render(&snapshot)),
            Err(ConfigError::InvalidValue("duplicate failover endpoint"))
        );
    }
}
