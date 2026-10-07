//! Generation-owned observed IRC state.
//!
//! Everything here is bounded, owned by one upstream connection generation, and
//! retained across downstream detach. ISUPPORT values drive channel, membership,
//! and mode interpretation. Where the server has not declared semantics, the state
//! records that explicitly instead of inventing a value.
//!
//! Three distinct categories of channel knowledge are kept apart and must never be
//! conflated:
//!
//! - [`NetworkState::desired_channels`] is durable operator intent. It survives a
//!   rejected join and a generation replacement.
//! - [`NetworkState::join_attempts`] is generation-local bookkeeping for a JOIN that
//!   was written upstream. Writing a command proves nothing, so this records only that
//!   the outcome is still outstanding, or which standard numeric rejected it.
//! - [`NetworkState::self_channels`] is observed membership, added only by an
//!   authoritative server event such as a self JOIN and removed by self PART/KICK.
//!
//! A fourth distinction sits on top of the first: a desired channel may be *detached*,
//! meaning it stays joined and still collects history while its live presentation is
//! suppressed for attached sessions. Membership and presentation are separate facts, and
//! only [`NetworkState::visible_channels`] may be shown to a client.
use i2pr_irc_core::Casemapping;
use std::collections::{BTreeMap, BTreeSet};

/// One channel of durable intent handed to a generation: what to hold, and whether to
/// show it.
///
/// The runtime keeps its own copy so a generation never borrows a store handle or a
/// record that could change underneath it. The supervisor re-reads it at each generation
/// boundary, which is what makes a mid-generation detach survive a reconnect.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesiredChannelPolicy {
    pub target: String,
    pub detached: bool,
}
impl DesiredChannelPolicy {
    pub fn attached(target: impl Into<String>) -> Self {
        Self {
            target: target.into(),
            detached: false,
        }
    }
    pub fn detached(target: impl Into<String>) -> Self {
        Self {
            target: target.into(),
            detached: true,
        }
    }
}

pub const MAX_CHANNELS: usize = 128;
pub const MAX_MEMBERS_PER_CHANNEL: usize = 2048;
pub const MAX_TOTAL_MEMBERS: usize = 8192;
pub const MAX_ISUPPORT_TOKENS: usize = 128;
pub const MAX_CHANNEL_NAME_BYTES: usize = 200;
pub const MAX_ISUPPORT_TOKEN_BYTES: usize = 64;
pub const MAX_TOPIC_BYTES: usize = 400;
pub const MAX_PREFIX_PAIRS: usize = 8;
pub const MAX_CHANTYPE_SYMBOLS: usize = 8;
pub const MAX_MODE_LETTERS: usize = 64;
pub const MAX_MODE_ARGS: usize = 16;
pub const MAX_MODE_ARG_BYTES: usize = 100;

/// Ceiling on one observed account name.
///
/// The server supplies this field and the bouncer retains it verbatim, so the ceiling is
/// what stops a single `ACCOUNT` line from becoming an unbounded allocation. An account
/// name is a service-assigned identifier rather than prose, which is why this sits well
/// below the realname and away ceilings below.
pub const MAX_ACCOUNT_BYTES: usize = 64;
/// Ceiling on one observed realname, independent of what the server advertises.
///
/// `setname` requires the server to publish a `NAMELEN` token, and when it does that
/// value is honoured as well. This is the hard ceiling that stands when it does not,
/// because a realname is free text in a field the bouncer cannot otherwise bound.
pub const MAX_REALNAME_BYTES: usize = 128;
/// Ceiling on one observed away message.
///
/// This bounds what *other* members put in the field. It is deliberately a separate
/// constant from the Operator's own `MAX_AWAY_TEXT_BYTES`: that one bounds the words
/// this bouncer writes upstream on the Operator's behalf, and this one bounds text that
/// arrived from elsewhere and is on its way to a client.
pub const MAX_AWAY_MESSAGE_BYTES: usize = 200;

/// Conservative pre-connection channel-type assumption. Live state uses the
/// server-advertised `CHANTYPES` once `005` supplies one.
pub const DEFAULT_CHANTYPES: &str = "#&";
/// ISUPPORT default membership mapping, used only until `PREFIX` is advertised.
const DEFAULT_PREFIX: &str = "(ov)@+";
/// Community default channel-mode classes, used only until `CHANMODES` is
/// advertised: A and B always take an argument, C takes one only when set,
/// and D never takes one.
const DEFAULT_CHANMODES: &str = "beI,k,l,imnpst";

/// Standard channel-failure numerics that a server may return for a JOIN and whose
/// reply identifies the refused channel. Network-specific numerics stay ordinary
/// server events because nothing specified here can classify them.
pub const JOIN_FAILURE_NUMERICS: [&str; 7] = ["403", "405", "471", "473", "474", "475", "476"];

/// Generation-local disposition of one written desired JOIN.
///
/// This is deliberately not membership state: it exists so a failed attempt is
/// explicit rather than silent, and so a rejected join can never be projected as a
/// successful one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JoinAttempt {
    /// The JOIN bytes were written and the server has neither confirmed nor
    /// rejected them yet.
    Pending,
    /// The server rejected this attempt with a standard channel-failure numeric.
    Rejected(&'static str),
}

/// Reaction the upstream owner must take after applying one server line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LineOutcome {
    /// No reply is owed.
    Quiet,
    /// The server PING must be answered with this token.
    ReplyPong(String),
    /// The line is not a legal message for this connection.
    Malformed,
}

/// Whether a channel mode consumes an argument on a given delta.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModeArity {
    Always,
    WhenSet,
    Never,
}

/// Bounded `PREFIX=(modes)prefixes` membership mapping.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrefixMap {
    /// Highest-ranked membership mode first, exactly as advertised.
    pairs: Vec<(char, char)>,
}
impl Default for PrefixMap {
    fn default() -> Self {
        Self::parse(DEFAULT_PREFIX).expect("static PREFIX default is well formed")
    }
}
impl PrefixMap {
    /// Validates cardinality, uniqueness, and the total mapping count. An invalid
    /// value is rejected in full rather than partially applied.
    pub fn parse(value: &str) -> Option<Self> {
        let body = value.strip_prefix('(')?;
        let close = body.find(')')?;
        let modes: Vec<char> = body[..close].chars().collect();
        let symbols: Vec<char> = body[close + 1..].chars().collect();
        if modes.is_empty()
            || modes.len() != symbols.len()
            || modes.len() > MAX_PREFIX_PAIRS
            || modes.iter().any(|c| !c.is_ascii_alphabetic())
            || symbols.iter().any(|c| !c.is_ascii_graphic() && *c != ' ')
            || modes.iter().collect::<BTreeSet<_>>().len() != modes.len()
            || symbols.iter().collect::<BTreeSet<_>>().len() != symbols.len()
        {
            return None;
        }
        Some(Self {
            pairs: modes.into_iter().zip(symbols).collect(),
        })
    }
    pub fn mode_for(&self, symbol: char) -> Option<char> {
        self.pairs
            .iter()
            .find(|(_, advertised)| *advertised == symbol)
            .map(|(mode, _)| *mode)
    }
    pub fn symbol_for(&self, mode: char) -> Option<char> {
        self.pairs
            .iter()
            .find(|(advertised, _)| *advertised == mode)
            .map(|(_, symbol)| *symbol)
    }
    pub fn is_membership_mode(&self, mode: char) -> bool {
        self.symbol_for(mode).is_some()
    }
    /// The advertised rank of a membership mode, highest first.
    fn rank_of_mode(&self, mode: char) -> Option<usize> {
        self.pairs
            .iter()
            .position(|(advertised, _)| *advertised == mode)
    }
    /// The highest-ranked symbol in `symbols`, ignoring anything the server never
    /// advertised in its `PREFIX` map.
    ///
    /// This is the one-symbol view every client that did not negotiate `multi-prefix` is
    /// entitled to, so it is defined once here rather than as "the first character" at
    /// each call site.
    pub fn highest(&self, symbols: &[char]) -> Option<char> {
        symbols
            .iter()
            .copied()
            .filter_map(|symbol| {
                self.rank_of_mode(self.mode_for(symbol)?)
                    .map(|rank| (rank, symbol))
            })
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, symbol)| symbol)
    }
    /// Splits an advertised membership symbol run from a NAMES/member entry and
    /// reports the member's symbols in rank order, highest first.
    ///
    /// A `multi-prefix` server sends the member's complete run; without it the run holds
    /// only the highest symbol. Both are read here rather than at the call site, because
    /// "which symbols did this entry actually claim" is a property of the observation and
    /// not of who is going to look at it. Arbitrary leading punctuation is never treated
    /// as a prefix.
    ///
    /// The run is re-sorted into rank order and de-duplicated rather than taken in the
    /// order it arrived. `multi-prefix` requires rank order, but a server that violated it
    /// would otherwise let a reordered prefix change which symbol a legacy client is shown
    /// as its highest.
    pub fn split_prefix_run<'a>(&self, entry: &'a str) -> (Vec<char>, &'a str) {
        let mut ranked: Vec<(usize, char)> = Vec::new();
        let mut index = 0;
        for (offset, symbol) in entry.char_indices() {
            let Some(mode) = self.mode_for(symbol) else {
                break;
            };
            index = offset + symbol.len_utf8();
            let Some(rank) = self.rank_of_mode(mode) else {
                continue;
            };
            if !ranked.iter().any(|(existing, _)| *existing == rank) {
                ranked.push((rank, symbol));
            }
        }
        ranked.sort_by_key(|(rank, _)| *rank);
        (
            ranked
                .into_iter()
                .take(MAX_PREFIX_PAIRS)
                .map(|(_, symbol)| symbol)
                .collect(),
            &entry[index..],
        )
    }
    /// Merges two observed membership symbol runs into one, in rank order.
    ///
    /// Used when a member is observed twice: a `MODE` delta adds a symbol to what a NAMES
    /// entry already established, and neither observation may discard the other. The
    /// union is capped at the same ceiling as a single observation, so repeated deltas
    /// cannot grow a member's prefix run without bound.
    pub fn merge_symbols(&self, left: &[char], right: &[char]) -> Vec<char> {
        let mut ranked: Vec<(usize, char)> = Vec::new();
        for symbol in left.iter().chain(right.iter()) {
            let Some(mode) = self.mode_for(*symbol) else {
                continue;
            };
            let Some(rank) = self.rank_of_mode(mode) else {
                continue;
            };
            if !ranked.iter().any(|(existing, _)| *existing == rank) {
                ranked.push((rank, *symbol));
            }
        }
        ranked.sort_by_key(|(rank, _)| *rank);
        ranked.truncate(MAX_PREFIX_PAIRS);
        ranked.into_iter().map(|(_, symbol)| symbol).collect()
    }
}

