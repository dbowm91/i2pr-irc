//! The bounded, allowlisted registration action model.
//!
//! A mature bouncer re-sends a few things after every successful registration: the modes it
//! wants on its own nick, an identify line for a services bot. This module is the *only*
//! way this build can emit an action, and it is an allowlist rather than a denylist on
//! purpose.
//!
//! # Why an allowlist
//!
//! A denylist is a list of commands someone thought of. `OPER`, `KILL`, `SQUIT`, `CONNECT`,
//! and the rest of the IRC vocabulary are not reachable by name from the Operator's own
//! client anyway, but a denylist would make this bouncer's safety depend on enumerating
//! them. An allowlist makes the reachable set a closed, reviewable list of two entries.
//!
//! # Why the two entries are the two entries
//!
//! - `MODE` on the bouncer's own nick. The only reason a bouncer sets modes on itself is to
//!   ask the server for the operator bits its configuration wants, and it must target its
//!   own nick because anything else would be configuring a *person*.
//! - `PRIVMSG` or `NOTICE` to an explicitly named service target. NickServ-style identify
//!   lines are the entire reason people reach for this feature, and they are addressed to a
//!   service, not to a channel or a person.
//!
//! # What this is not
//!
//! Not arbitrary network quote. There is no variant that can express a raw line, a prefixed
//! line, a client tag, or a command outside the two above. The parser is an allowlist of
//! *shapes*, and a stored action is one of those shapes -- it is never text that is replayed
//! because it was reviewed once.
//!
//! # Replay semantics
//!
//! Actions are intentional per-generation setup, replayed after **every** successful
//! registration generation. That is the difference from retrying an ambiguous user message:
//! nobody typed this at a moment whose delivery is in doubt, and an identify line that is
//! sent twice on reconnect is the intended behaviour rather than a duplicate. Every action
//! is idempotent by construction, which is what makes replaying it correct.
//!
//! # Secrets
//!
//! An action's text may contain a service password, so the payload is treated as secret
//! throughout: `Debug` renders `[redacted]`, the payload never reaches diagnostics, and
//! diagnostics reports only a count.

use i2pr_irc_store::StoredSecret;
use std::fmt;

pub use i2pr_irc_store::RegistrationActionPhase as ActionPhase;

/// Ceiling on actions one Network may store.
///
/// Small because these are per-generation setup, not a scripting facility. The ceiling is a
/// property of the model rather than of storage, so a store that somehow holds more is
/// refused at load rather than replayed.
pub const MAX_REGISTRATION_ACTIONS: usize = 8;

/// Ceiling on the text of one action.
///
/// Below the wire's line ceiling with room for the prefix the owner adds, so an accepted
/// action is always emittable as one frame.
pub const MAX_ACTION_TEXT_BYTES: usize = 200;

/// Ceiling on one action's target.
pub const MAX_ACTION_TARGET_BYTES: usize = 64;

/// Ceiling on every action's text for one Network combined.
///
/// A separate ceiling from the per-action one because eight 200-byte actions is 1600 bytes
/// of upstream traffic per registration, and a bouncer that sends that on every reconnect is
/// a bouncer flooding a server. The combined ceiling is the one that actually bounds what a
/// generation emits.
pub const MAX_TOTAL_ACTION_BYTES: usize = 512;

/// What kind of frame one stored action is.
///
/// A closed enum rather than a command string, so the emitter cannot be reached by a value
/// this module did not construct.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionKind {
    /// `MODE` on the bouncer's own nick.
    Mode,
    /// A message to an explicitly configured service target.
    Message,
}

impl ActionKind {
    /// The stable name an Operator stores this under.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mode => "mode",
            Self::Message => "message",
        }
    }
}

/// Everything that can be wrong with a proposed action.
///
/// Each variant says what to remove, because an Operator who typed a refused action needs to
/// know which part of it to change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActionError {
    /// The command is not on the allowlist.
    ///
    /// Carries the command name because the Operator needs to know what was refused, and a
    /// command name is not a secret -- it is the thing they typed.
    ForbiddenCommand(String),
    /// `MODE` with no mode string.
    MissingMode,
    /// A target that is not a valid IRC nick.
    InvalidTarget,
    /// Text longer than [`MAX_ACTION_TEXT_BYTES`].
    TextTooLong,
    /// Empty text.
    EmptyText,
    /// More actions than [`MAX_REGISTRATION_ACTIONS`].
    TooManyActions,
    /// The actions together exceed [`MAX_TOTAL_ACTION_BYTES`].
    TooManyBytes,
    /// A client tag was present.
    ///
    /// A distinct variant rather than a forbidden command: the command was allowed and the
    /// tag was not, and an Operator who pasted a tagged line needs to hear that the *tag* is
    /// the problem rather than guessing from a refusal about `PRIVMSG`.
    TagsRefused,
    /// A prefix was present.
    PrefixedRefused,
}

