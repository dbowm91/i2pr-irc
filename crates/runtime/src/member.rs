//! Per-session mediation of richer member-state IRCv3 semantics.
//!
//! The upstream negotiation is one fixed decision made once per generation, but the set
//! of attached clients is not fixed at all. This module is where those two facts meet: it
//! decides, for one session, what that session may be shown of one upstream line.
//!
//! Three mechanisms, and the difference between them matters:
//!
//! - **Withhold.** A message form a client never negotiated. `ACCOUNT`, `AWAY`, and
//!   `SETNAME` exist only because a capability was negotiated, so a client without one is
//!   sent nothing rather than an event shape it cannot interpret.
//! - **Reduce.** A frame that is well formed for the bouncer and unreadable for one
//!   client. An extended `JOIN` and a complete prefix run both fall here: the upstream
//!   was entitled to send them, and the client is not entitled to receive them.
//! - **Pass.** Everything else, unchanged.
//!
//! Nothing here fabricates. A reduction removes information the receiving client cannot
//! use; it never adds information the upstream did not send, and it never fills in a field
//! the bouncer did not observe. That last rule is what keeps an unobserved realname out
//! of a JOIN instead of appearing as an empty string.
use crate::{
    session::SessionCapabilities,
    state::{AccountState, NetworkState, PrefixMap},
};
use i2pr_irc_wire::Message;

/// What one attached session may be shown of one upstream line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Mediated {
    /// Deliver the frame unchanged.
    Pass,
    /// Deliver a different rendering of the same observed facts.
    Rewritten(Message),
    /// Withhold the frame entirely.
    Withhold,
}

impl Mediated {
    fn gate(permitted: bool) -> Self {
        if permitted {
            Self::Pass
        } else {
            Self::Withhold
        }
    }
}

/// Decides what one session may be shown of one fanned-out upstream line.
///
/// `state` is consulted only where a decision genuinely depends on this generation's
/// observed facts. Everything else is a property of the line's shape and the session's
/// negotiation, so the same answer holds for every client on the Network.
pub fn mediate(state: &NetworkState, message: &Message, caps: &SessionCapabilities) -> Mediated {
    let _ = state;
    let command = message.command.as_slice();
    // Each of these three exists *only* when a capability was negotiated, in both
    // directions. A server emits them because the bouncer asked for the capability, and
    // the bouncer relays them to a session because that session asked for it. A session
    // that asked for none of them has no way to know what any of them mean.
    if command.eq_ignore_ascii_case(b"ACCOUNT") {
        return Mediated::gate(caps.negotiated_account_notify());
    }
    if command.eq_ignore_ascii_case(b"AWAY") {
        return Mediated::gate(caps.negotiated_away_notify());
    }
    if command.eq_ignore_ascii_case(b"SETNAME") {
        return Mediated::gate(caps.negotiated_setname());
    }
    // `extended-join` is the one member form that changes a JOIN's parameter list, so it
    // is the one that cannot simply be withheld: a client that never negotiated it would
    // read the trailing account and realname as unrelated parameters and mis-parse the
    // whole line. It is reduced to the plain form instead.
    if command.eq_ignore_ascii_case(b"JOIN") && message.params.len() >= 3 {
        return match caps.negotiated_extended_join() {
            true => Mediated::Pass,
            false => Mediated::Rewritten(reduce_join(message)),
        };
    }
    Mediated::Pass
}

/// Drops the `extended-join` trailing parameters from a JOIN.
///
/// Everything kept is what a plain JOIN is: the prefix, the command, and the channel. The
/// tags are the upstream's own and travel unchanged, because a client that negotiated
/// `message-tags` asked for them.
///
/// `truncate_params` also clears the trailing marker, because the trailing parameter *was*
/// the realname. Left set, the encoder would render the surviving channel as a trailing
/// argument (`JOIN :#room`) -- parseable, since `:` is only a delimiter, but no server
/// emits a channel JOIN that way.
fn reduce_join(message: &Message) -> Message {
    message.truncate_params(1)
}

/// Degrades one routed reply for a session that did not negotiate `multi-prefix`.
///
/// A reply is delivered only to the client that asked for it, so this runs per session
/// rather than per line and needs no withholding: the question is only whether the client
/// can read a prefix run it was not offered.
///
/// Returns `Pass` when the reply carries nothing that needs reducing, which is the case
/// for every reply other than the two that embed membership prefixes.
pub fn degrade_routed_reply(message: &Message, prefix: &PrefixMap, multi_prefix: bool) -> Mediated {
    if multi_prefix {
        return Mediated::Pass;
    }
    let command = message.command.as_slice();
    // RPL_WHOREPLY: the membership prefixes live at the end of the flags field, after the
    // two status characters RFC 2812 fixes at its start.
    if command.eq_ignore_ascii_case(b"352") && message.params.len() >= 7 {
        let raw = String::from_utf8_lossy(&message.params[6]).into_owned();
        let Some(reduced) = reduce_who_flags(&raw, prefix) else {
            return Mediated::Pass;
        };
        let mut rewritten = message.clone();
        rewritten.params[6] = reduced.into_bytes();
        return Mediated::Rewritten(rewritten);
    }
    // RPL_WHOISCHANNELS: the last parameter is a space-separated list whose entries each
    // carry a prefix run.
    if command.eq_ignore_ascii_case(b"319") && message.params.len() >= 2 {
        let raw = String::from_utf8_lossy(message.params.last().expect("checked length"));
        let Some(reduced) = reduce_channel_list(&raw, prefix) else {
            return Mediated::Pass;
        };
        let mut rewritten = message.clone();
        if let Some(last) = rewritten.params.last_mut() {
            *last = reduced.into_bytes();
        }
        return Mediated::Rewritten(rewritten);
    }
    Mediated::Pass
}