/// Bounded `CHANTYPES` channel-type set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChanTypes(Vec<char>);
impl Default for ChanTypes {
    fn default() -> Self {
        Self::parse(DEFAULT_CHANTYPES).expect("static CHANTYPES default is well formed")
    }
}
impl ChanTypes {
    pub fn parse(value: &str) -> Option<Self> {
        let symbols: Vec<char> = value.chars().collect();
        if symbols.is_empty()
            || symbols.len() > MAX_CHANTYPE_SYMBOLS
            || symbols.iter().any(|c| !c.is_ascii_graphic())
            || symbols.iter().collect::<BTreeSet<_>>().len() != symbols.len()
        {
            return None;
        }
        Some(Self(symbols))
    }
    /// True when `target` is a channel under the currently advertised types.
    pub fn is_channel(&self, target: &str) -> bool {
        target.starts_with(|c: char| self.0.contains(&c))
    }
}

/// Bounded `CHANMODES=A,B,C,D` parameter-consumption classes.
///
/// Group A (list modes) and group B (always-parameter settings) always consume an
/// argument; group C consumes one only when the mode is set; group D never does.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChanModes {
    always: Vec<char>,
    when_set: Vec<char>,
    never: Vec<char>,
}
impl Default for ChanModes {
    fn default() -> Self {
        Self::parse(DEFAULT_CHANMODES).expect("static CHANMODES default is well formed")
    }
}
impl ChanModes {
    /// Parses `CHANMODES=A,B,C,D`. A malformed or oversized value is rejected and
    /// the retained mapping stays authoritative.
    pub fn parse(value: &str) -> Option<Self> {
        let groups: Vec<&str> = value.split(',').collect();
        if groups.len() != 4 {
            return None;
        }
        let mut letters: Vec<Vec<char>> = Vec::with_capacity(4);
        for group in groups {
            let group: Vec<char> = group.chars().collect();
            if group.len() > MAX_MODE_LETTERS
                || group.iter().any(|c| !c.is_ascii_alphabetic())
                || group.iter().collect::<BTreeSet<_>>().len() != group.len()
            {
                return None;
            }
            letters.push(group);
        }
        let mut groups = letters.into_iter();
        let list_modes = groups.next().expect("four groups");
        let always_modes = groups.next().expect("four groups");
        let when_set = groups.next().expect("four groups");
        let never = groups.next().expect("four groups");
        let always: Vec<char> = list_modes
            .iter()
            .chain(always_modes.iter())
            .copied()
            .collect();
        let mut unique = BTreeSet::new();
        for mode in always.iter().chain(when_set.iter()).chain(never.iter()) {
            unique.insert(*mode);
        }
        if unique.len() != always.len() + when_set.len() + never.len() {
            return None;
        }
        Some(Self {
            always,
            when_set,
            never,
        })
    }
    /// Argument consumption for a mode letter, or `None` when the server has not
    /// declared semantics for it.
    pub fn arity(&self, mode: char) -> Option<ModeArity> {
        if self.always.contains(&mode) {
            Some(ModeArity::Always)
        } else if self.when_set.contains(&mode) {
            Some(ModeArity::WhenSet)
        } else if self.never.contains(&mode) {
            Some(ModeArity::Never)
        } else {
            None
        }
    }
}

/// One retained channel mode and the arguments required to reconstruct it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModeValue {
    Flag,
    Arguments(Vec<String>),
}

/// Channel mode state that can reconstruct a truthful `324`, or that declares
/// itself incomplete so that no `324` is synthesized.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModeSnapshot {
    entries: BTreeMap<char, ModeValue>,
    complete: bool,
}
impl Default for ModeSnapshot {
    /// A fresh snapshot can still be completed truthfully.
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
            complete: true,
        }
    }
}
impl ModeSnapshot {
    /// A fresh snapshot that can still be completed truthfully.
    pub fn empty() -> Self {
        Self::default()
    }
    pub fn is_complete(&self) -> bool {
        self.complete
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn mark_incomplete(&mut self) {
        self.complete = false;
    }
    /// Retains one mode with its arguments. A ceiling breach makes the snapshot
    /// incomplete rather than silently dropping a required parameter.
    pub fn set(&mut self, mode: char, arguments: Vec<String>) {
        if self.entries.len() >= MAX_MODE_LETTERS && !self.entries.contains_key(&mode) {
            self.complete = false;
            return;
        }
        if arguments.len() > MAX_MODE_ARGS
            || arguments
                .iter()
                .any(|value| value.len() > MAX_MODE_ARG_BYTES || value.contains(['\0', '\r', '\n']))
        {
            self.complete = false;
            return;
        }
        self.entries.insert(
            mode,
            if arguments.is_empty() {
                ModeValue::Flag
            } else {
                ModeValue::Arguments(arguments)
            },
        );
    }
    pub fn clear(&mut self, mode: char) {
        self.entries.remove(&mode);
    }
    /// Replaces the whole snapshot, as an authoritative server mode line does.
    pub fn replace(&mut self, other: &ModeSnapshot) {
        self.entries = other.entries.clone();
        self.complete = other.complete;
    }
    /// Mode letters plus arguments, or `None` while the snapshot is incomplete.
    pub fn render(&self) -> Option<String> {
        if !self.complete {
            return None;
        }
        let mut letters = String::with_capacity(self.entries.len());
        let mut arguments: Vec<&str> = Vec::new();
        for (mode, value) in &self.entries {
            letters.push(*mode);
            if let ModeValue::Arguments(values) = value {
                arguments.extend(values.iter().map(String::as_str));
            }
        }
        if arguments.is_empty() {
            Some(letters)
        } else {
            Some(format!("{} {}", letters, arguments.join(" ")))
        }
    }
}

/// What is known about the account a member identified with.
///
/// The three states are genuinely different answers. `Unknown` means nothing has been
/// observed, which is not the same statement as "is not logged in": a member who joined
/// before the bouncer negotiated `extended-join` has no account, while a member whose
/// extended JOIN carried `*` has been *observed* not to be logged in. Collapsing them
/// would let the bouncer present a member as logged out because it never looked.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum AccountState {
    /// No account observation has been made for this member.
    #[default]
    Unknown,
    /// The member's account state was observed.
    ///
    /// The inner `None` is an observed logout; `Some` is an observed login.
    Known(Option<String>),
}
impl AccountState {
    /// Reads one account parameter, where `*` is the spec's logged-out sentinel.
    ///
    /// Returns `None` when the parameter is not a usable account name, so an
    /// unrepresentable value is omitted rather than retained as something a client
    /// would read as a real account.
    pub fn observed(raw: &str) -> Self {
        if raw == "*" {
            return Self::Known(None);
        }
        let account = raw.trim();
        if account.is_empty()
            || account.len() > MAX_ACCOUNT_BYTES
            || account.bytes().any(|byte| {
                byte == 0 || byte == b'\r' || byte == b'\n' || byte.is_ascii_whitespace()
            })
        {
            return Self::Unknown;
        }
        Self::Known(Some(account.to_owned()))
    }
}

/// Whether a member was observed away, and the message they gave for it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AwayState {
    /// Observed present.
    Here,
    /// Observed away with a bounded message.
    Away(String),
}

/// One observed channel member and whatever richer metadata was learned about them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemberEntry {
    pub nick: String,
    /// Membership symbols held by this member, highest rank first.
    ///
    /// A `multi-prefix` observation fills this with the complete run; a plain one leaves
    /// only the highest symbol, which is what the server said and therefore still the
    /// truth about what was claimed.
    pub symbols: Vec<char>,
    /// True when `symbols` is the member's complete set rather than its highest alone.
    ///
    /// False for any member first observed without `multi-prefix`, and a `MODE` delta can
    /// add a symbol to a complete set but can never complete an incomplete one.
    pub symbols_complete: bool,
    /// What is known about this member's account.
    pub account: AccountState,
    /// The realname, from an extended JOIN or a `SETNAME`. Absent when never observed.
    pub realname: Option<String>,
    /// Observed away state. Absent when nothing has been observed.
    pub away: Option<AwayState>,
}
impl Default for MemberEntry {
    fn default() -> Self {
        Self {
            nick: String::new(),
            symbols: Vec::new(),
            symbols_complete: false,
            account: AccountState::Unknown,
            realname: None,
            away: None,
        }
    }
}
impl MemberEntry {
    /// Renders the NAMES entry for this member at one session's negotiated surface.
    ///
    /// A complete run is shown only to a session that negotiated `multi-prefix` *and* for a
    /// member whose run was actually observed completely. Both conditions are required, and
    /// the reason matters: a client that negotiated `multi-prefix` reads an absent symbol as
    /// an absent mode, so showing it a partial run would tell it the member holds no other
    /// modes. That is a claim about membership the bouncer cannot make about a run it never
    /// saw whole.
    ///
    /// Anything else yields the single highest symbol. That symbol was genuinely observed,
    /// and it is the whole of what a view without `multi-prefix` can represent -- so
    /// omitting it would understate what the bouncer knows, and widening it would overstate
    /// what the server said.
    pub fn display(&self, multi_prefix: bool) -> String {
        let complete = multi_prefix && self.symbols_complete;
        let symbols: String = if complete {
            self.symbols.iter().collect()
        } else {
            self.symbols
                .first()
                .map(|symbol| symbol.to_string())
                .unwrap_or_default()
        };
        format!("{symbols}{}", self.nick)
    }
    /// The account name to render in an extended JOIN, if one is known.
    ///
    /// `Some(None)` is the spec's `*`, which is the correct rendering for a member observed
    /// to be logged out. `None` means the account was never observed, and the caller must
    /// fall back to a plain JOIN rather than claim a logout that was not observed.
    pub fn account_field(&self) -> Option<Option<&str>> {
        match &self.account {
            AccountState::Unknown => None,
            AccountState::Known(account) => Some(account.as_deref()),
        }
    }
}