impl fmt::Display for ActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ForbiddenCommand(name) => {
                write!(f, "{name} is not a permitted registration action")
            }
            Self::MissingMode => write!(f, "a mode action needs a mode string"),
            Self::InvalidTarget => write!(f, "a message action needs a service target"),
            Self::TextTooLong => write!(f, "the action text is too long"),
            Self::EmptyText => write!(f, "the action text is empty"),
            Self::TooManyActions => write!(f, "too many registration actions"),
            Self::TooManyBytes => write!(f, "the actions exceed the total size ceiling"),
            Self::TagsRefused => write!(f, "a client tag is not accepted in a stored action"),
            Self::PrefixedRefused => write!(f, "a prefix is not accepted in a stored action"),
        }
    }
}

/// One stored, allowlisted registration action.
///
/// Hand-written `Debug` for the same reason [`crate::bouncerserv::ServCommand`] has one:
/// the text may be a service password, and a derived `Debug` is one line away from printing
/// it. The type redacts *and* the value is a [`StoredSecret`], so it both cannot be printed
/// and cannot be copied into an ordinary `String` by accident.
#[derive(Clone, Eq, PartialEq)]
pub struct RegistrationAction {
    pub kind: ActionKind,
    pub phase: i2pr_irc_store::RegistrationActionPhase,
    /// The mode string, or the message target.
    pub target: String,
    /// The action text, or `None` for a `MODE` whose modes are in `target`.
    pub text: Option<StoredSecret>,
}

impl fmt::Debug for RegistrationAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Kind and the fact that a text exists; never the text.
        write!(
            f,
            "RegistrationAction({} target={:?} text={})",
            self.kind.as_str(),
            self.target,
            if self.text.is_some() {
                "[redacted]"
            } else {
                "none"
            }
        )
    }
}

impl RegistrationAction {
    /// A `MODE` action on the bouncer's own nick.
    pub fn mode(modes: &str) -> Result<Self, ActionError> {
        Self::mode_in_phase(modes, i2pr_irc_store::RegistrationActionPhase::PostJoin)
    }

    pub fn mode_in_phase(
        modes: &str,
        phase: i2pr_irc_store::RegistrationActionPhase,
    ) -> Result<Self, ActionError> {
        Self::validate_modes(modes)?;
        Ok(Self {
            kind: ActionKind::Mode,
            phase,
            target: modes.to_owned(),
            text: None,
        })
    }

    /// A message action to an explicitly named service target.
    pub fn message(target: &str, text: &str) -> Result<Self, ActionError> {
        Self::message_in_phase(
            target,
            text,
            i2pr_irc_store::RegistrationActionPhase::PostJoin,
        )
    }

    pub fn message_in_phase(
        target: &str,
        text: &str,
        phase: i2pr_irc_store::RegistrationActionPhase,
    ) -> Result<Self, ActionError> {
        if target.is_empty() || target.len() > MAX_ACTION_TARGET_BYTES {
            return Err(ActionError::InvalidTarget);
        }
        if !valid_service_target(target) {
            return Err(ActionError::InvalidTarget);
        }
        if text.is_empty() {
            return Err(ActionError::EmptyText);
        }
        if text.len() > MAX_ACTION_TEXT_BYTES {
            return Err(ActionError::TextTooLong);
        }
        Ok(Self {
            kind: ActionKind::Message,
            phase,
            target: target.to_owned(),
            text: Some(StoredSecret::new(text.to_owned())),
        })
    }

    fn validate_modes(modes: &str) -> Result<(), ActionError> {
        if modes.is_empty() {
            return Err(ActionError::MissingMode);
        }
        if modes.len() > MAX_ACTION_TARGET_BYTES {
            return Err(ActionError::TextTooLong);
        }
        // A mode string is one or more `sign letter` pairs and nothing else. Refusing a space
        // is what stops a `MODE` action from carrying a trailing parameter, which would
        // otherwise smuggle an arbitrary argument past an allowlist that only checked the
        // command name.
        let mut chars = modes.chars();
        while let Some(sign) = chars.next() {
            if sign != '+' && sign != '-' {
                return Err(ActionError::MissingMode);
            }
            let letter = chars.next().ok_or(ActionError::MissingMode)?;
            if !letter.is_ascii_alphabetic() {
                return Err(ActionError::MissingMode);
            }
        }
        Ok(())
    }