/// Reduces a WHO flags field to the single membership symbol a legacy client can read.
///
/// The flags field has no fixed position for the membership run: RFC 2812 fixes only the
/// leading `H`/`*` online marker and the optional `G`/`g` operator marker, and a
/// `multi-prefix` server appends the run after them. So the run is found by scanning for
/// advertised membership symbols rather than by counting from the start, and the highest
/// one is written back at the position the run occupied. Every other character keeps its
/// place and its meaning.
///
/// The six characters RFC 2812 assigns to the field itself are never treated as
/// membership symbols, whatever the server's `PREFIX` map happens to contain. A server
/// that advertised `H` or `G` as a mode would otherwise have this rewrite delete a status
/// flag that has nothing to do with channel membership.
fn reduce_who_flags(flags: &str, prefix: &PrefixMap) -> Option<String> {
    let symbols: Vec<char> = flags
        .chars()
        .filter(|symbol| !is_who_flag_placeholder(*symbol) && prefix.mode_for(*symbol).is_some())
        .collect();
    let highest = prefix.highest(&symbols)?;
    let mut reduced = String::with_capacity(flags.len());
    let mut written = false;
    for symbol in flags.chars() {
        if !is_who_flag_placeholder(symbol) && prefix.mode_for(symbol).is_some() {
            if !written {
                reduced.push(highest);
                written = true;
            }
        } else {
            reduced.push(symbol);
        }
    }
    Some(reduced)
}

/// True for the characters RFC 2812 reserves inside a WHO flags field.
///
/// These are status and transport markers with fixed positions, not membership symbols.
/// `H`/`*` say whether the user is online, `G`/`g` whether they are an IRC operator, and
/// `?`/`!` mark IRCOp and TLS status. Treating any of them as a prefix symbol because a
/// server listed it in `PREFIX` would corrupt the field for every client.
fn is_who_flag_placeholder(symbol: char) -> bool {
    matches!(symbol, 'H' | '*' | 'G' | 'g' | '?' | '!')
}

/// Reduces one RPL_WHOISCHANNELS list to single-symbol entries.
///
/// Returns `None` when nothing needed reducing, so an already-compatible reply is relayed
/// byte for byte rather than re-encoded into a shape that differs only invisibly.
fn reduce_channel_list(list: &str, prefix: &PrefixMap) -> Option<String> {
    let mut changed = false;
    let entries: Vec<String> = list
        .split(' ')
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let (symbols, bare) = prefix.split_prefix_run(entry);
            match prefix.highest(&symbols) {
                Some(highest) if symbols.len() > 1 => {
                    changed = true;
                    format!("{highest}{bare}")
                }
                _ => entry.to_owned(),
            }
        })
        .collect();
    changed.then(|| entries.join(" "))
}

/// Renders this bouncer's own channel JOIN for one session.
///
/// An extended JOIN is emitted only when the session negotiated `extended-join` *and* both
/// fields were actually observed. The account in particular is not defaulted to `*` when
/// nothing has been observed: `*` is the spec's statement that the server told us this
/// member is not logged in, and rendering it from an absence would be that statement made
/// up. An unobserved profile therefore yields the plain JOIN, which is the shape every
/// client can read.
pub fn own_join_line(state: &NetworkState, channel: &str, caps: &SessionCapabilities) -> String {
    if caps.negotiated_extended_join() {
        let (account, realname) = state.own_profile();
        if let (AccountState::Known(account), Some(realname)) = (account, realname) {
            let account = account.unwrap_or_else(|| "*".to_owned());
            return format!(":{} JOIN {channel} {account} :{realname}\r\n", state.nick);
        }
    }
    format!(":{} JOIN {channel}\r\n", state.nick)
}

/// The `NAMELEN` token a `setname` session must be told about, if nobody else did.
///
/// `setname` requires the server to publish a maximum realname length, and this bouncer
/// is that server for the sessions attached to it. The value is the tighter of what
/// upstream advertised and the bouncer's own hard ceiling, because advertising a length
/// the bouncer would then refuse to retain would invite clients to send names that are
/// silently dropped.
///
/// It is emitted only to a session that negotiated `setname`: a client that never asked
/// for the semantics has no use for the ceiling, and unsolicited `005` tokens are noise.
/// And it is emitted at all only when upstream published no `NAMELEN` of its own, because
/// the upstream token is relayed verbatim and two answers to one question is worse than
/// one.
pub fn namelen_token(state: &NetworkState, caps: &SessionCapabilities) -> Option<String> {
    (caps.negotiated_setname() && !state.upstream_published_namelen())
        .then(|| format!("NAMELEN={}", state.namelen_ceiling()))
}