/// The bounded facts a single membership observation contributes to one member.
///
/// Grouped rather than passed as a widening argument list, because every field here is
/// "something the server may or may not have told us", and a caller that omitted one
/// would silently convert it into an assertion that it is unknown.
#[derive(Clone, Debug, Default)]
pub struct MemberObservation {
    /// Membership symbols this observation claimed, in rank order.
    pub symbols: Vec<char>,
    /// True when this observation claimed the member's complete set.
    pub symbols_complete: bool,
    pub account: AccountState,
    pub realname: Option<String>,
}

impl MemberObservation {
    /// An observation that says only "this member is here", from a plain NAMES entry or a
    /// JOIN with no membership prefix.
    pub fn membership_only() -> Self {
        Self::default()
    }
}

#[derive(Clone, Debug)]
pub struct ChannelState {
    pub topic: Option<String>,
    pub members: Vec<MemberEntry>,
    /// A `353` observation has been seen for this channel.
    pub names_seen: bool,
    /// Membership is fully known, so a NAMES projection is truthful. A newly
    /// observed channel is not yet known to be incomplete; breaches set it false.
    pub members_complete: bool,
    pub modes: ModeSnapshot,
}
impl Default for ChannelState {
    fn default() -> Self {
        Self {
            topic: None,
            members: Vec::new(),
            names_seen: false,
            members_complete: true,
            modes: ModeSnapshot::default(),
        }
    }
}

/// Live state learned from one upstream generation.
#[derive(Clone, Debug)]
pub struct NetworkState {
    pub nick: String,
    pub casemapping: Casemapping,
    pub chantypes: ChanTypes,
    pub prefix: PrefixMap,
    pub chanmodes: ChanModes,
    pub isupport: BTreeSet<String>,
    pub channels: BTreeMap<String, ChannelState>,
    /// Channels this bouncer currently holds on the network, learned only from
    /// authoritative server events. Command emission never adds to this set.
    pub self_channels: BTreeSet<String>,
    /// Durable operator intent, preserved across generations by the supervisor and
    /// never removed by a join failure.
    pub desired_channels: Vec<String>,
    /// Casemapped identities of desired channels whose live presentation is suppressed.
    ///
    /// This is the bouncer-owned decision, not a server-observed fact: the channel is
    /// still joined and still in `self_channels`, so history, presence, and upstream
    /// membership are unaffected. Only what a local session is shown changes.
    detached: BTreeSet<Vec<u8>>,
    /// Channels whose membership was confirmed while detached and which must therefore
    /// be projected to attached sessions the moment they are reattached.
    ///
    /// Without this, reattaching a channel the server had quietly parted upstream would
    /// show a live channel with no topic, modes, or names, because the synthetic JOIN
    /// alone would claim a membership the client never saw established.
    pending_reveal: BTreeSet<Vec<u8>>,
    /// Bounded generation-local record of written desired JOINs, keyed by casemapped
    /// channel identity. Discarded when the generation is replaced.
    join_attempts: BTreeMap<Vec<u8>, (String, JoinAttempt)>,
    /// True when the server acknowledged `multi-prefix` for this generation.
    ///
    /// This decides whether an observed prefix run is the member's complete set or only
    /// its highest symbol. It is generation-local because it is a property of one
    /// negotiation: a reconnect that failed to negotiate `multi-prefix` genuinely has
    /// less membership information, and must not inherit the previous generation's claim.
    multi_prefix: bool,
    /// The realname ceiling the server advertised through `NAMELEN`.
    ///
    /// `setname` requires the server to publish it, so its absence is not itself a fault.
    /// It is recorded because honouring it is what stops the bouncer retaining a realname
    /// the server would have rejected.
    namelen: Option<usize>,
}
impl NetworkState {
    /// Builds generation-local state from durable channel intent.
    ///
    /// `desired` is read from storage at each generation, not from whatever the record
    /// said when the owner started: a JOIN or a detach performed mid-generation is part
    /// of durable intent and a reconnect must restore it, not the owner's birth record.
    pub fn new(nick: &str, desired: &[DesiredChannelPolicy]) -> Self {
        Self {
            nick: nick.to_owned(),
            casemapping: Casemapping::Rfc1459,
            chantypes: ChanTypes::default(),
            prefix: PrefixMap::default(),
            chanmodes: ChanModes::default(),
            isupport: BTreeSet::new(),
            channels: BTreeMap::new(),
            self_channels: BTreeSet::new(),
            desired_channels: desired.iter().map(|entry| entry.target.clone()).collect(),
            detached: desired
                .iter()
                .filter(|entry| entry.detached)
                .map(|entry| Casemapping::Rfc1459.fold(entry.target.as_bytes()))
                .collect(),
            pending_reveal: BTreeSet::new(),
            join_attempts: BTreeMap::new(),
            multi_prefix: false,
            namelen: None,
        }
    }
    /// Records what the upstream negotiation agreed for member metadata.
    ///
    /// Called once per generation as the `CAP` acknowledgement is applied, and never from
    /// a downstream path: which capabilities the *server* agreed is not something a client
    /// attaching can influence.
    pub fn set_upstream_multi_prefix(&mut self, negotiated: bool) {
        self.multi_prefix = negotiated;
    }
    /// True when this generation's membership observations carry complete prefix runs.
    pub fn upstream_multi_prefix(&self) -> bool {
        self.multi_prefix
    }
    /// The realname ceiling to apply to an observation from this server.
    pub fn namelen_ceiling(&self) -> usize {
        match self.namelen {
            Some(advertised) => advertised.clamp(1, MAX_REALNAME_BYTES),
            None => MAX_REALNAME_BYTES,
        }
    }
    /// True when the server published its own `NAMELEN`.
    ///
    /// `setname` obliges the server to publish one, and the bouncer relays what the
    /// server said. It therefore owes a ceiling of its own only when nobody upstream
    /// supplied one -- publishing a second `NAMELEN` beside a relayed one would put two
    /// answers to the same question in one `005`.
    pub fn upstream_published_namelen(&self) -> bool {
        self.namelen.is_some()
    }
    fn bounded_realname(&self, raw: &str) -> Option<String> {
        if raw.is_empty() || raw.len() > self.namelen_ceiling() {
            return None;
        }
        if raw
            .bytes()
            .any(|byte| byte == 0 || byte == b'\r' || byte == b'\n')
        {
            return None;
        }
        Some(raw.to_owned())
    }
    /// The observed account name and realname for this bouncer's own nick, if any channel
    /// holds this bouncer's own membership.
    ///
    /// An extended-join projection of our own channel JOIN needs exactly these two fields,
    /// and it must not invent them: a generation that joined before `extended-join` was
    /// negotiated has neither.
    pub fn own_profile(&self) -> (AccountState, Option<String>) {
        for state in self.channels.values() {
            for member in &state.members {
                if self.same_nick(&member.nick, &self.nick) {
                    return (member.account.clone(), member.realname.clone());
                }
            }
        }
        (AccountState::Unknown, None)
    }
    /// Suppresses downstream presentation of `channel` without touching membership.
    ///
    /// Returns false when the channel was already detached, so a caller can avoid
    /// emitting a synthetic transition that would claim something did not change.
    pub fn detach(&mut self, channel: &str) -> bool {
        let key = self.casemapping.fold(channel.as_bytes());
        let fresh = self.detached.insert(key.clone());
        self.pending_reveal.remove(&key);
        fresh
    }
    /// Restores downstream presentation of `channel`.
    ///
    /// Returns false when the channel was not detached, for the same reason as
    /// [`NetworkState::detach`]. Membership is untouched: if the server has not
    /// confirmed this bouncer holds the channel, the reveal waits for that event.
    pub fn reattach(&mut self, channel: &str) -> bool {
        let key = self.casemapping.fold(channel.as_bytes());
        if !self.detached.remove(&key) {
            return false;
        }
        if self
            .self_channels
            .iter()
            .any(|held| self.casemapping.fold(held.as_bytes()) == key)
        {
            self.pending_reveal.remove(&key);
        } else {
            self.pending_reveal.insert(key);
        }
        true
    }
    /// The `MONITOR` limit the server advertised, if any.
    ///
    /// Read from ISUPPORT rather than negotiated, so the upstream CAP fingerprint stays
    /// independent of which clients happen to be attached. `MONITOR=0` is reported as
    /// `Some(0)`: the server did advertise the feature and disabled it, which is a
    /// different answer from never mentioning it.
    pub fn monitor_limit(&self) -> Option<usize> {
        self.isupport
            .iter()
            .find_map(|token| token.strip_prefix("MONITOR="))
            .and_then(|value| value.parse::<usize>().ok())
    }