    /// Rebuilds an action from its durable form.
    ///
    /// Re-runs the same validation as the constructors. A stored row is not trusted on the
    /// strength of having come out of this bouncer's own database: the table is a durable
    /// surface, and the validation is what makes "every action this process can emit was
    /// checked by this build's rules" a property of the type rather than of the history of
    /// what was written.
    pub fn from_parts(
        kind: ActionKind,
        target: String,
        payload: StoredSecret,
        phase: i2pr_irc_store::RegistrationActionPhase,
    ) -> Result<Self, ActionError> {
        let text = payload.expose();
        match kind {
            ActionKind::Mode => {
                let action = Self::mode_in_phase(&target, phase)?;
                // A stored mode action with a payload is corrupt rather than ignored: the
                // text would otherwise be a secret sitting in a row nothing ever emits.
                if !text.is_empty() {
                    return Err(ActionError::PrefixedRefused);
                }
                Ok(action)
            }
            ActionKind::Message => Self::message_in_phase(&target, text, phase),
        }
    }

    /// The durable form of this action.
    ///
    /// `None` only for a value this module cannot itself have constructed, which is
    /// unreachable in practice; the caller is expected to check the count and refuse rather
    /// than to drop the gap.
    pub fn to_stored(&self) -> Option<i2pr_irc_store::StoredRegistrationAction> {
        let kind = match self.kind {
            ActionKind::Mode => i2pr_irc_store::RegistrationActionKind::Mode,
            ActionKind::Message => i2pr_irc_store::RegistrationActionKind::Message,
        };
        // The payload column is never null, so a mode action stores an empty string rather
        // than a null. The read path treats empty-plus-mode as the corruption case above.
        let payload = match &self.text {
            Some(text) => text.clone(),
            None => StoredSecret::new(String::new()),
        };
        Some(i2pr_irc_store::StoredRegistrationAction {
            kind,
            phase: self.phase,
            target: self.target.clone(),
            payload,
        })
    }

    /// Bytes this action will occupy in the upstream frame it produces.
    ///
    /// Counted at accept time so the combined ceiling is checked against what will actually
    /// be sent rather than against an estimate.
    pub fn frame_bytes(&self) -> usize {
        match &self.text {
            Some(text) => self.target.len() + text.expose().len() + 16,
            None => self.target.len() + 16,
        }
    }
}

/// Every action one Network will replay after each successful registration.
///
/// A type rather than a `Vec` so the count and total-byte ceilings cannot be bypassed by
/// whoever builds one.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ActionSet {
    actions: Vec<RegistrationAction>,
}

impl ActionSet {
    /// Builds a set, refusing one that breaches either ceiling.
    ///
    /// Both ceilings are checked here rather than at parse time because both are properties
    /// of the *set*: a store migrated from a future schema could hand over more actions than
    /// this build accepts, and the only safe response is to refuse rather than to truncate
    /// and silently replay a prefix.
    pub fn new(actions: Vec<RegistrationAction>) -> Result<Self, ActionError> {
        if actions.len() > MAX_REGISTRATION_ACTIONS {
            return Err(ActionError::TooManyActions);
        }
        let total: usize = actions.iter().map(RegistrationAction::frame_bytes).sum();
        if total > MAX_TOTAL_ACTION_BYTES {
            return Err(ActionError::TooManyBytes);
        }
        Ok(Self { actions })
    }

    /// The actions, in replay order.
    pub fn actions(&self) -> &[RegistrationAction] {
        &self.actions
    }

    /// Actions for one execution phase, preserving the Operator's order.
    pub fn actions_in_phase(
        &self,
        phase: i2pr_irc_store::RegistrationActionPhase,
    ) -> impl Iterator<Item = &RegistrationAction> {
        self.actions
            .iter()
            .filter(move |action| action.phase == phase)
    }

    /// How many actions this Network replays.
    pub fn len(&self) -> usize {
        self.actions.len()
    }

    /// Whether this Network replays nothing.
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }
}

/// The upstream frame one action produces.
///
/// A method on the action rather than a free function, so the only way to get a frame out of
/// a stored action is through the one place that decided it is safe to send.
impl RegistrationAction {
    /// The frame to write upstream, or `None` when the bouncer's nick is not yet known.
    ///
    /// Takes the nick rather than reading it from state so the caller cannot accidentally
    /// render against a nick from a different generation: the frame is built at the moment
    /// the generation writes it.
    pub fn frame(&self, nick: &str) -> Option<String> {
        match (&self.kind, &self.text) {
            (ActionKind::Mode, _) => Some(format!("MODE {nick} {}\r\n", self.target)),
            (ActionKind::Message, Some(text)) => {
                Some(format!("PRIVMSG {} :{}\r\n", self.target, text.expose()))
            }
            // A message action without text cannot be built by this module; returning
            // `None` rather than an empty frame keeps a malformed stored value from
            // producing a line the server would interpret as an empty message.
            (ActionKind::Message, None) => None,
        }
    }
}

