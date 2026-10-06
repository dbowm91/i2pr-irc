//! Downstream admission: what happens between a client socket and a Network owner.
//!
//! # The problem this solves
//!
//! Before M005-A a client connection was accepted by handing the whole socket to the
//! Network owner immediately. That made three things impossible:
//!
//! * a client that registers with no Network selected had nowhere to be refused;
//! * a client refused before registration had no way to learn *why*, because the only
//!   component holding its write half was the owner that had just rejected it;
//! * the owner's per-session ceiling was the only admission control, so a client flood
//!   against a full Network spent owner work to be told the answer was no.
//!
//! # What admission owns
//!
//! Admission takes ownership of the socket at accept time: it splits the stream, starts
//! the writer task, allocates the ephemeral [`SessionId`], and drives registration. Only
//! when registration completes does it decide where the client goes:
//!
//! * a selected Network gets a [`PreparedSession`] handed to that Network's owner, or
//! * no Network was selected, and the client stays here as a control-only session.
//!
//! Nothing in this module can reach storage, an upstream connection, or another
//! Network's state. It submits one typed request through a [`RuntimeControlHandle`]
//! and otherwise only speaks IRC to the one client it accepted.

use crate::{
    RuntimeError,
    downstream::DownstreamDisposition,
    session::{ClientWiring, RefusedRegistration, SessionHandle, SessionIntent},
};
use i2pr_irc_core::{ByteStream, ClientId, NetworkId, SessionId};
use std::collections::BTreeSet;

/// How long one client may take to complete registration before it is dropped.
///
/// Bounded because an accepted socket is process state. Without a ceiling a client
/// could open a connection, send nothing, and hold a writer task and read half
/// indefinitely; the ceiling turns that into an ordinary idle disconnect.
pub const ADMISSION_REGISTRATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// The nickname an unbound client is told to use in operator-facing messages.
const UNBOUND_NICK_PLACEHOLDER: &str = "*";

/// What a client was told, and which Network it was bound to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Disposition {
    /// Registration completed and the client was handed to a Network owner.
    Bound,
    /// Registration completed with no Network selected. The client can still run the
    /// protocol, but it has no channel list, no upstream projection, and no history.
    Unbound,
    /// Registration never completed.
    Refused,
}

/// One registered local session, ready to be handed to a Network owner.
///
/// The type is deliberately one-shot: it is consumed by
/// [`crate::RuntimeControlHandle::bind`], which sends it down a bounded queue to
/// exactly one owner. A session that has been transferred cannot be transferred again,
/// so a client cannot end up owned by two Networks, and a retry after a lost reply
/// cannot create a second view of the same conversation.
///
/// It is `Debug`-free on purpose. It holds a live socket and negotiated capabilities,
/// neither of which belongs in a diagnostic string.
pub struct PreparedSession {
    wiring: ClientWiring,
    /// Selected before registration began, so the reader could already enforce the
    /// Network's nickname. `None` for a control-only client.
    network: Option<NetworkId>,
    /// Capabilities negotiated during registration.
    negotiated: BTreeSet<String>,
}

impl PreparedSession {
    pub(crate) fn new(wiring: ClientWiring, network: Option<NetworkId>) -> Self {
        let negotiated = wiring.negotiated().clone();
        Self {
            wiring,
            network,
            negotiated,
        }
    }

    /// The Network this session is bound to.
    pub fn network(&self) -> Option<NetworkId> {
        self.network
    }

    /// The nickname this client registered with.
    pub fn registered_nick(&self) -> Option<&str> {
        self.wiring.registered_nick()
    }

    /// The capabilities this client negotiated during registration.
    pub fn negotiated(&self) -> &BTreeSet<String> {
        &self.negotiated
    }

    pub(crate) fn handle(&self) -> &SessionHandle {
        self.wiring.handle()
    }

    /// The ephemeral identity admission allocated for this connection.
    ///
    /// It is fixed from accept time and is never reassigned by a transfer, so a reply
    /// scoped to this attachment can never reach a later one.
    pub fn session_id(&self) -> SessionId {
        self.wiring.session()
    }

    /// Consumes the session for hand-off to its Network owner.
    pub(crate) fn into_wiring(self) -> ClientWiring {
        self.wiring
    }

    /// Records the negotiated capabilities on the session handle.
    ///
    /// Must be called before the owner reads `capabilities()`; a client that negotiated
    /// a capability and is then treated as not having it would receive unsolicited
    /// traffic, and one treated as having a capability it never asked for would be
    /// denied a reply it expects.
    pub(crate) fn publish_negotiated(&self) {
        self.wiring.set_negotiated(&self.negotiated);
    }
}