    /// Casemapped identities of every detached channel, whether or not it is currently
    /// joined. A detached channel the server has parted is still detached: the policy is
    /// durable and does not lapse because membership ended.
    pub fn detached_channels(&self) -> Vec<String> {
        self.detached
            .iter()
            .map(|folded| String::from_utf8_lossy(folded).into_owned())
            .collect()
    }
    pub fn is_detached(&self, channel: &str) -> bool {
        self.detached
            .contains(&self.casemapping.fold(channel.as_bytes()))
    }
    /// True when `channel`'s membership was confirmed while it was detached.
    pub fn is_reveal_pending(&self, channel: &str) -> bool {
        self.pending_reveal
            .contains(&self.casemapping.fold(channel.as_bytes()))
    }
    /// Clears a pending reveal, called once the channel has been projected.
    pub fn clear_reveal(&mut self, channel: &str) {
        self.pending_reveal
            .remove(&self.casemapping.fold(channel.as_bytes()));
    }
    /// Observed membership that may be presented downstream right now.
    ///
    /// This is the single accessor every projection and fanout decision reads. Detached
    /// channels are excluded here rather than filtered at each call site, so a new
    /// downstream-facing path cannot accidentally bypass the policy.
    pub fn visible_channels(&self) -> Vec<String> {
        self.self_channels
            .iter()
            .filter(|channel| !self.is_detached(channel))
            .cloned()
            .collect()
    }
    /// True when a casemapped channel identity from a buffer table is detached.
    ///
    /// Buffer tables are keyed by casemapped identity, so this is how a history buffer
    /// for a detached channel is kept out of a downstream replay.
    pub fn is_detached_key(&self, folded: &str) -> bool {
        self.detached
            .contains(&self.casemapping.fold(folded.as_bytes()))
    }
    pub fn same_nick(&self, left: &str, right: &str) -> bool {
        self.casemapping.fold(left.as_bytes()) == self.casemapping.fold(right.as_bytes())
    }
    /// Observed membership only. Desired intent and unconfirmed attempts are
    /// deliberately excluded so a projection can never claim an unjoined channel.
    pub fn joined_channels(&self) -> Vec<String> {
        self.self_channels.iter().cloned().collect()
    }
    /// Records that a desired JOIN for `channel` is being written upstream. This
    /// opens an outstanding attempt, never membership: only a self JOIN closes it as
    /// confirmed and only a recognized rejection closes it as failed.
    pub fn begin_desired_join(&mut self, channel: &str) -> bool {
        self.record_join_attempt(channel, JoinAttempt::Pending)
    }
    /// Records a standard channel-failure rejection for `channel`. Observed
    /// membership is untouched, so a refused join can never be projected as joined.
    pub fn reject_desired_join(&mut self, channel: &str, numeric: &'static str) {
        self.record_join_attempt(channel, JoinAttempt::Rejected(numeric));
    }
    /// Forgets any attempt record for `channel`, used when authoritative membership
    /// confirms the channel or when a generation is replaced.
    pub fn clear_join_attempt(&mut self, channel: &str) {
        let key = self.casemapping.fold(channel.as_bytes());
        self.join_attempts.remove(&key);
    }
    /// Every generation-local attempt with its channel name, ordered by casemapped
    /// identity so diagnostics and tests are deterministic.
    pub fn join_attempts(&self) -> Vec<(String, JoinAttempt)> {
        self.join_attempts
            .values()
            .map(|(name, attempt)| (name.clone(), *attempt))
            .collect()
    }
    /// Desired channels whose JOIN outcome the server has not yet reported.
    pub fn pending_joins(&self) -> Vec<String> {
        self.join_attempts
            .values()
            .filter(|(_, attempt)| *attempt == JoinAttempt::Pending)
            .map(|(name, _)| name.clone())
            .collect()
    }
    /// Desired channels this generation failed to join, with the numeric that
    /// refused them. Bounded non-secret diagnostic state, never configuration.
    pub fn rejected_joins(&self) -> Vec<(String, &'static str)> {
        self.join_attempts
            .values()
            .filter_map(|(name, attempt)| match attempt {
                JoinAttempt::Pending => None,
                JoinAttempt::Rejected(numeric) => Some((name.clone(), *numeric)),
            })
            .collect()
    }
    fn record_join_attempt(&mut self, channel: &str, attempt: JoinAttempt) -> bool {
        if !self.is_channel(channel) || !valid_channel_token(channel) {
            return false;
        }
        let key = self.casemapping.fold(channel.as_bytes());
        if !self.join_attempts.contains_key(&key) && self.join_attempts.len() >= MAX_CHANNELS {
            return false;
        }
        self.join_attempts
            .insert(key, (channel.to_owned(), attempt));
        true
    }
    /// Classifies a standard join-rejection reply and records it against the target
    /// channel, but only after the parameter shape has been validated.
    fn apply_join_failure(&mut self, numeric: &'static str, params: &[Vec<u8>]) {
        let text = |value: &[u8]| String::from_utf8_lossy(value).into_owned();
        // RFC-shaped replies name the channel in the last non-trailing parameter;
        // a bare numeric carries it as the only parameter.
        let candidate = match params.len() {
            0 => return,
            1 => text(&params[0]),
            _ => text(&params[params.len() - 2]),
        };
        if !self.is_channel(&candidate) || !valid_channel_token(&candidate) {
            return;
        }
        self.reject_desired_join(&candidate, numeric);
    }
    /// Authoritative self membership: only a server-confirmed self JOIN may add a
    /// channel, and the outstanding attempt for it becomes meaningless.
    fn confirm_self_join(&mut self, channel: &str) {
        if self.ensure_channel(channel) && self.self_channels.len() < MAX_CHANNELS {
            self.self_channels.insert(channel.to_owned());
        }
        self.clear_join_attempt(channel);
    }

    /// Removes observed membership after an authoritative self PART/KICK.
    fn forget_self_membership(&mut self, channel: &str) {
        self.self_channels.remove(channel);
        self.clear_join_attempt(channel);
    }

    pub fn total_members(&self) -> usize {
        self.channels
            .values()
            .map(|state| state.members.len())
            .sum()
    }
    fn is_channel(&self, name: &str) -> bool {
        name.len() <= MAX_CHANNEL_NAME_BYTES && self.chantypes.is_channel(name)
    }
    fn ensure_channel(&mut self, name: &str) -> bool {
        if !self.is_channel(name) {
            return false;
        }
        if self.channels.len() >= MAX_CHANNELS && !self.channels.contains_key(name) {
            return false;
        }
        self.channels.entry(name.to_owned()).or_default();
        true
    }
    fn member_index(&self, channel: &str, nick: &str) -> Option<usize> {
        let folded = self.casemapping.fold(nick.as_bytes());
        self.channels.get(channel).and_then(|state| {
            state
                .members
                .iter()
                .position(|member| self.casemapping.fold(member.nick.as_bytes()) == folded)
        })
    }
    fn insert_member(&mut self, channel: &str, nick: &str, observation: MemberObservation) {
        if nick.is_empty() {
            return;
        }
        if let Some(index) = self.member_index(channel, nick)
            && let Some(state) = self.channels.get_mut(channel)
            && let Some(member) = state.members.get_mut(index)
        {
            // Re-observing a member merges rather than replaces. A NAMES entry establishes
            // the prefix run; a later `MODE` delta adds one symbol to it, and neither may
            // erase the other. An observation that carries no account or realname leaves
            // what is already known untouched, because "not mentioned" is not "logged out".
            if !observation.symbols.is_empty() {
                let merged = self
                    .prefix
                    .merge_symbols(&member.symbols, &observation.symbols);
                member.symbols = merged;
                member.symbols_complete |= observation.symbols_complete;
            }
            if let AccountState::Known(account) = &observation.account {
                member.account = AccountState::Known(account.clone());
            }
            if let Some(realname) = observation.realname {
                member.realname = Some(realname);
            }
            return;
        }
        let total = self.total_members();
        let known_length = self
            .channels
            .get(channel)
            .map_or(0, |state| state.members.len());
        if total >= MAX_TOTAL_MEMBERS || known_length >= MAX_MEMBERS_PER_CHANNEL {
            if let Some(state) = self.channels.get_mut(channel) {
                state.members_complete = false;
            }
            return;
        }
        if !self.ensure_channel(channel) {
            return;
        }
        if let Some(state) = self.channels.get_mut(channel) {
            if state.members.len() >= MAX_MEMBERS_PER_CHANNEL {
                state.members_complete = false;
                return;
            }
            state.members.push(MemberEntry {
                nick: nick.to_owned(),
                symbols: observation.symbols,
                symbols_complete: observation.symbols_complete,
                account: observation.account,
                realname: observation.realname,
                away: None,
            });
        }
    }
    /// Records an observed account change for every channel this member is in.
    ///
    /// `account-notify` defines the message as covering the target's *common* channels, and
    /// a member has one account rather than one per room. An unknown member is a line the
    /// bouncer simply has nowhere to put: it is applied nowhere rather than to some
    /// guessed channel.
    pub fn note_account(&mut self, nick: &str, account: AccountState) {
        self.for_each_member(nick, |member| member.account = account.clone());
    }
    /// Records an observed realname change for every channel this member is in.
    ///
    /// An over-long or framing-bearing realname is dropped, which leaves whatever was
    /// previously observed in place: retaining the last known value is a stale answer,
    /// but replacing it with a fabricated one would be a false one, and the projection
    /// omits a realname it does not have rather than inventing one.
    pub fn note_realname(&mut self, nick: &str, realname: &str) {
        let Some(realname) = self.bounded_realname(realname) else {
            return;
        };
        self.for_each_member(nick, |member| member.realname = Some(realname.clone()));
    }
    /// Records an observed away-state change for every channel this member is in.
    pub fn note_away(&mut self, nick: &str, away: AwayState) {
        self.for_each_member(nick, |member| member.away = Some(away.clone()));
    }
    fn for_each_member(&mut self, nick: &str, mut apply: impl FnMut(&mut MemberEntry)) {
        let casemapping = self.casemapping;
        let folded = casemapping.fold(nick.as_bytes());
        for state in self.channels.values_mut() {
            for member in &mut state.members {
                if casemapping.fold(member.nick.as_bytes()) == folded {
                    apply(member);
                }
            }
        }
    }
    /// Bounds one observed away message to the ceiling, returning `None` when the message
    /// cannot be retained.
    fn bounded_away(&self, raw: &str) -> Option<String> {
        if raw.is_empty() || raw.len() > MAX_AWAY_MESSAGE_BYTES {
            return None;
        }
        if raw
            .bytes()
            .any(|byte| byte == 0 || byte == b'\r' || byte == b'\n')
        {
            return None;
        }
        Some(raw.to_owned())
    }
    fn remove_member(&mut self, channel: &str, nick: &str) {
        let casemapping = self.casemapping;
        let folded = casemapping.fold(nick.as_bytes());
        if let Some(state) = self.channels.get_mut(channel)
            && let Some(index) = state
                .members
                .iter()
                .position(|member| casemapping.fold(member.nick.as_bytes()) == folded)
        {
            state.members.remove(index);
        }
    }
    fn rename_member(&mut self, old: &str, new: &str) {
        let casemapping = self.casemapping;
        let old_folded = casemapping.fold(old.as_bytes());
        for state in self.channels.values_mut() {
            for member in &mut state.members {
                if casemapping.fold(member.nick.as_bytes()) == old_folded {
                    member.nick = new.to_owned();
                }
            }
        }
        if self.same_nick(old, &self.nick) {
            self.nick = new.to_owned();
        }
    }
    fn remove_everywhere(&mut self, nick: &str) {
        let folded = self.casemapping.fold(nick.as_bytes());
        for state in self.channels.values_mut() {
            state
                .members
                .retain(|member| self.casemapping.fold(member.nick.as_bytes()) != folded);
        }
    }