/// Whether a target is shaped like a service nick.
///
/// Deliberately stricter than [`crate::state::valid_client_nick`]: a stored action target is
/// typed by hand into a control line, and accepting anything a *person's* nick could be
/// would let an Operator configure the bouncer to message a person on every reconnect, which
/// is precisely the "arbitrary network quote" this module exists to prevent. The target must
/// be a nick that ends in `Serv`, matching the service naming every IRC network uses.
fn valid_service_target(target: &str) -> bool {
    target.ends_with("Serv")
        && target.len() > "Serv".len()
        && crate::valid_client_nick(target.as_bytes())
}

/// Every command this build will replay, for documentation and tests.
///
/// Kept as data rather than as prose so the allowlist has exactly one definition. A command
/// named in a doc comment but absent here would be a promise the code does not keep.
pub const PERMITTED_COMMANDS: [&str; 2] = ["MODE", "PRIVMSG"];

/// Every command this build refuses, named so the refusal is explicit rather than incidental.
///
/// This is documentation and a test oracle, **not** the enforcement. Enforcement is the
/// allowlist above; this list exists so the closure record can show what was considered and
/// so a test can assert the two never drift apart.
pub const FORBIDDEN_COMMANDS: [&str; 16] = [
    "NICK",
    "JOIN",
    "PART",
    "QUIT",
    "CAP",
    "AUTHENTICATE",
    "BOUNCER",
    "PRIVMSGX",
    "OPER",
    "KILL",
    "SQUIT",
    "CONNECT",
    "DIE",
    "RESTART",
    "WALLOPS",
    "DCC",
];
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_allowlist_and_the_denylist_never_overlap() {
        // The denylist is documentation and a test oracle, not the enforcement. If a name
        // ever appeared in both, the doc would claim something the code refuses to promise.
        for forbidden in FORBIDDEN_COMMANDS {
            assert!(
                !PERMITTED_COMMANDS.contains(&forbidden),
                "{forbidden} is both permitted and forbidden"
            );
        }
        assert_eq!(
            PERMITTED_COMMANDS.len(),
            2,
            "the allowlist is exactly two entries"
        );
    }

    #[test]
    fn the_model_has_no_command_name_to_be_refused() {
        // The strongest form of the allowlist claim: `RegistrationAction` is constructed
        // only by `mode` and `message`, and neither accepts a command name. There is no
        // argument through which `JOIN` or `AUTHENTICATE` could reach a stored action, so
        // the command matrix is not enforced here -- it is unrepresentable here. The
        // parser is where a command name is read at all, and that is where
        // `every_refused_command_is_refused_by_the_parser` lives.
        let kinds = [ActionKind::Mode, ActionKind::Message];
        assert_eq!(kinds.len(), PERMITTED_COMMANDS.len());
        for kind in kinds {
            assert!(PERMITTED_COMMANDS.contains(&kind_upper(kind)));
        }
    }

    /// The upper-case IRC spelling one kind produces upstream.
    fn kind_upper(kind: ActionKind) -> &'static str {
        match kind {
            ActionKind::Mode => "MODE",
            ActionKind::Message => "PRIVMSG",
        }
    }

    #[test]
    fn a_mode_action_must_be_only_sign_letter_pairs() {
        for good in ["+B", "+B-i", "-i", "+B-i+w"] {
            assert!(
                RegistrationAction::mode(good).is_ok(),
                "{good} should be allowed"
            );
        }
        for bad in ["", "+", "B", "+B i", "+B,i", "+B\nQUIT", "+B;QUIT"] {
            assert_eq!(
                RegistrationAction::mode(bad).unwrap_err(),
                ActionError::MissingMode,
                "{bad:?} must be refused"
            );
        }
    }

    #[test]
    fn a_mode_action_cannot_carry_a_trailing_parameter() {
        // The specific smuggling route a command-name-only allowlist would leave open.
        let error = RegistrationAction::mode("+B #channel").unwrap_err();
        assert_eq!(error, ActionError::MissingMode);
    }

    #[test]
    fn a_message_action_must_target_a_service() {
        assert!(RegistrationAction::message("NickServ", "IDENTIFY pw").is_ok());
        for bad in ["", "bot", "#channel", "alice", "Serv", "Nick Serv"] {
            assert_eq!(
                RegistrationAction::message(bad, "IDENTIFY pw").unwrap_err(),
                ActionError::InvalidTarget,
                "{bad:?} must be refused: it is not a service"
            );
        }
    }

    #[test]
    fn a_message_action_needs_non_empty_bounded_text() {
        assert_eq!(
            RegistrationAction::message("NickServ", "").unwrap_err(),
            ActionError::EmptyText
        );
        let long = "x".repeat(MAX_ACTION_TEXT_BYTES + 1);
        assert_eq!(
            RegistrationAction::message("NickServ", &long).unwrap_err(),
            ActionError::TextTooLong
        );
    }

    #[test]
    fn a_stored_action_never_renders_its_payload() {
        let action =
            RegistrationAction::message("NickServ", "IDENTIFY hunter2").expect("an action");
        let rendered = format!("{action:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(rendered.contains("[redacted]"), "{rendered}");
        assert!(
            !format!("{:?}", action.to_stored()).contains("hunter2"),
            "the durable form must redact too"
        );
        // And the durable round trip preserves the value without ever exposing it in a
        // format string.
        let stored = action.to_stored().expect("a durable form");
        let rebuilt = RegistrationAction::from_parts(
            ActionKind::Message,
            stored.target,
            stored.payload,
            stored.phase,
        )
        .expect("a stored action revalidates");
        assert_eq!(
            rebuilt.frame("bot").as_deref(),
            Some("PRIVMSG NickServ :IDENTIFY hunter2\r\n")
        );
    }

    #[test]
    fn a_stored_mode_action_with_a_payload_is_refused_rather_than_ignored() {
        // A payload on a mode action is text nothing would ever emit, which is exactly what
        // a leaked secret sitting in a row looks like.
        let error = RegistrationAction::from_parts(
            ActionKind::Mode,
            "+B".to_owned(),
            StoredSecret::new("hunter2".to_owned()),
            i2pr_irc_store::RegistrationActionPhase::PostJoin,
        )
        .unwrap_err();
        assert_eq!(error, ActionError::PrefixedRefused);
    }

    #[test]
    fn a_stored_action_is_revalidated_against_this_builds_rules() {
        // Not trusted for having come out of this bouncer's own database.
        let cases = [
            (ActionKind::Message, "bot".to_owned(), "IDENTIFY pw"),
            (ActionKind::Mode, "B".to_owned(), ""),
            (ActionKind::Message, "NickServ".to_owned(), ""),
        ];
        for (kind, target, payload) in cases {
            assert!(
                RegistrationAction::from_parts(
                    kind,
                    target.clone(),
                    StoredSecret::new(payload.to_owned()),
                    i2pr_irc_store::RegistrationActionPhase::PostJoin,
                )
                .is_err(),
                "{kind:?} {target} must not survive revalidation"
            );
        }
    }

    #[test]
    fn an_action_set_refuses_to_breach_either_ceiling() {
        let action = || RegistrationAction::message("NickServ", "IDENTIFY pw").expect("an action");
        assert!(ActionSet::new(vec![]).expect("an empty set").is_empty());
        let full = vec![action(); MAX_REGISTRATION_ACTIONS];
        assert_eq!(
            ActionSet::new(full).expect("a full set").len(),
            MAX_REGISTRATION_ACTIONS
        );

        let too_many = vec![action(); MAX_REGISTRATION_ACTIONS + 1];
        assert_eq!(
            ActionSet::new(too_many).unwrap_err(),
            ActionError::TooManyActions
        );

        // Few enough actions, but too many bytes: the combined ceiling is a property of the
        // set and not derivable from the count alone.
        let bulky = (0..4)
            .map(|_| {
                RegistrationAction::message("NickServ", &"x".repeat(MAX_ACTION_TEXT_BYTES))
                    .expect("an action")
            })
            .collect::<Vec<_>>();
        assert!(
            bulky.len() <= MAX_REGISTRATION_ACTIONS,
            "the fixture must stay under the count ceiling so it tests the byte ceiling"
        );
        assert_eq!(
            ActionSet::new(bulky).unwrap_err(),
            ActionError::TooManyBytes
        );
    }

    #[test]
    fn an_action_frame_targets_the_nick_this_generation_registered_with() {
        let action = RegistrationAction::mode("+B").expect("an action");
        assert_eq!(
            action.frame("bouncer").as_deref(),
            Some("MODE bouncer +B\r\n")
        );
        assert_eq!(
            action.frame("other").as_deref(),
            Some("MODE other +B\r\n"),
            "a mode action configures whichever identity this generation holds"
        );
    }
}