/// Accepts exactly one local connection and decides where it goes.
///
/// Constructing a `DownstreamAdmission` allocates the session identity and splits the
/// socket; it does not connect upstream, read storage, or touch any Network state.
pub struct DownstreamAdmission {
    /// The Network this client was selected onto, decided by the operator's
    /// configuration before the connection was accepted.
    selected: Option<NetworkSelection>,
    control: crate::RuntimeControlHandle,
    session: SessionId,
    client: ClientId,
    /// How long this client may take to register.
    ///
    /// A ceiling, not a preference: an accepted socket is process state, and without
    /// one a client could connect, send nothing, and hold a writer task and read half
    /// for as long as it liked.
    registration_timeout: std::time::Duration,
}

impl DownstreamAdmission {
    /// Builds an admission for one accepted connection.
    ///
    /// `selected` is the Network the operator's binding selected for this client, or
    /// `None` when no binding applies. `expected_nick` is that Network's registered
    /// nickname, which admission enforces during registration so a client cannot claim
    /// an identity its Network did not register.
    pub fn new(
        selected: Option<NetworkSelection>,
        control: crate::RuntimeControlHandle,
        session: SessionId,
        client: ClientId,
    ) -> Self {
        Self {
            selected,
            control,
            session,
            client,
            registration_timeout: ADMISSION_REGISTRATION_TIMEOUT,
        }
    }

    /// Builds an admission with an explicit registration ceiling.
    ///
    /// Production uses [`DownstreamAdmission::new`]. Tests use this so the timeout path
    /// is exercised in milliseconds instead of minutes, which is the difference between
    /// a ceiling that is proven and a ceiling that is merely present.
    pub fn with_registration_timeout(
        selected: Option<NetworkSelection>,
        control: crate::RuntimeControlHandle,
        session: SessionId,
        client: ClientId,
        timeout: std::time::Duration,
    ) -> Self {
        Self {
            registration_timeout: timeout,
            ..Self::new(selected, control, session, client)
        }
    }

    pub fn session(&self) -> SessionId {
        self.session
    }

    pub fn client(&self) -> ClientId {
        self.client
    }

    /// The Network this client was selected onto, if any.
    pub fn selected_network(&self) -> Option<NetworkId> {
        self.selected.as_ref().map(|selection| selection.network)
    }

    /// Runs registration and hands the client to its destination.
    ///
    /// Every exit path ends the client. A bound session's ownership moves to the owner;
    /// an unbound session is driven to completion here; a refused one is written its
    /// reason and closed. There is no path that returns while leaving a socket, writer
    /// task, or read half alive with nobody driving them.
    pub async fn run(self, stream: Box<dyn ByteStream>) -> AdmissionOutcome {
        let expected_nick = self
            .selected
            .as_ref()
            .map(|selection| selection.expected_nick.clone());
        // The bindable set is the controller's own bounded snapshot of which Networks
        // exist. It is read once, at accept time, so a `BOUNCER BIND` can be answered
        // with a `FAIL` *during* registration rather than only after it.
        let snapshot = self.control.subscribe_status().borrow().clone();
        let bindable: std::collections::BTreeSet<NetworkId> = snapshot
            .networks
            .iter()
            .map(|entry| entry.network)
            .take(crate::bouncer_networks::MAX_BOUNCER_BATCH)
            .collect();
        let wiring = ClientWiring::new(self.session, self.client, expected_nick, bindable, stream);
        // Registration reports no intents upward: nothing has claimed this client yet,
        // and forwarding a pre-registration intent to an owner that does not exist is
        // exactly the coupling admission exists to remove. The ceiling is applied inside
        // `register` so the write half survives it and the client can be told why.
        let registered = match wiring.register(self.registration_timeout).await {
            Ok(registered) => registered,
            Err(refused) => {
                // The client is told, on the socket it opened, and then closed. A
                // client that connects and then says nothing is told so, rather than
                // simply dropped: silence is indistinguishable from a fault.
                self.refuse(refused, "451", "Registration did not complete")
                    .await;
                return AdmissionOutcome::Refused;
            }
        };

        // A pre-registration `BOUNCER BIND` is resolved here, after registration
        // completed and before anything is handed anywhere. The netid was already
        // checked against the snapshot taken at accept time; the binding is now real.
        let selection = self
            .selected
            .as_ref()
            .map(|selection| selection.network)
            .or_else(|| registered.bind_request());
        match selection {
            Some(network) => {
                let prepared = PreparedSession::new(registered, Some(network));
                match self.control.bind(network, prepared).await {
                    Ok(()) => AdmissionOutcome::Bound {
                        network,
                        session: self.session,
                    },
                    Err(_) => AdmissionOutcome::Refused,
                }
            }
            None => {
                // No Network claimed this client. It keeps a live socket and a working
                // protocol, but it is told plainly that it has no channel list, so it
                // cannot mistake a control-only session for a bouncer session.
                let prepared = PreparedSession::new(registered, None);
                let control = self.control.clone();
                serve_unbound(prepared, control).await;
                AdmissionOutcome::Unbound {
                    session: self.session,
                }
            }
        }
    }