    /// Applies one decoded server line to observed state.
    pub fn apply_line(&mut self, message: &i2pr_irc_wire::Message) -> LineOutcome {
        let command = String::from_utf8_lossy(&message.command).to_ascii_uppercase();
        let text = |value: &[u8]| String::from_utf8_lossy(value).into_owned();
        let source = source_nick(message.prefix.as_deref());
        let params = &message.params;
        match command.as_str() {
            "005" => {
                for token in params
                    .iter()
                    .skip(1)
                    .filter_map(|value| std::str::from_utf8(value).ok())
                    .flat_map(str::split_whitespace)
                {
                    self.apply_isupport_token(token);
                }
            }
            "JOIN" if !params.is_empty() => {
                let channel = text(&params[0]);
                if let Some(nick) = source.as_deref()
                    && self.is_channel(&channel)
                {
                    let (symbols, bare) = self.prefix.split_prefix_run(nick);
                    if self.same_nick(bare, &self.nick) {
                        self.confirm_self_join(&channel);
                    }
                    // `extended-join` appends the account and the realname. A JOIN without
                    // them says nothing about either, and "this JOIN did not mention an
                    // account" is not the same observation as "this member is logged out":
                    // the account stays `Unknown` so a projection cannot render a `*` the
                    // server never sent.
                    let mut observation = MemberObservation {
                        symbols,
                        symbols_complete: self.multi_prefix,
                        ..MemberObservation::membership_only()
                    };
                    if params.len() >= 3 {
                        observation.account = AccountState::observed(&text(&params[1]));
                        observation.realname =
                            self.bounded_realname(&text(params.last().expect("checked length")));
                    }
                    self.insert_member(&channel, bare, observation);
                }
            }
            // The server-to-client form is only ever emitted under `away-notify`, so
            // receiving one means the server agrees the bouncer negotiated it. The
            // message covers the sender's own state, which is why the observation is
            // applied from the prefix rather than from a parameter.
            "AWAY" => {
                if let Some(nick) = source.as_deref() {
                    let away = match params.last() {
                        Some(raw) => {
                            let raw = text(raw);
                            self.bounded_away(&raw)
                                .map(AwayState::Away)
                                .unwrap_or(AwayState::Here)
                        }
                        None => AwayState::Here,
                    };
                    self.note_away(nick, away);
                }
            }
            "ACCOUNT" if !params.is_empty() => {
                if let Some(nick) = source.as_deref() {
                    let account = AccountState::observed(&text(&params[0]));
                    self.note_account(nick, account);
                }
            }
            "SETNAME" if !params.is_empty() => {
                if let Some(nick) = source.as_deref() {
                    let realname = text(params.last().expect("checked length"));
                    self.note_realname(nick, &realname);
                }
            }
            "PART" if !params.is_empty() => {
                let channel = text(&params[0]);
                if let Some(nick) = source.as_deref() {
                    self.remove_member(&channel, nick);
                    if self.same_nick(nick, &self.nick) {
                        self.forget_self_membership(&channel);
                    }
                }
            }
            "KICK" if params.len() > 1 => {
                let channel = text(&params[0]);
                let target = text(&params[1]);
                self.remove_member(&channel, &target);
                if self.same_nick(&target, &self.nick) {
                    self.forget_self_membership(&channel);
                }
            }
            "QUIT" => {
                if let Some(nick) = source.as_deref() {
                    self.remove_everywhere(nick);
                }
            }
            "NICK" if !params.is_empty() => {
                if crate::valid_client_nick(&params[0])
                    && let Some(nick) = source.as_deref()
                {
                    let replacement = text(&params[0]);
                    self.rename_member(nick, &replacement);
                }
            }
            "353" if params.len() >= 3 => {
                // The channel-visibility field is `=`, `*`, or `@`; it is a
                // server-reply grammar value and never a member PREFIX symbol.
                let channel = text(&params[params.len() - 2]);
                let names = text(params.last().expect("checked length"));
                if matches!(text(&params[params.len() - 3]).as_str(), "=" | "*" | "@") {
                    self.apply_names(&channel, &names);
                }
            }
            "366" if params.len() >= 2 => {
                let channel = text(&params[1]);
                if let Some(state) = self.channels.get_mut(&channel) {
                    state.names_seen = true;
                }
            }
            "332" if params.len() >= 3 => {
                let topic = text(params.last().expect("checked length"));
                self.apply_topic(&text(&params[1]), &topic);
            }
            "TOPIC" if params.len() >= 2 => {
                let topic = text(params.last().expect("checked length"));
                self.apply_topic(&text(&params[0]), &topic);
            }
            "MODE" if params.len() >= 2 => {
                let channel = text(&params[0]);
                let spec = text(&params[1]);
                let arguments: Vec<String> = params[2..].iter().map(|value| text(value)).collect();
                let delta = ModeDelta::default();
                if self.apply_modes(&channel, &spec, &arguments, delta) {
                    self.mark_modes_incomplete(&channel);
                }
            }
            "324" if params.len() >= 3 => {
                let channel = text(&params[1]);
                let spec = text(&params[2]);
                let arguments: Vec<String> = params[3..].iter().map(|value| text(value)).collect();
                let delta = ModeDelta {
                    authoritative: true,
                    ..ModeDelta::default()
                };
                if self.apply_modes(&channel, &spec, &arguments, delta) {
                    self.mark_modes_incomplete(&channel);
                }
            }
            "PING" => {
                if params.is_empty() {
                    return LineOutcome::Malformed;
                }
                return LineOutcome::ReplyPong(text(params.last().expect("non-empty")));
            }
            other => {
                // Standard channel-failure numerics are the only reply that can
                // disambiguate a refused desired attempt from a confirmed one.
                // Every other command, including network-specific numerics, stays an
                // ordinary server event.
                if let Some(failure) = JOIN_FAILURE_NUMERICS
                    .iter()
                    .find(|value| **value == other)
                    .copied()
                {
                    self.apply_join_failure(failure, params);
                }
            }
        }
        LineOutcome::Quiet
    }

    fn apply_isupport_token(&mut self, token: &str) {
        if let Some(value) = token.strip_prefix("CASEMAPPING=") {
            self.casemapping = match value {
                "ascii" => Casemapping::Ascii,
                // Both spellings are accepted: `rfc1459-strict` is the Modern IRC
                // Client Protocol value and `strict-rfc1459` is the older
                // RPL_ISUPPORT draft spelling. Folding `~`/`^` when the server
                // said they are distinct would merge two identities, so an
                // unrecognized spelling keeps the rfc1459 default rather than
                // silently claiming strictness.
                "rfc1459-strict" | "strict-rfc1459" => Casemapping::StrictRfc1459,
                _ => Casemapping::Rfc1459,
            };
        } else if let Some(value) = token.strip_prefix("CHANTYPES=")
            // A malformed value leaves the retained mapping authoritative.
            && let Some(chantypes) = ChanTypes::parse(value)
        {
            self.chantypes = chantypes;
        } else if let Some(value) = token.strip_prefix("PREFIX=")
            && let Some(prefix) = PrefixMap::parse(value)
        {
            self.prefix = prefix;
        } else if let Some(value) = token.strip_prefix("CHANMODES=")
            && let Some(chanmodes) = ChanModes::parse(value)
        {
            self.chanmodes = chanmodes;
        } else if let Some(value) = token.strip_prefix("NAMELEN=") {
            // A `NAMELEN` that does not parse leaves the hard ceiling authoritative rather
            // than widening or narrowing what the bouncer is willing to retain.
            self.namelen = value.parse::<usize>().ok().filter(|len| *len > 0);
        }
        // Tokens are retained for the downstream `005` projection, bounded.
        if token.len() <= MAX_ISUPPORT_TOKEN_BYTES
            && token.bytes().all(|b| b.is_ascii_graphic())
            && !matches!(token, "are" | "supported" | "by" | "this" | "server")
            && self.isupport.len() < MAX_ISUPPORT_TOKENS
        {
            self.isupport.insert(token.to_owned());
        }
    }

    fn apply_topic(&mut self, channel: &str, topic: &str) {
        if !self.ensure_channel(channel) {
            return;
        }
        // An unrepresentable topic is omitted rather than truncated.
        if topic.len() > MAX_TOPIC_BYTES {
            return;
        }
        if let Some(state) = self.channels.get_mut(channel) {
            state.topic = Some(topic.to_owned());
        }
    }

    fn mark_modes_incomplete(&mut self, channel: &str) {
        if let Some(state) = self.channels.get_mut(channel) {
            state.modes.mark_incomplete();
        }
    }

    fn apply_names(&mut self, channel: &str, names: &str) {
        if !self.is_channel(channel) {
            return;
        }
        let entries: Vec<MemberObservation> = names
            .split_whitespace()
            .map(|entry| {
                let (symbols, _) = self.prefix.split_prefix_run(entry);
                MemberObservation {
                    symbols,
                    symbols_complete: self.multi_prefix,
                    ..MemberObservation::membership_only()
                }
            })
            .collect();
        let names: Vec<String> = names
            .split_whitespace()
            .map(|entry| self.prefix.split_prefix_run(entry).1.to_owned())
            .collect();
        for (observation, nick) in entries.into_iter().zip(names) {
            self.insert_member(channel, &nick, observation);
        }
        if let Some(state) = self.channels.get_mut(channel) {
            state.names_seen = true;
        }
    }

    fn apply_modes(
        &mut self,
        channel: &str,
        spec: &str,
        arguments: &[String],
        mut delta: ModeDelta,
    ) -> bool {
        if !self.is_channel(channel) {
            // User modes are not channel state and are not projected downstream.
            return false;
        }
        let spec = spec.strip_prefix('+').unwrap_or(spec);
        if spec.is_empty() {
            return false;
        }
        if !self.ensure_channel(channel) {
            return true;
        }
        let mut next_argument = 0usize;
        let mut adding = true;
        for letter in spec.chars() {
            match letter {
                '+' => {
                    adding = true;
                    continue;
                }
                '-' => {
                    adding = false;
                    continue;
                }
                letter if letter.is_ascii_alphabetic() => {}
                _ => return true,
            }
            if self.prefix.is_membership_mode(letter) {
                let Some(nick) = arguments.get(next_argument) else {
                    return true;
                };
                next_argument += 1;
                if !self.apply_membership_mode(channel, letter, adding, nick) {
                    return true;
                }
                continue;
            }
            let Some(arity) = self.chanmodes.arity(letter) else {
                // Parameter consumption is unknown, so no truthful projection exists.
                return true;
            };
            let takes_argument = arity != ModeArity::Never;
            if adding {
                if takes_argument {
                    let Some(argument) = arguments.get(next_argument) else {
                        return true;
                    };
                    next_argument += 1;
                    delta.sets.push((letter, vec![argument.clone()]));
                } else {
                    delta.sets.push((letter, Vec::new()));
                }
            } else {
                // Clearing a mode needs no argument to stay reconstructable, so a
                // missing clear argument stays truthful instead of failing the delta.
                if takes_argument && next_argument < arguments.len() {
                    next_argument += 1;
                }
                delta.clears.push(letter);
            }
        }
        self.commit_modes(channel, delta);
        false
    }

    fn apply_membership_mode(
        &mut self,
        channel: &str,
        mode: char,
        adding: bool,
        nick: &str,
    ) -> bool {
        let symbol = self.prefix.symbol_for(mode);
        match self.member_index(channel, nick) {
            Some(index) => {
                // The merge is computed from an owned copy rather than in place: it needs
                // the prefix map while the member itself is mutably borrowed, and taking
                // `&self` for the map would overlap that borrow.
                let Some(current) = self
                    .channels
                    .get(channel)
                    .and_then(|state| state.members.get(index))
                    .map(|member| member.symbols.clone())
                else {
                    return false;
                };
                let next = match (adding, symbol) {
                    // A delta adds one symbol to the run and leaves completeness alone.
                    // Completing an incomplete set would be a claim this observation does
                    // not support.
                    (true, Some(symbol)) => self.prefix.merge_symbols(&current, &[symbol]),
                    (false, Some(symbol)) => {
                        let mut next = current;
                        next.retain(|held| *held != symbol);
                        next
                    }
                    // A membership mode with no advertised symbol cannot change anything,
                    // so the run is left exactly as observed.
                    (_, None) => current,
                };
                if let Some(state) = self.channels.get_mut(channel)
                    && let Some(member) = state.members.get_mut(index)
                {
                    member.symbols = next;
                }
                true
            }
            None => {
                // The member is unknown, so the member list is no longer truthful.
                if let Some(state) = self.channels.get_mut(channel) {
                    state.members_complete = false;
                }
                false
            }
        }
    }

    fn commit_modes(&mut self, channel: &str, delta: ModeDelta) {
        let Some(state) = self.channels.get_mut(channel) else {
            return;
        };
        if delta.authoritative {
            let mut replacement = ModeSnapshot::empty();
            for (mode, arguments) in delta.sets {
                replacement.set(mode, arguments);
            }
            state.modes.replace(&replacement);
            return;
        }
        let mut snapshot = state.modes.clone();
        for (mode, arguments) in delta.sets {
            snapshot.set(mode, arguments);
        }
        for mode in delta.clears {
            snapshot.clear(mode);
        }
        state.modes.replace(&snapshot);
    }
}

/// One parsed channel mode delta before it is committed to a snapshot.
#[derive(Clone, Debug, Default)]
struct ModeDelta {
    sets: Vec<(char, Vec<String>)>,
    clears: Vec<char>,
    authoritative: bool,
}

/// A channel name that may safely key attempt bookkeeping: bounded and free of the
/// characters that separate parameters in a server reply.
pub fn valid_channel_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_CHANNEL_NAME_BYTES
        && !value
            .bytes()
            .any(|b| b.is_ascii_whitespace() || matches!(b, b',' | b':' | 0 | b'\r' | b'\n'))
}