    /// Writes the refusal reason, then closes the socket.
    async fn refuse(self, refused: RefusedRegistration, code: &str, text: &str) {
        let _ = self.control;
        refused.refuse(code, text).await;
    }
}

/// Which Network an operator's binding selected for one client, and the nickname that
/// Network registered.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkSelection {
    pub network: NetworkId,
    pub expected_nick: String,
}

/// What admitting one connection produced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionOutcome {
    /// The client registered and now belongs to `network`.
    Bound {
        network: NetworkId,
        session: SessionId,
    },
    /// The client registered with no Network selected and was served locally.
    Unbound { session: SessionId },
    /// The client was refused; its socket is closed.
    Refused,
}

impl AdmissionOutcome {
    pub fn disposition(&self) -> Disposition {
        match self {
            Self::Bound { .. } => Disposition::Bound,
            Self::Unbound { .. } => Disposition::Unbound,
            Self::Refused => Disposition::Refused,
        }
    }
}

/// Serves a registered client that no Network claimed.
///
/// This is not a reduced bouncer. It runs the same registered-command surface, but
/// every command that needs a Network -- channel membership, history, read markers,
/// upstream forwarding -- is refused by name, and the refusal says why. The client can
/// still negotiate capabilities, so a later release can hand it a view without the
/// protocol having to change shape.
async fn serve_unbound(
    prepared: PreparedSession,
    control: crate::controller::RuntimeControlHandle,
) {
    let handle = prepared.handle().clone();
    let nick = prepared
        .registered_nick()
        .map_or_else(|| UNBOUND_NICK_PLACEHOLDER.to_owned(), str::to_owned);

    // Two surfaces over one controller, because two tasks need it. The command handler
    // answers from fresh controller state; the notification task keeps the one revision
    // this client was last told about, which is the only thing that needs to persist
    // between turns. Sharing one mutable surface between two tasks would need a lock
    // around every reply, and there is nothing to protect: the two never disagree,
    // because one is the definition of what to say and the other is a record of what was
    // said.
    let commands = std::sync::Arc::new(tokio::sync::Mutex::new(
        crate::control_session::ControlSurface::new(control.clone(), handle.clone(), nick.clone()),
    ));
    commands.lock().await.send_initial_batch().await;

    let mut notify =
        crate::control_session::ControlSurface::new(control.clone(), handle.clone(), nick.clone());
    let mut status = control.subscribe_status();
    // The notification task ends with the connection. It is aborted below rather than
    // left to notice a closed socket on its own: a task that outlives the session it was
    // announcing to is a task writing to a client nobody is reading.
    let notifier = tokio::spawn(async move {
        loop {
            if status.changed().await.is_err() {
                return;
            }
            notify.publish_changes().await;
        }
    });

    // A bounded, fixed welcome burst. It names no channel, no endpoint, and nothing
    // about the bouncer's upstream identity.
    for line in [
        format!(":bouncer 001 {nick} :Welcome to the bouncer control session\r\n"),
        format!(":bouncer 002 {nick} :This session is not attached to a network\r\n"),
        format!(":bouncer 003 {nick} :Select a network with BOUNCER BIND\r\n"),
        format!(":bouncer 376 {nick} :End of MOTD\r\n"),
    ] {
        if handle.queue_control(&line).is_err() {
            notifier.abort();
            return;
        }
    }

    let reply_handle = handle.clone();
    prepared
        .into_wiring()
        .serve_locally(move |intent| {
            let commands = commands.clone();
            let handle = reply_handle.clone();
            let nick = nick.clone();
            async move {
                match intent {
                    SessionIntent::Quit => {}
                    SessionIntent::Control { wire } => {
                        // `serve_locally` runs one intent at a time, so this lock is
                        // uncontended in practice. It exists only so the handler closure
                        // can be `FnMut` over a stateful surface; there is no concurrent
                        // caller to reason about and none is introduced.
                        commands.lock().await.dispatch(&wire).await;
                    }
                    // Every intent that needs a Network is refused by name, and the
                    // refusal says why. Silence would leave a client believing the bouncer
                    // had understood it.
                    other => {
                        let _ = other;
                        let _ = handle.queue_control(&format!(
                            ":bouncer 421 {nick} * :No network selected for this session\r\n"
                        ));
                    }
                }
            }
        })
        .await;
    notifier.abort();
}

/// The error a caller reports when admission itself could not run.
///
/// Split from [`RuntimeError`] so "the client was refused" and "the runtime failed"
/// are never the same value in a diagnostic.
pub fn admission_failed(error: RuntimeError) -> DownstreamDisposition {
    match error {
        RuntimeError::QueueOverloaded => DownstreamDisposition::QueueOverload,
        _ => DownstreamDisposition::SupervisorStop,
    }
}