/// The nick portion of a message prefix.
pub fn source_nick(prefix: Option<&[u8]>) -> Option<String> {
    prefix
        .map(|prefix| {
            String::from_utf8_lossy(
                prefix
                    .split(|b| *b == b'!' || *b == b'@')
                    .next()
                    .unwrap_or(prefix),
            )
            .into_owned()
        })
        .filter(|nick| !nick.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use i2pr_irc_wire::Message;

    fn line(raw: &[u8]) -> Message {
        Message::parse(raw).expect("fixture parses")
    }
    fn rendered(state: &NetworkState, channel: &str) -> Vec<String> {
        let mut members: Vec<String> = state.channels[channel]
            .members
            .iter()
            .map(|member| member.display(false))
            .collect();
        members.sort();
        members
    }

    #[test]
    fn default_prefix_is_bounded_and_validated() {
        let map = PrefixMap::default();
        assert_eq!(map.symbol_for('o'), Some('@'));
        assert_eq!(map.symbol_for('v'), Some('+'));
        assert!(map.is_membership_mode('o'));
        assert!(!map.is_membership_mode('b'));
        assert_eq!(map.split_prefix_run("@Alice"), (vec!['@'], "Alice"));
        assert_eq!(map.split_prefix_run("~Bob"), (Vec::new(), "~Bob"));
        assert_eq!(map.split_prefix_run("Alice"), (Vec::new(), "Alice"));
        // A run is read whole and put back into rank order, so a server that sent it
        // backwards cannot change which symbol a legacy client is shown as its highest.
        assert_eq!(map.split_prefix_run("+@Alice"), (vec!['@', '+'], "Alice"));
        assert_eq!(map.split_prefix_run("@+Alice"), (vec!['@', '+'], "Alice"));
        // A repeated symbol is one held mode, not two.
        assert_eq!(map.split_prefix_run("@@Alice"), (vec!['@'], "Alice"));
        assert_eq!(map.highest(&['+', '@']), Some('@'));
        assert_eq!(map.highest(&['+']), Some('+'));
        assert_eq!(map.highest(&['!']), None);
        assert_eq!(map.merge_symbols(&['+'], &['@']), vec!['@', '+']);
        assert_eq!(map.merge_symbols(&['@'], &['!']), vec!['@']);
        assert!(PrefixMap::parse("(ov@+").is_none());
        assert!(PrefixMap::parse("(ov)@").is_none());
        assert!(PrefixMap::parse("()@").is_none());
        assert!(PrefixMap::parse("(abcdefghi)@%+!").is_none());
        assert!(PrefixMap::parse("(oa)@%").is_some());
    }

    #[test]
    fn chantypes_are_bounded_and_classify_targets() {
        assert!(ChanTypes::default().is_channel("#x"));
        assert!(ChanTypes::default().is_channel("&x"));
        assert!(!ChanTypes::default().is_channel("+x"));
        assert!(!ChanTypes::default().is_channel("bob"));
        assert!(ChanTypes::parse("").is_none());
        assert!(ChanTypes::parse("##").is_none());
        assert!(ChanTypes::parse("#&+!").is_some());
        let widened = ChanTypes::parse("#&+!").expect("bounded set");
        assert!(widened.is_channel("+x"));
        assert!(!widened.is_channel("%x"));
    }

    #[test]
    fn chanmode_arity_follows_advertised_classes() {
        let modes = ChanModes::default();
        assert_eq!(modes.arity('k'), Some(ModeArity::Always));
        assert_eq!(modes.arity('b'), Some(ModeArity::Always));
        assert_eq!(modes.arity('l'), Some(ModeArity::WhenSet));
        assert_eq!(modes.arity('n'), Some(ModeArity::Never));
        assert_eq!(modes.arity('z'), None);
        let custom = ChanModes::parse("b,k,l,imnpstR").expect("custom modes");
        assert_eq!(custom.arity('R'), Some(ModeArity::Never));
        assert!(ChanModes::parse("b,k,l").is_none());
        assert!(ChanModes::parse("b,k,l,nn").is_none());
        assert!(ChanModes::parse("b,k,l,b").is_none());
    }

    #[test]
    fn parameterised_modes_round_trip_truthfully() {
        let mut state = NetworkState::new("bot", &[]);
        state.apply_line(&line(b":srv 005 bot CHANMODES=beI,k,l,imnpst\r\n"));
        state.apply_line(&line(b":srv 324 bot #room +tkl key 42\r\n"));
        let snapshot = &state.channels["#room"].modes;
        assert!(snapshot.is_complete());
        assert_eq!(snapshot.render().as_deref(), Some("klt key 42"));
    }

    #[test]
    fn incremental_parameterised_modes_merge_without_loss() {
        let mut state = NetworkState::new("bot", &[]);
        state.apply_line(&line(b":srv 005 bot CHANMODES=beI,k,l,imnpst\r\n"));
        state.apply_line(&line(b":srv MODE #room +nt\r\n"));
        state.apply_line(&line(b":srv MODE #room +kl key 42\r\n"));
        state.apply_line(&line(b":srv MODE #room -k\r\n"));
        state.apply_line(&line(b":srv MODE #room +l 42\r\n"));
        let snapshot = &state.channels["#room"].modes;
        assert!(snapshot.is_complete());
        assert_eq!(snapshot.render().as_deref(), Some("lnt 42"));
    }

    #[test]
    fn unknown_mode_letter_makes_the_snapshot_incomplete() {
        let mut state = NetworkState::new("bot", &[]);
        state.apply_line(&line(b":srv 005 bot CHANMODES=beI,k,l,imnpst\r\n"));
        state.apply_line(&line(b":srv MODE #room +nq\r\n"));
        let snapshot = &state.channels["#room"].modes;
        assert!(!snapshot.is_complete());
        assert!(snapshot.render().is_none());
    }

    #[test]
    fn missing_parameter_makes_the_snapshot_incomplete() {
        let mut state = NetworkState::new("bot", &[]);
        state.apply_line(&line(b":srv 005 bot CHANMODES=beI,k,l,imnpst\r\n"));
        state.apply_line(&line(b":srv MODE #room +kl key\r\n"));
        assert!(!state.channels["#room"].modes.is_complete());
    }

    #[test]
    fn authoritative_snapshot_restores_completeness() {
        let mut state = NetworkState::new("bot", &[]);
        state.apply_line(&line(b":srv 005 bot CHANMODES=beI,k,l,imnpst\r\n"));
        state.apply_line(&line(b":srv MODE #room +nq\r\n"));
        assert!(!state.channels["#room"].modes.is_complete());
        state.apply_line(&line(b":srv 324 bot #room +nt\r\n"));
        let snapshot = &state.channels["#room"].modes;
        assert!(snapshot.is_complete());
        assert_eq!(snapshot.render().as_deref(), Some("nt"));
    }

    #[test]
    fn advertised_prefix_replaces_the_hard_coded_assumption() {
        let mut state = NetworkState::new("bot", &[]);
        state.apply_line(&line(b":srv 005 bot PREFIX=(qaohv)~&@%+ CHANTYPES=#&\r\n"));
        state.apply_line(&line(
            b":srv 353 bot = #room :*Admin ~Quiet @Op %Half +Voice Bot\r\n",
        ));
        assert_eq!(
            rendered(&state, "#room"),
            ["%Half", "*Admin", "+Voice", "@Op", "Bot", "~Quiet"]
        );
    }

    #[test]
    fn symbols_outside_the_advertised_mapping_are_nicks() {
        let mut state = NetworkState::new("bot", &[]);
        state.apply_line(&line(b":srv 005 bot PREFIX=(ov)@+\r\n"));
        state.apply_line(&line(b":srv 353 bot = #room :@Op ~Someone bot\r\n"));
        assert_eq!(rendered(&state, "#room"), ["@Op", "bot", "~Someone"]);
    }

    #[test]
    fn membership_mode_changes_update_or_invalidate_membership() {
        let mut state = NetworkState::new("bot", &[]);
        state.apply_line(&line(b":srv 353 bot = #room :@Op bot\r\n"));
        state.apply_line(&line(b":srv MODE #room +o bot\r\n"));
        let member = state.channels["#room"]
            .members
            .iter()
            .find(|member| member.nick == "bot");
        assert_eq!(
            member.map(|member| member.symbols.as_slice()),
            Some(['@'].as_slice())
        );
        assert!(state.channels["#room"].members_complete);
        state.apply_line(&line(b":srv MODE #room -o bot\r\n"));
        let member = state.channels["#room"]
            .members
            .iter()
            .find(|member| member.nick == "bot");
        assert_eq!(
            member.map(|member| member.symbols.as_slice()),
            Some([].as_slice())
        );
        state.apply_line(&line(b":srv MODE #room +v Stranger\r\n"));
        assert!(!state.channels["#room"].members_complete);
        assert!(!state.channels["#room"].modes.is_complete());
    }

    #[test]
    fn both_strict_casemapping_spellings_are_honored() {
        // The Modern IRC Client Protocol spells the value `rfc1459-strict`; the
        // older RPL_ISUPPORT draft spells it `strict-rfc1459`. Recognizing only
        // one of them folds `~`/`^` on a network that said they are distinct,
        // which silently merges two identities.
        for token in ["CASEMAPPING=rfc1459-strict", "CASEMAPPING=strict-rfc1459"] {
            let mut state = NetworkState::new("bot", &[]);
            state.apply_line(&line(format!(":srv 005 bot {token}\r\n").as_bytes()));
            assert!(!state.same_nick("bot~", "bot^"), "{token}");
            assert!(state.same_nick("bot[", "bot{"), "{token}");
            assert!(state.same_nick("Bot", "bot"), "{token}");
        }
        // An unrecognized value keeps the documented rfc1459 default rather than
        // claiming strictness the server never promised.
        let mut state = NetworkState::new("bot", &[]);
        state.apply_line(&line(b":srv 005 bot CASEMAPPING=rfc7613\r\n"));
        assert!(state.same_nick("bot~", "bot^"));
        assert!(state.same_nick("bot[", "bot{"));
    }

    #[test]
    fn nick_part_quit_topic_and_desired_state_are_tracked() {
        let mut state = NetworkState::new("bot", &[DesiredChannelPolicy::attached("#room")]);
        assert!(state.begin_desired_join("#room"));
        state.apply_line(&line(b":bot!u@h JOIN #room\r\n"));
        state.apply_line(&line(b":alice!u@h JOIN #room\r\n"));
        state.apply_line(&line(b":alice!u@h NICK alice2\r\n"));
        assert!(rendered(&state, "#room").contains(&"alice2".to_owned()));
        state.apply_line(&line(b":alice2!u@h TOPIC #room :new subject\r\n"));
        assert_eq!(
            state.channels["#room"].topic.as_deref(),
            Some("new subject")
        );
        state.apply_line(&line(b":alice2!u@h QUIT :bye\r\n"));
        assert_eq!(rendered(&state, "#room"), ["bot"]);
        state.apply_line(&line(b":bot!u@h PART #room\r\n"));
        assert!(state.self_channels.is_empty());
    }

    #[test]
    fn a_written_desired_join_is_not_observed_membership() {
        let mut state = NetworkState::new("bot", &[DesiredChannelPolicy::attached("#room")]);
        assert!(state.begin_desired_join("#room"));
        // Writing the command is not evidence: nothing is joined yet.
        assert!(state.self_channels.is_empty());
        assert!(state.joined_channels().is_empty());
        assert_eq!(state.pending_joins(), ["#room"]);
        // Only the server's own JOIN confirms membership.
        state.apply_line(&line(b":bot!u@h JOIN #room\r\n"));
        assert_eq!(state.joined_channels(), ["#room"]);
        assert!(state.pending_joins().is_empty());
        assert!(state.rejected_joins().is_empty());
        assert!(state.join_attempts().is_empty());
        // Desired intent is unchanged by a confirmation.
        assert_eq!(state.desired_channels, ["#room"]);
    }

    #[test]
    fn self_kick_removes_observed_membership() {
        let mut state = NetworkState::new("bot", &[DesiredChannelPolicy::attached("#room")]);
        state.begin_desired_join("#room");
        state.apply_line(&line(b":bot!u@h JOIN #room\r\n"));
        assert_eq!(state.joined_channels(), ["#room"]);
        state.apply_line(&line(b":op!u@h KICK #room bot :bye\r\n"));
        assert!(state.joined_channels().is_empty());
    }

    #[test]
    fn every_standard_join_failure_leaves_membership_absent() {
        for numeric in JOIN_FAILURE_NUMERICS {
            let mut state = NetworkState::new("bot", &[DesiredChannelPolicy::attached("#room")]);
            assert!(state.begin_desired_join("#room"));
            let reply = format!(":srv {numeric} bot #room :No such channel\r\n");
            state.apply_line(&line(reply.as_bytes()));
            assert!(
                state.joined_channels().is_empty(),
                "{numeric} must not create membership"
            );
            assert!(state.pending_joins().is_empty(), "{numeric} clears pending");
            assert_eq!(
                state.rejected_joins(),
                [("#room".to_owned(), numeric)],
                "{numeric} is recorded as a bounded diagnostic"
            );
            // A rejection is an observation about this generation, not a
            // configuration change.
            assert_eq!(state.desired_channels, ["#room"]);
            assert!(
                state
                    .channels
                    .get("#room")
                    .is_none_or(|room| room.members.is_empty())
            );
        }
    }

    #[test]
    fn join_failure_accepts_the_bare_and_nick_prefixed_reply_shapes() {
        let mut state = NetworkState::new("bot", &[]);
        state.apply_line(&line(b":srv 471 #full :Cannot join channel (+l)\r\n"));
        assert_eq!(state.rejected_joins(), [("#full".to_owned(), "471")]);
        state.apply_line(&line(b":srv 473 bot #locked :Cannot join channel (+i)\r\n"));
        assert_eq!(
            state.rejected_joins(),
            [("#full".to_owned(), "471"), ("#locked".to_owned(), "473")]
        );
    }

    #[test]
    fn malformed_and_unrelated_rejections_are_not_recorded() {
        let mut state = NetworkState::new("bot", &[]);
        // A non-channel target, a target that is not a channel name, and a
        // network-specific numeric must all stay ordinary server events.
        state.apply_line(&line(b":srv 403 bot :No such channel\r\n"));
        state.apply_line(&line(b":srv 403 bot #room,other :No such channel\r\n"));
        state.apply_line(&line(
            b":srv 437 bot #room :Nick/channel is temporarily unavailable\r\n",
        ));
        assert!(state.rejected_joins().is_empty());
        assert!(state.joined_channels().is_empty());
    }

    #[test]
    fn a_confirmed_join_clears_a_previous_rejection_and_casemaps_the_key() {
        let mut state = NetworkState::new("bot", &[DesiredChannelPolicy::attached("#Room")]);
        // rfc1459 casemapping folds `[]\^` to lowercase, so a differently cased
        // confirmation addresses the same attempt.
        assert!(state.begin_desired_join("#Room"));
        state.apply_line(&line(b":srv 471 bot #room :Cannot join channel (+l)\r\n"));
        assert_eq!(state.rejected_joins(), [("#room".to_owned(), "471")]);
        state.apply_line(&line(b":bot!u@h JOIN #room\r\n"));
        assert_eq!(state.joined_channels(), ["#room"]);
        assert!(state.rejected_joins().is_empty());
        assert!(state.join_attempts().is_empty());
    }

    #[test]
    fn join_attempts_are_bounded_and_reject_unrepresentable_targets() {
        let mut state = NetworkState::new("bot", &[]);
        assert!(!state.begin_desired_join("notachannel"));
        assert!(!state.begin_desired_join(""));
        assert!(state.join_attempts().is_empty());
        for index in 0..MAX_CHANNELS {
            assert!(state.begin_desired_join(&format!("#room{index}")));
        }
        assert!(!state.begin_desired_join("#overflow"));
        assert_eq!(state.pending_joins().len(), MAX_CHANNELS);
    }

    #[test]
    fn names_visibility_accepts_equals_star_and_at() {
        for visibility in ["=", "*", "@"] {
            let mut state = NetworkState::new("bot", &[]);
            state.apply_line(&line(
                format!(":srv 353 bot {visibility} #secret :bot @Alice\r\n").as_bytes(),
            ));
            assert_eq!(
                rendered(&state, "#secret"),
                ["@Alice", "bot"],
                "{visibility}"
            );
        }
    }

    #[test]
    fn observed_member_metadata_is_bounded_and_never_fabricated() {
        let mut state = NetworkState::new("bot", &[]);
        state.apply_line(&line(b":srv 005 bot PREFIX=(ov)@+ NAMELEN=32\r\n"));
        state.apply_line(&line(b":srv 353 bot = #room :@+Alice bot\r\n"));
        state.apply_line(&line(b":srv 366 bot #room :End of /NAMES list.\r\n"));

        // The run is retained as observed -- the entry really did carry both symbols -- but no
        // completeness is claimed, so it is never presented as a complete set.
        let alice = |state: &NetworkState| {
            state.channels["#room"]
                .members
                .iter()
                .find(|member| member.nick == "Alice")
                .expect("Alice is a member")
                .clone()
        };
        let member = alice(&state);
        assert_eq!(member.symbols, ['@', '+']);
        assert!(
            !member.symbols_complete,
            "an unnegotiated run is never complete"
        );
        assert_eq!(
            member.display(true),
            "@Alice",
            "an incomplete run must fall back to the one symbol every client can read"
        );
        assert_eq!(member.display(false), "@Alice");
        assert_eq!(member.account, AccountState::Unknown);

        // An extended JOIN carries the account and realname; `*` is an *observed* logout,
        // which is a different fact from never having looked.
        state.apply_line(&line(b":Bob!u@h JOIN #room bobacct :Bob Example\r\n"));
        state.apply_line(&line(b":Carol!u@h JOIN #room * :Carol Example\r\n"));
        let find = |state: &NetworkState, nick: &str| {
            state.channels["#room"]
                .members
                .iter()
                .find(|member| member.nick == nick)
                .expect("member")
                .clone()
        };
        assert_eq!(
            find(&state, "Bob").account,
            AccountState::Known(Some("bobacct".into()))
        );
        assert_eq!(find(&state, "Bob").realname.as_deref(), Some("Bob Example"));
        assert_eq!(find(&state, "Carol").account, AccountState::Known(None));
        // A plain JOIN said nothing, so the account stays unknown rather than becoming an
        // asserted logout.
        assert_eq!(find(&state, "Alice").account, AccountState::Unknown);

        // `account-notify` and `setname` apply to every channel the member is in.
        state.apply_line(&line(b":Bob!u@h ACCOUNT bob2 :Logged in\r\n"));
        state.apply_line(&line(b":Bob!u@h SETNAME :Bob Renamed\r\n"));
        assert_eq!(
            find(&state, "Bob").account,
            AccountState::Known(Some("bob2".into()))
        );
        assert_eq!(find(&state, "Bob").realname.as_deref(), Some("Bob Renamed"));
        // A `MODE` delta adds to the run rather than replacing it, and removes exactly one symbol.
        state.apply_line(&line(b":Op!u@h MODE #room +v Alice\r\n"));
        assert_eq!(
            find(&state, "Alice").symbols,
            ['@', '+'],
            "a delta must widen the run, not replace it with one symbol"
        );
        state.apply_line(&line(b":Op!u@h MODE #room -v Alice\r\n"));
        assert_eq!(find(&state, "Alice").symbols, ['@']);
        assert_eq!(find(&state, "Bob").symbols, Vec::<char>::new());

        // An away change is observed in both directions, and the bare form means back.
        state.apply_line(&line(b":Bob!u@h AWAY :lunch\r\n"));
        assert_eq!(
            find(&state, "Bob").away,
            Some(AwayState::Away("lunch".to_owned()))
        );
        state.apply_line(&line(b":Bob!u@h AWAY\r\n"));
        assert_eq!(find(&state, "Bob").away, Some(AwayState::Here));

        // Beyond `NAMELEN`, and beyond the hard ceiling, nothing is retained. Dropping it
        // leaves the last known value in place, which is stale but not false.
        state.apply_line(&line(
            b":Bob!u@h SETNAME :0123456789012345678901234567890123456789\r\n",
        ));
        assert_eq!(
            find(&state, "Bob").realname.as_deref(),
            Some("Bob Renamed"),
            "an over-long realname must not be retained or truncate the previous one"
        );

        // A NAMES entry establishes completeness only when `multi-prefix` was negotiated.
        let mut negotiated = NetworkState::new("bot", &[]);
        negotiated.set_upstream_multi_prefix(true);
        negotiated.apply_line(&line(b":srv 353 bot = #room :@+Alice bot\r\n"));
        let alice = negotiated.channels["#room"]
            .members
            .iter()
            .find(|member| member.nick == "Alice")
            .expect("Alice is a member");
        assert!(alice.symbols_complete);
        assert_eq!(alice.display(true), "@+Alice");
        assert_eq!(alice.display(false), "@Alice");
    }

    #[test]
    fn an_unusable_account_or_realname_is_omitted_rather_than_retained() {
        assert_eq!(AccountState::observed("*"), AccountState::Known(None));
        assert_eq!(
            AccountState::observed("real"),
            AccountState::Known(Some("real".into()))
        );
        for unusable in [
            "",
            "   ",
            "with space",
            "with\ttab",
            "with\nnewline",
            "with\0nul",
            &"a".repeat(MAX_ACCOUNT_BYTES + 1),
        ] {
            assert_eq!(
                AccountState::observed(unusable),
                AccountState::Unknown,
                "{unusable:?} must not become an account a client could read"
            );
        }
    }

    #[test]
    fn names_visibility_is_never_a_membership_prefix() {
        let mut state = NetworkState::new("bot", &[]);
        // `@` is a visibility field, and a `~` prefix is not in the advertised
        // mapping, so neither may become a member symbol.
        state.apply_line(&line(b":srv 005 bot PREFIX=(ov)@+\r\n"));
        state.apply_line(&line(b":srv 353 bot @ #secret :@Alice ~Someone bot\r\n"));
        let symbols: Vec<Vec<char>> = state.channels["#secret"]
            .members
            .iter()
            .map(|member| member.symbols.clone())
            .collect();
        assert_eq!(symbols, [vec!['@'], Vec::new(), Vec::new()]);
        assert_eq!(rendered(&state, "#secret"), ["@Alice", "bot", "~Someone"]);
    }

    #[test]
    fn unknown_names_visibility_is_ignored_without_corrupting_membership() {
        let mut state = NetworkState::new("bot", &[]);
        state.apply_line(&line(b":srv 353 bot = #room :bot @Alice\r\n"));
        state.apply_line(&line(b":srv 353 bot ! #room :Intruder\r\n"));
        assert_eq!(rendered(&state, "#room"), ["@Alice", "bot"]);
        assert!(state.channels["#room"].members_complete);
    }

    #[test]
    fn member_ceilings_invalidate_membership_truthfully() {
        let mut state = NetworkState::new("bot", &[]);
        let names: Vec<String> = (0..MAX_MEMBERS_PER_CHANNEL + 8)
            .map(|index| format!("user{index}"))
            .collect();
        // Real servers split a large member list across several lines.
        for chunk in names.chunks(20) {
            state.apply_line(&line(
                format!(":srv 353 bot = #room :{}\r\n", chunk.join(" ")).as_bytes(),
            ));
        }
        let room = &state.channels["#room"];
        assert_eq!(room.members.len(), MAX_MEMBERS_PER_CHANNEL);
        assert!(!room.members_complete);
    }

    #[test]
    fn oversized_topic_is_omitted_not_truncated() {
        let mut state = NetworkState::new("bot", &[]);
        state.apply_line(&line(
            format!(
                ":srv 332 bot #room :{}\r\n",
                "x".repeat(MAX_TOPIC_BYTES + 1)
            )
            .as_bytes(),
        ));
        assert!(state.channels["#room"].topic.is_none());
    }

    #[test]
    fn ping_reaction_and_malformed_ping_are_explicit() {
        let mut state = NetworkState::new("bot", &[]);
        assert_eq!(state.apply_line(&line(b"PING\r\n")), LineOutcome::Malformed);
        assert_eq!(
            state.apply_line(&line(b":srv PING :alive\r\n")),
            LineOutcome::ReplyPong("alive".into())
        );
    }

    #[test]
    fn user_mode_target_is_not_channel_state() {
        let mut state = NetworkState::new("bot", &[]);
        state.apply_line(&line(b":srv MODE bot +i\r\n"));
        assert!(state.channels.is_empty());
    }

    #[test]
    fn advertised_chantypes_change_live_classification() {
        let mut state = NetworkState::new("bot", &[]);
        state.apply_line(&line(b":srv 005 bot CHANTYPES=#&+!\r\n"));
        state.apply_line(&line(b":srv 332 bot +lobby :welcome\r\n"));
        assert_eq!(state.channels["+lobby"].topic.as_deref(), Some("welcome"));
        state.apply_line(&line(b":srv MODE +lobby +nt\r\n"));
        assert!(state.channels["+lobby"].modes.is_complete());
    }
}
