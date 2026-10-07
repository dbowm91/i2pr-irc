//! Plan 026 — M005-G richer IRCv3 member-state mediation.
//!
//! M005-F proved that a capability can be advertised and negotiated per session. This
//! suite covers the harder half: that what a session is *shown* is decided by what that
//! session negotiated, on a Network whose upstream negotiated once for everybody.
//!
//! The five accepted capabilities share one rule, and each of them breaks it in a
//! different way:
//!
//! - `extended-join` changes a JOIN's parameter list, so relaying it verbatim hands a
//!   client it did not ask for this a frame it cannot parse.
//! - `account-notify`, `away-notify` and `setname` produce message forms that exist only
//!   because a capability was negotiated, so a client that negotiated none must be sent
//!   none of them.
//! - `multi-prefix` widens a prefix run, so it must be widened only for a client that
//!   asked -- in NAMES, in the live projection, and in routed WHO and WHOIS replies.
//!
//! Two properties run underneath all of them. Nothing is fabricated: an account or
//! realname that was never observed is omitted rather than defaulted to the spec's `*`.
//! And a reattaching client sees the same view it would have received live, because two
//! answers to "what does this client see" would be worse than either one of them.

use std::sync::Arc;
use std::time::Duration;

use i2pr_irc_core::{ByteStream, ClientId, I2pEndpoint, NetworkId, SessionId};
use i2pr_irc_runtime::admission::{AdmissionOutcome, DownstreamAdmission, NetworkSelection};
use i2pr_irc_runtime::controller::{ControlSnapshot, RuntimeControlHandle, RuntimeController};
use i2pr_irc_store::{NetworkRecord, Store, StorePath};
use i2pr_irc_testkit::{FakeI2pStreamProvider, FaultScript, ScriptedStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CEILING: Duration = Duration::from_secs(10);

/// Everything the reviewed upstream request set can ask for.
const FULL: &str = "message-tags server-time batch labeled-response echo-message extended-join account-notify away-notify multi-prefix setname";

/// The same Network on a server that offers no member-state capability at all.
const LEAN: &str = "message-tags server-time batch labeled-response";

/// `005` tokens a `setname`-capable server publishes, including the ceiling it owes.
const ISUPPORT_FULL: &str = "PREFIX=(ov)@+ CHANTYPES=# NAMELEN=64";
/// `005` tokens from a server that publishes no realname ceiling of its own.
const ISUPPORT_BARE: &str = "PREFIX=(ov)@+ CHANTYPES=#";
/// The bouncer's own channel membership as an `extended-join` server reports it.
const OWN_JOIN_EXTENDED: &str = ":bot!u@h JOIN {channel} bouncerbot :The Bouncer";
/// The same membership in the plain form, which tells the bouncer nothing about the
/// bouncer's own account or realname.
const OWN_JOIN_PLAIN: &str = ":bot!u@h JOIN {channel}";

fn b32() -> String {
    format!("{}.b32.i2p", "a".repeat(52))
}

fn record(network: u64, channels: &[&str]) -> NetworkRecord {
    NetworkRecord {
        network: NetworkId(network),
        display_name: format!("net-{network}"),
        endpoint: I2pEndpoint::parse(&b32()).expect("a test destination"),
        nick: "bot".to_owned(),
        username: "user".to_owned(),
        realname: "bouncer".to_owned(),
        sasl: None,
        desired_channels: channels
            .iter()
            .enumerate()
            .map(|(index, target)| i2pr_irc_store::DesiredChannelRecord {
                target: (*target).to_owned(),
                position: index,
                detached: false,
            })
            .collect(),
        auto_away: false,
        keep_nick: false,
    }
}

#[derive(Clone)]
struct Provider(Arc<FakeI2pStreamProvider>);

impl Provider {
    fn new() -> Self {
        let provider = Arc::new(FakeI2pStreamProvider::default());
        for _ in 0..16 {
            provider
                .queue_outcome(Ok(FaultScript::default()))
                .expect("provider queue has room");
        }
        Self(provider)
    }

    async fn plain_peer(&self) -> ScriptedStream {
        let peer = self.0.take_peer().await;
        self.0.take_controller().await;
        peer
    }
}

#[async_trait::async_trait]
impl i2pr_irc_core::I2pStreamProvider for Provider {
    async fn connect(
        &self,
        endpoint: &I2pEndpoint,
    ) -> Result<Box<dyn ByteStream>, i2pr_irc_core::ProviderError> {
        self.0.connect(endpoint).await
    }
}

struct Runtime {
    control: RuntimeControlHandle,
    provider: Provider,
    task: tokio::task::JoinHandle<Result<(), i2pr_irc_runtime::RuntimeError>>,
    upstreams: Vec<ScriptedStream>,
    _store: Store,
}

impl Runtime {
    async fn start() -> Self {
        let store = Store::open(&StorePath::Memory).expect("store opens");
        let handle = store.handle_clone();
        let provider = Provider::new();
        let (mut controller, control) =
            RuntimeController::with_durable(provider.clone(), handle.clone(), Arc::new(handle));
        let task = tokio::spawn(async move { controller.serve().await });
        wait_for(&control, |_| true).await;
        Self {
            control,
            provider,
            task,
            upstreams: Vec::new(),
            _store: store,
        }
    }

    /// Brings a Network online with `cap_ls` offered upstream.
    ///
    /// `isupport` and `own_join` are the two things a server controls that decide what the
    /// bouncer can say about itself: the tokens it publishes, and whether it reports the
    /// bouncer's own membership in the extended form. Both are parameters so a test can
    /// choose what the bouncer legitimately knows.
    async fn bring_online(
        &mut self,
        network: u64,
        channels: &[&str],
        cap_ls: &str,
        isupport: &str,
        own_join: &str,
    ) -> usize {
        self.bring_online_with_names(network, channels, cap_ls, isupport, own_join, "")
            .await
    }

    /// As [`Runtime::bring_online`], plus `extra_names` added to each channel's `NAMES`.
    ///
    /// Membership a test depends on has to exist before any client attaches. A `353`
    /// written afterwards is still in flight when the first client registers, and an
    /// unsolicited `353` fans out verbatim -- correct behaviour, and one that would leave
    /// the test measuring the fanout rather than the bouncer's own rendering of the same
    /// facts.
    async fn bring_online_with_names(
        &mut self,
        network: u64,
        channels: &[&str],
        cap_ls: &str,
        isupport: &str,
        own_join: &str,
        extra_names: &str,
    ) -> usize {
        self.control
            .create(record(network, channels))
            .await
            .unwrap_or_else(|error| panic!("create {network}: {error:?}"));
        let mut upstream = self.provider.plain_peer().await;
        read_until(&mut upstream, b"USER user 0 * :bouncer\r\n").await;
        let ack = cap_ls
            .split_whitespace()
            .map(|name| name.to_owned())
            .collect::<Vec<_>>()
            .join(" ");
        upstream
            .write_all(format!(":srv CAP * LS :{ack}\r\n:srv 001 bot :welcome\r\n").as_bytes())
            .await
            .expect("upstream accepts registration");
        // Answering with exactly what the bouncer asked for -- rather than the whole
        // advertisement -- keeps each test honest about which capabilities were actually
        // granted, since an ACK naming a capability that was never requested takes a
        // different branch of registration than the real one.
        loop {
            let line = read_line(&mut upstream).await;
            if line.contains("CAP END") {
                break;
            }
            if let Some(request) = line.strip_prefix("CAP REQ :") {
                let requested = request.trim_end_matches("\r\n");
                upstream
                    .write_all(format!(":srv CAP * ACK :{requested}\r\n").as_bytes())
                    .await
                    .expect("upstream acks");
            }
        }
        // The bouncer's own channel membership is the extended form, which is what gives
        // a reattaching client something to observe about itself. `NAMELEN` is published
        // because `setname` obliges the server to say how long a realname may be.
        for channel in channels {
            let mut lines = vec![
                format!(":srv 005 bot {isupport}"),
                own_join.replace("{channel}", channel),
            ];
            // An extra `353` only when there are names to put in it: a memberless `353` is
            // not a line any server sends, and it would claim a NAMES list was seen.
            if !extra_names.is_empty() {
                lines.push(format!(":srv 353 bot = {channel} :{extra_names}"));
            }
            lines.push(format!(":srv 366 bot {channel} :End of /NAMES list."));
            let frame = lines
                .into_iter()
                .map(|line| format!("{line}\r\n"))
                .collect::<String>();
            upstream
                .write_all(frame.as_bytes())
                .await
                .expect("upstream joins");
        }
        let peer = self.upstreams.len();
        self.upstreams.push(upstream);
        // Liveness alone is not enough. The advertisement is published just after upstream
        // negotiation finishes, and a client that registers in between would be answered
        // against the fallback list -- which is a real ordering this test must not race.
        wait_for(&self.control, |snapshot| {
            snapshot
                .networks
                .iter()
                .any(|entry| entry.network == NetworkId(network) && entry.live)
        })
        .await;
        let deadline = tokio::time::Instant::now() + CEILING;
        loop {
            if !self
                .control
                .advertisement(NetworkId(network))
                .await
                .unwrap_or_default()
                .is_empty()
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the owner never published an advertisement for {network}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // Neither liveness nor the advertisement proves the startup frames were *applied*.
        // They were only written, and the owner applies them later, in order. A probe client
        // settles that ordering: its barrier is a frame it is owed anyway, so seeing it
        // proves every earlier line -- including any `353` -- was applied first.
        //
        // Without this the suite passes in isolation and fails under parallel load, which is
        // the worst possible failure mode for a harness.
        let mut probe = register(&*self, NetworkId(network), SessionId(u64::MAX), "").await;
        sync(self, peer, &mut probe).await;
        drop(probe);
        peer
    }

    async fn upstream(&mut self, peer: usize) -> &mut ScriptedStream {
        &mut self.upstreams[peer]
    }

    async fn send_upstream(&mut self, peer: usize, frame: &str) {
        self.upstreams[peer]
            .write_all(frame.as_bytes())
            .await
            .expect("upstream accepts traffic");
    }

    async fn stop(self) {
        self.control.request_stop();
        self.task
            .await
            .expect("controller task joins")
            .expect("controller reports success");
        self._store.shutdown().expect("store shuts down");
    }
}

async fn wait_for<F>(control: &RuntimeControlHandle, ready: F)
where
    F: Fn(&ControlSnapshot) -> bool,
{
    let mut status = control.subscribe_status();
    let deadline = tokio::time::Instant::now() + CEILING;
    loop {
        if ready(&status.borrow()) {
            return;
        }
        let _ = tokio::time::timeout_at(deadline, status.changed())
            .await
            .expect("the controller publishes a snapshot");
    }
}

struct Client {
    end: ScriptedStream,
    _script: i2pr_irc_testkit::FaultController,
    /// Retained so a client is only dropped when the test ends it. Dropping the task
    /// would cancel admission rather than ending it, which is not what a test that just
    /// wants a live client means.
    _outcome: tokio::task::JoinHandle<AdmissionOutcome>,
    seen: String,
}

impl Client {
    async fn until(&mut self, needle: &str) {
        self.await_new(0, needle).await
    }

    async fn await_new(&mut self, mark: usize, needle: &str) {
        let deadline = tokio::time::Instant::now() + CEILING;
        while !self.seen[mark..].contains(needle) {
            let mut chunk = [0u8; 1024];
            let count = tokio::time::timeout_at(deadline, self.end.read(&mut chunk))
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "timed out waiting for {needle:?}; saw {:?}",
                        &self.seen[mark..]
                    )
                })
                .expect("the client stream does not fail");
            assert!(
                count > 0,
                "client closed waiting for {needle:?}; saw {:?}",
                &self.seen[mark..]
            );
            self.seen
                .push_str(&String::from_utf8_lossy(&chunk[..count]));
        }
    }

    /// Asserts a frame is *absent* from a window, after first giving it a chance to
    /// arrive.
    ///
    /// Absence is the interesting half of every mediation test, and absence cannot be
    /// asserted by reading once: the frame may simply not have been delivered yet. The
    /// settle window is what makes "never arrived" a claim about the runtime rather than
    /// about the scheduler.
    async fn assert_absent(&mut self, mark: usize, needle: &str, context: &str) {
        self.settle().await;
        assert!(
            !self.seen[mark..].contains(needle),
            "{context}: {needle:?} must not reach this session; saw {:?}",
            &self.seen[mark..]
        );
    }

    async fn settle(&mut self) {
        let mut chunk = [0u8; 1024];
        loop {
            match tokio::time::timeout(Duration::from_millis(250), self.end.read(&mut chunk)).await
            {
                Ok(Ok(count)) if count > 0 => {
                    self.seen
                        .push_str(&String::from_utf8_lossy(&chunk[..count]));
                }
                _ => break,
            }
        }
    }

    async fn send(&mut self, frame: &str) {
        self.end
            .write_all(frame.as_bytes())
            .await
            .expect("the client writes");
    }

    fn mark(&self) -> usize {
        self.seen.len()
    }

    fn since(&self, mark: usize) -> String {
        self.seen[mark..].to_owned()
    }
}

fn admit(runtime: &Runtime, network: NetworkId, session: SessionId) -> Client {
    let (end, runtime_end, script) = ScriptedStream::pair(FaultScript::default());
    let control = runtime.control.clone();
    let outcome = tokio::spawn(async move {
        let stream: Box<dyn ByteStream> = Box::new(runtime_end);
        DownstreamAdmission::new(
            Some(NetworkSelection {
                network,
                expected_nick: "bot".to_owned(),
            }),
            control,
            session,
            ClientId(7),
        )
        .run(stream)
        .await
    });
    Client {
        end,
        _script: script,
        _outcome: outcome,
        seen: String::new(),
    }
}

async fn register(
    runtime: &Runtime,
    network: NetworkId,
    session: SessionId,
    capabilities: &str,
) -> Client {
    let mut client = admit(runtime, network, session);
    // Requested *during* registration, which is the only moment a negotiated surface can
    // apply to the projection: the projection is sent once, at attach.
    let request = if capabilities.is_empty() {
        "NICK bot\r\nUSER user 0 * :client\r\nCAP END\r\n".to_owned()
    } else {
        format!("CAP REQ :{capabilities}\r\nNICK bot\r\nUSER user 0 * :client\r\nCAP END\r\n")
    };
    client.send(&request).await;
    client.until("001 bot").await;
    if !capabilities.is_empty() {
        assert!(
            !client.seen.contains("NAK"),
            "an advertised capability must be acknowledgeable during registration: {:?}",
            client.seen
        );
    }
    client.settle().await;
    client
}

async fn read_line(stream: &mut ScriptedStream) -> String {
    let mut all = Vec::new();
    let mut buf = [0u8; 1];
    loop {
        let count = tokio::time::timeout(CEILING, stream.read(&mut buf))
            .await
            .expect("upstream produces a line within the ceiling")
            .expect("the upstream stream does not fail");
        assert!(count > 0, "upstream stream ended mid-line");
        all.push(buf[0]);
        if all.ends_with(b"\r\n") {
            return String::from_utf8_lossy(&all).into_owned();
        }
    }
}

async fn read_until(stream: &mut ScriptedStream, needle: &[u8]) -> String {
    let mut all = Vec::new();
    let mut buf = [0; 512];
    let read = async {
        while !all.windows(needle.len()).any(|window| window == needle) {
            let count = stream.read(&mut buf).await.unwrap();
            assert!(count > 0, "upstream stream ended");
            all.extend_from_slice(&buf[..count]);
        }
        while let Ok(Ok(count)) = tokio::time::timeout(Duration::ZERO, stream.read(&mut buf)).await
        {
            if count == 0 {
                break;
            }
            all.extend_from_slice(&buf[..count]);
        }
    };
    tokio::time::timeout(CEILING, read)
        .await
        .unwrap_or_else(|_| panic!("upstream never sent {}", String::from_utf8_lossy(needle)));
    String::from_utf8_lossy(&all).into_owned()
}

/// The `005` tokens a session was offered.
async fn isupport(client: &mut Client) -> String {
    let mark = client.mark();
    client.send("CAP LS\r\n").await;
    client.await_new(mark, "CAP bot LS").await;
    client.since(mark)
}

/// Sends `frames` upstream and waits until a session has observed everything before them.
///
/// Frames written upstream are only *queued*; the owner applies them later, and in order.
/// Asserting against a projection straight after writing upstream races that queue, and a
/// test that passes when it wins is not a test. The marker is a frame this client is owed
/// anyway, so seeing it proves every earlier line was applied first.
async fn sync(runtime: &mut Runtime, peer: usize, client: &mut Client) {
    let mark = client.mark();
    runtime
        .send_upstream(peer, ":sync!u@h PRIVMSG #room :barrier\r\n")
        .await;
    client.await_new(mark, "barrier").await;
}

/// Answers one upstream query with `replies`, echoing the request's route label.
///
/// The bouncer requested `labeled-response` upstream, so a reply is only *routed* -- and
/// therefore only degraded -- when it carries the label the server was given. A reply
/// without it is an uncorrelated frame and fans out verbatim, which is correct behaviour
/// and would make these tests measure nothing.
///
/// The tag goes in front of the prefix, because that is the only position a parser reads it
/// from. A reply whose label sits after the prefix is not a tagged reply at all: it does not
/// parse, and it is dropped rather than routed.
async fn answer_query(runtime: &mut Runtime, peer: usize, command: &str, replies: &str) {
    let seen = read_until(runtime.upstream(peer).await, command.as_bytes()).await;
    let request = seen
        .lines()
        .find(|line| line.contains(command))
        .unwrap_or_else(|| panic!("{command} never reached the upstream: {seen:?}"));
    let label = request
        .strip_prefix('@')
        .and_then(|tags| tags.split_once('=').map(|(_, rest)| rest))
        .and_then(|rest| rest.split(' ').next())
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| panic!("{command} went upstream without a route label: {request:?}"))
        .to_owned();
    let tag = format!("@label={label}");
    let sent = replies
        .lines()
        // Each element already carries the CRLF the template spelled out, so the carriage
        // return is stripped before one is appended back. Leaving it would emit `\r\r\n`
        // and every frame past the first would be unparseable.
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| !line.trim().is_empty())
        .map(|line| format!("{tag} {line}\r\n"))
        .collect::<String>();
    runtime.send_upstream(peer, &sent).await;
}

// ----------------------------------------------------------------- tests

#[tokio::test]
async fn member_capabilities_are_advertised_only_when_upstream_supplied_them() {
    let mut runtime = Runtime::start().await;
    runtime
        .bring_online(1, &["#room"], FULL, ISUPPORT_FULL, OWN_JOIN_EXTENDED)
        .await;

    let mut client = register(&runtime, NetworkId(1), SessionId(1), "").await;
    let listing = isupport(&mut client).await;
    for served in [
        "extended-join",
        "account-notify",
        "away-notify",
        "multi-prefix",
        "setname",
    ] {
        assert!(
            listing.contains(served),
            "upstream negotiated {served}, so the bouncer can mediate it: {listing:?}"
        );
    }
    runtime.stop().await;

    // The same Network on a server that offers none of them. Advertising a capability
    // whose upstream never supplied it would promise every client a richer view than any
    // of them could ever receive.
    let mut runtime = Runtime::start().await;
    runtime
        .bring_online(1, &["#room"], LEAN, ISUPPORT_FULL, OWN_JOIN_EXTENDED)
        .await;
    let mut client = register(&runtime, NetworkId(1), SessionId(1), "").await;
    let listing = isupport(&mut client).await;
    for withheld in [
        "extended-join",
        "account-notify",
        "away-notify",
        "multi-prefix",
        "setname",
    ] {
        assert!(
            !listing.contains(withheld),
            "upstream supplied nothing for {withheld}, so it must not be advertised: {listing:?}"
        );
    }
    runtime.stop().await;
}

#[tokio::test]
async fn deferred_member_capabilities_are_never_advertised() {
    let mut runtime = Runtime::start().await;
    runtime
        .bring_online(1, &["#room"], FULL, ISUPPORT_FULL, OWN_JOIN_EXTENDED)
        .await;
    let mut client = register(&runtime, NetworkId(1), SessionId(1), "").await;
    let listing = isupport(&mut client).await;
    for deferred in [
        "account-tag",
        "chghost",
        "invite-notify",
        "extended-monitor",
    ] {
        assert!(
            !listing.contains(deferred),
            "{deferred} is deferred, so advertising it would be a promise this build keeps: {listing:?}"
        );
    }
    runtime.stop().await;
}

#[tokio::test]
async fn an_extended_join_is_reduced_for_a_client_that_did_not_negotiate_it() {
    let mut runtime = Runtime::start().await;
    let peer = runtime
        .bring_online(1, &["#room"], FULL, ISUPPORT_FULL, OWN_JOIN_EXTENDED)
        .await;

    let mut legacy = register(&runtime, NetworkId(1), SessionId(1), "").await;
    let mut rich = register(&runtime, NetworkId(1), SessionId(2), "extended-join").await;
    legacy.settle().await;
    rich.settle().await;

    runtime
        .send_upstream(peer, ":Alice!u@h JOIN #room aliceacct :Alice Example\r\n")
        .await;
    rich.await_new(0, "Alice Example").await;
    let legacy_mark = legacy.mark();
    legacy.await_new(legacy_mark, "JOIN #room").await;

    assert!(
        rich.since(0)
            .contains("JOIN #room aliceacct :Alice Example"),
        "the negotiated session must receive the extended form: {:?}",
        rich.since(0)
    );
    assert!(
        legacy.since(legacy_mark).contains("JOIN #room\r\n"),
        "the session that never negotiated it must receive the plain form: {:?}",
        legacy.since(legacy_mark)
    );
    assert!(
        !legacy.since(legacy_mark).contains("aliceacct"),
        "the account must not leak into a session that cannot parse it: {:?}",
        legacy.since(legacy_mark)
    );
    runtime.stop().await;
}

#[tokio::test]
async fn an_unobserved_profile_projects_a_plain_join_rather_than_a_guessed_logout() {
    // The bouncer's own membership reported in the extended form, so both of the fields
    // an extended JOIN carries were genuinely observed.
    let mut runtime = Runtime::start().await;
    runtime
        .bring_online(1, &["#room"], FULL, ISUPPORT_FULL, OWN_JOIN_EXTENDED)
        .await;
    let mut client = register(&runtime, NetworkId(1), SessionId(1), "extended-join").await;
    client.until("005").await;
    assert!(
        client.seen.contains("JOIN #room bouncerbot :The Bouncer"),
        "an observed profile must reach a session that negotiated the form: {:?}",
        client.seen
    );
    runtime.stop().await;

    // The same Network where the server reported that membership in the plain form. The
    // bouncer then knows it is in `#room` and knows nothing else -- and `*` is the spec's
    // statement that a *server* said "not logged in". Rendering it from an observation
    // that was never made would assert something the bouncer does not know.
    let mut runtime = Runtime::start().await;
    runtime
        .bring_online(1, &["#room"], FULL, ISUPPORT_FULL, OWN_JOIN_PLAIN)
        .await;
    let mut client = register(&runtime, NetworkId(1), SessionId(1), "extended-join").await;
    client.until("005").await;
    assert!(
        client.seen.contains("JOIN #room\r\n"),
        "an unobserved profile must yield the plain form every client can read: {:?}",
        client.seen
    );
    assert!(
        !client.seen.contains("JOIN #room * :"),
        "an unobserved account must never be rendered as a logged-out marker: {:?}",
        client.seen
    );
    runtime.stop().await;
}

#[tokio::test]
async fn account_changes_reach_only_the_sessions_that_negotiated_account_notify() {
    let mut runtime = Runtime::start().await;
    let peer = runtime
        .bring_online(1, &["#room"], FULL, ISUPPORT_FULL, OWN_JOIN_EXTENDED)
        .await;

    let mut legacy = register(&runtime, NetworkId(1), SessionId(1), "").await;
    let mut watching = register(&runtime, NetworkId(1), SessionId(2), "account-notify").await;
    legacy.settle().await;
    watching.settle().await;

    let legacy_mark = legacy.mark();
    let watching_mark = watching.mark();
    runtime
        .send_upstream(peer, ":Alice!u@h ACCOUNT aliceacct :Logged in\r\n")
        .await;
    watching.await_new(watching_mark, "ACCOUNT aliceacct").await;
    legacy
        .assert_absent(legacy_mark, "ACCOUNT", "a session without account-notify")
        .await;
    assert!(
        !legacy.since(legacy_mark).contains("aliceacct"),
        "the account must not reach a session that cannot interpret it: {:?}",
        legacy.since(legacy_mark)
    );
    runtime.stop().await;
}

#[tokio::test]
async fn away_changes_reach_only_the_sessions_that_negotiated_away_notify() {
    let mut runtime = Runtime::start().await;
    let peer = runtime
        .bring_online(1, &["#room"], FULL, ISUPPORT_FULL, OWN_JOIN_EXTENDED)
        .await;

    let mut legacy = register(&runtime, NetworkId(1), SessionId(1), "").await;
    let mut watching = register(&runtime, NetworkId(1), SessionId(2), "away-notify").await;
    legacy.settle().await;
    watching.settle().await;

    let legacy_mark = legacy.mark();
    let watching_mark = watching.mark();
    runtime
        .send_upstream(peer, ":Alice!u@h AWAY :back shortly\r\n")
        .await;
    watching
        .await_new(watching_mark, "AWAY :back shortly")
        .await;
    legacy
        .assert_absent(legacy_mark, "AWAY", "a session without away-notify")
        .await;
    runtime.stop().await;
}

#[tokio::test]
async fn setname_changes_reach_only_negotiated_sessions_and_the_command_needs_negotiation() {
    let mut runtime = Runtime::start().await;
    let peer = runtime
        .bring_online(1, &["#room"], FULL, ISUPPORT_FULL, OWN_JOIN_EXTENDED)
        .await;

    let mut legacy = register(&runtime, NetworkId(1), SessionId(1), "").await;
    let mut watching = register(&runtime, NetworkId(1), SessionId(2), "setname").await;
    legacy.settle().await;
    watching.settle().await;

    // The server-to-client form is gated on the receiving session.
    let legacy_mark = legacy.mark();
    let watching_mark = watching.mark();
    runtime
        .send_upstream(peer, ":Alice!u@h SETNAME :Alice Example\r\n")
        .await;
    watching
        .await_new(watching_mark, "SETNAME :Alice Example")
        .await;
    legacy
        .assert_absent(legacy_mark, "SETNAME", "a session without setname")
        .await;

    // And the client-to-server form is gated on the sending session. A `SETNAME` from a
    // client that never negotiated it cannot be honoured anywhere upstream, because
    // upstream negotiation is what decides whether the server accepts one at all.
    let legacy_mark = legacy.mark();
    legacy.send("SETNAME :Local Attempt\r\n").await;
    legacy.await_new(legacy_mark, "421").await;
    assert!(
        legacy.since(legacy_mark).contains("Unsupported command"),
        "a refused SETNAME must say so: {:?}",
        legacy.since(legacy_mark)
    );
    assert!(
        !legacy.since(legacy_mark).contains("Local Attempt"),
        "a refused SETNAME must never reach upstream: {:?}",
        legacy.since(legacy_mark)
    );

    // The negotiated session's SETNAME does travel upstream, which is what makes the
    // capability worth advertising in the first place.
    let watching_mark = watching.mark();
    watching.send("SETNAME :Local Name\r\n").await;
    let _ = read_until(runtime.upstream(peer).await, b"SETNAME :Local Name").await;
    assert!(
        !watching.since(watching_mark).contains("421"),
        "a negotiated SETNAME must not be refused: {:?}",
        watching.since(watching_mark)
    );
    runtime.stop().await;
}

#[tokio::test]
async fn the_realname_ceiling_is_published_once_and_only_where_it_is_owed() {
    // A server that publishes its own `NAMELEN`, which the bouncer relays verbatim.
    let mut runtime = Runtime::start().await;
    runtime
        .bring_online(1, &["#room"], FULL, ISUPPORT_FULL, OWN_JOIN_EXTENDED)
        .await;
    let mut client = register(&runtime, NetworkId(1), SessionId(1), "setname").await;
    client.until("005").await;
    assert_eq!(
        client.seen.matches("NAMELEN=").count(),
        1,
        "the relayed upstream token must not be duplicated by one of the bouncer's own: {:?}",
        client.seen
    );
    assert!(
        client.seen.contains("NAMELEN=64"),
        "the value the server published is the value that applies: {:?}",
        client.seen
    );
    runtime.stop().await;

    // A server that publishes none. `setname` obliges *the bouncer* to publish one to a
    // session that negotiated it, so the obligation does not disappear with the upstream
    // token -- it moves to the bouncer.
    let mut runtime = Runtime::start().await;
    runtime
        .bring_online(1, &["#room"], FULL, ISUPPORT_BARE, OWN_JOIN_EXTENDED)
        .await;
    let mut client = register(&runtime, NetworkId(1), SessionId(1), "setname").await;
    client.until("005").await;
    assert_eq!(
        client.seen.matches("NAMELEN=").count(),
        1,
        "the bouncer owes a `setname` session exactly one ceiling: {:?}",
        client.seen
    );
    runtime.stop().await;
}

#[tokio::test]
async fn membership_is_rendered_at_each_sessions_negotiated_prefix_width() {
    let mut runtime = Runtime::start().await;
    // A complete prefix run, as a `multi-prefix` server sends it, established during
    // startup so that every session is projected from it. Written after startup it would
    // still be in flight when the first client registers, and an unsolicited `353` fans out
    // verbatim -- correct behaviour, and one that leaves this test measuring the fanout
    // rather than the bouncer's own rendering of the same facts.
    runtime
        .bring_online_with_names(
            1,
            &["#room"],
            FULL,
            ISUPPORT_FULL,
            OWN_JOIN_EXTENDED,
            "@+Alice",
        )
        .await;

    let mut legacy = register(&runtime, NetworkId(1), SessionId(1), "").await;
    let mut wide = register(&runtime, NetworkId(1), SessionId(2), "multi-prefix").await;
    legacy.until("005").await;
    wide.until("005").await;

    assert!(
        legacy.seen.contains("@Alice"),
        "a legacy session must see the single highest symbol: {:?}",
        legacy.seen
    );
    assert!(
        wide.seen.contains("@+Alice"),
        "a multi-prefix session must see the complete run: {:?}",
        wide.seen
    );
    assert!(
        !legacy.seen.contains("@+Alice"),
        "a complete run must never be handed to a session that cannot parse it: {:?}",
        legacy.seen
    );
    runtime.stop().await;
}

#[tokio::test]
async fn who_replies_are_reduced_for_a_session_without_multi_prefix() {
    let mut runtime = Runtime::start().await;
    let peer = runtime
        .bring_online(1, &["#room"], FULL, ISUPPORT_FULL, OWN_JOIN_EXTENDED)
        .await;

    // Distinct queries, for the same reason as the WHOIS test below: this suite is about
    // what each session may read, not about who a reply belongs to.
    let mut legacy = register(&runtime, NetworkId(1), SessionId(1), "").await;
    let mut wide = register(&runtime, NetworkId(1), SessionId(2), "multi-prefix").await;
    legacy.settle().await;
    wide.settle().await;

    let legacy_mark = legacy.mark();
    legacy.send("WHO #alpha\r\n").await;
    answer_query(
        &mut runtime,
        peer,
        "WHO #alpha",
        ":srv 352 bot #alpha u h srv Alice H@+ :0 Alice Example\r\n\
         :srv 315 bot #alpha :End of /WHO list.\r\n",
    )
    .await;
    legacy.await_new(legacy_mark, "315").await;
    let seen = legacy.since(legacy_mark);
    assert!(
        seen.contains("352 bot #alpha u h srv Alice H@ "),
        "the membership run must be reduced to its highest symbol: {seen:?}"
    );
    assert!(!seen.contains("@+"), "a full run must be reduced: {seen:?}");

    let wide_mark = wide.mark();
    wide.send("WHO #beta\r\n").await;
    answer_query(
        &mut runtime,
        peer,
        "WHO #beta",
        ":srv 352 bot #beta u h srv Bob H@+ :0 Bob Example\r\n\
         :srv 315 bot #beta :End of /WHO list.\r\n",
    )
    .await;
    wide.await_new(wide_mark, "315").await;
    let seen = wide.since(wide_mark);
    assert!(
        seen.contains("352 bot #beta u h srv Bob H@+"),
        "a multi-prefix session must keep the complete run: {seen:?}"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn whois_channel_lists_are_reduced_for_a_session_without_multi_prefix() {
    let mut runtime = Runtime::start().await;
    let peer = runtime
        .bring_online(1, &["#room"], FULL, ISUPPORT_FULL, OWN_JOIN_EXTENDED)
        .await;

    // The two sessions ask about *different* targets. A reply is routed to the client
    // that asked, so two identical queries on one Network would let one reply answer the
    // other's route and this test would measure routing rather than mediation.
    let mut legacy = register(&runtime, NetworkId(1), SessionId(1), "").await;
    let mut wide = register(&runtime, NetworkId(1), SessionId(2), "multi-prefix").await;
    legacy.settle().await;
    wide.settle().await;

    let legacy_mark = legacy.mark();
    legacy.send("WHOIS Alice\r\n").await;
    answer_query(
        &mut runtime,
        peer,
        "WHOIS Alice",
        ":srv 311 bot Alice u h * :Alice Example\r\n\
         :srv 319 bot Alice :@+#alpha +#beta\r\n\
         :srv 318 bot Alice :End of /WHOIS list.\r\n",
    )
    .await;
    legacy.await_new(legacy_mark, "318").await;
    let seen = legacy.since(legacy_mark);
    assert!(
        seen.contains("@#alpha +#beta"),
        "each entry must be reduced to its highest symbol: {seen:?}"
    );
    assert!(
        !seen.contains("@+#alpha"),
        "a full run must be reduced: {seen:?}"
    );

    let wide_mark = wide.mark();
    wide.send("WHOIS Bob\r\n").await;
    answer_query(
        &mut runtime,
        peer,
        "WHOIS Bob",
        ":srv 311 bot Bob u h * :Bob Example\r\n\
         :srv 319 bot Bob :@+#alpha +#beta\r\n\
         :srv 318 bot Bob :End of /WHOIS list.\r\n",
    )
    .await;
    wide.await_new(wide_mark, "318").await;
    let seen = wide.since(wide_mark);
    assert!(
        seen.contains("@+#alpha +#beta"),
        "a multi-prefix session must keep the complete run: {seen:?}"
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_mode_delta_widens_a_run_without_completing_one_that_was_never_observed() {
    let mut runtime = Runtime::start().await;
    let peer = runtime
        .bring_online(1, &["#room"], FULL, ISUPPORT_FULL, OWN_JOIN_EXTENDED)
        .await;

    // Alice joined after the channel's NAMES list, so no complete run was ever observed
    // for her. A `MODE` delta then adds a voice she did have.
    runtime
        .send_upstream(peer, ":Alice!u@h JOIN #room\r\n")
        .await;
    runtime
        .send_upstream(peer, ":Op!u@h MODE #room +v Alice\r\n")
        .await;
    let mut wide = register(&runtime, NetworkId(1), SessionId(1), "multi-prefix").await;
    wide.until("005").await;
    sync(&mut runtime, peer, &mut wide).await;
    let names = names_of(&wide.seen, "#room");
    assert!(
        names.contains("+Alice"),
        "the observed delta must be retained: {names:?}"
    );
    assert!(
        !names.contains("@%+Alice"),
        "an incomplete run must never be presented as complete: {names:?}"
    );
    runtime.stop().await;
}

/// The membership entries one `353` carried for `channel`.
fn names_of(seen: &str, channel: &str) -> String {
    let needle = format!(" 353 bot = {channel} :");
    let Some(start) = seen.find(&needle) else {
        return String::new();
    };
    let rest = &seen[start + needle.len()..];
    let end = rest.find("\r\n").unwrap_or(rest.len());
    rest[..end].to_owned()
}

#[tokio::test]
async fn an_over_long_value_is_relayed_but_never_held() {
    let mut runtime = Runtime::start().await;
    let peer = runtime
        .bring_online(1, &["#room"], FULL, ISUPPORT_FULL, OWN_JOIN_EXTENDED)
        .await;

    let mut client = register(
        &runtime,
        NetworkId(1),
        SessionId(1),
        "setname account-notify",
    )
    .await;

    // `NAMELEN=64` was published, so these exceed what this server accepts and what the
    // bouncer will retain.
    let long = "x".repeat(200);
    let setname = format!(":Alice!u@h SETNAME :{long}\r\n");
    let account = format!(":Alice!u@h ACCOUNT {long} :Logged in\r\n");

    // Relay is not retention. A negotiated client is owed the frame the server sent, and
    // dropping it would hide an event that really happened.
    let mark = client.mark();
    runtime.send_upstream(peer, &setname).await;
    client.await_new(mark, "SETNAME").await;
    assert!(
        client.since(mark).contains(&long),
        "a negotiated session must still receive the frame: {:?}",
        client.since(mark)
    );
    let mark = client.mark();
    runtime.send_upstream(peer, &account).await;
    client.await_new(mark, "ACCOUNT").await;

    // What must not happen is the value entering observed state, where it would be held
    // for the life of the generation and re-rendered on every subsequent projection. The
    // observable consequence is that membership still projects cleanly afterwards.
    runtime
        .send_upstream(peer, ":srv 366 bot #room :End of /NAMES list.\r\n")
        .await;
    let mut fresh = register(
        &runtime,
        NetworkId(1),
        SessionId(2),
        "extended-join setname account-notify away-notify",
    )
    .await;
    fresh.until("005").await;
    assert!(
        fresh.seen.contains("353 bot = #room"),
        "the retained membership must still project: {:?}",
        fresh.seen
    );
    assert!(
        fresh.seen.matches(&long).count() <= 1,
        "an unretainable value must not reappear in a projection: {:?}",
        fresh.seen
    );
    runtime.stop().await;
}

#[tokio::test]
async fn a_reattached_session_is_shown_the_membership_it_would_have_received_live() {
    let mut runtime = Runtime::start().await;
    let peer = runtime
        .bring_online_with_names(
            1,
            &["#room"],
            FULL,
            ISUPPORT_FULL,
            OWN_JOIN_EXTENDED,
            "@Alice",
        )
        .await;

    let mut live = register(&runtime, NetworkId(1), SessionId(1), "extended-join").await;
    runtime
        .send_upstream(peer, ":Carol!u@h JOIN #room carolacct :Carol Example\r\n")
        .await;
    live.await_new(0, "Carol Example").await;

    let mut fresh = register(&runtime, NetworkId(1), SessionId(2), "extended-join").await;
    fresh.until("005").await;

    // Membership is the part that can disagree, and it does not. Not in the strict sense --
    // Carol joined after the live session was projected, so its `353` cannot list her --
    // but in the sense that matters: nothing the live session knows is missing from the
    // reattached one, and a member who arrived while the fresh client was absent is
    // reconstructed rather than forgotten.
    let fresh_names = names_of(&fresh.seen, "#room");
    let live_names = names_of(&live.seen, "#room");
    for member in live_names.split_whitespace() {
        assert!(
            fresh_names
                .split_whitespace()
                .any(|entry| entry.ends_with(member)),
            "a reattached session must keep every member the live one had: {live_names:?} vs {fresh_names:?}"
        );
    }
    assert!(
        fresh_names.contains("Carol"),
        "a member who arrived while the client was away must be reconstructed: {fresh_names:?}"
    );

    // Per-member account and realname are the part that *cannot* agree, and the boundary is
    // deliberate rather than accidental. No projection frame carries them: `353` has no
    // field for an account or a realname, and inventing a frame to hold them would be a
    // bouncer-only extension no client understands. So they are available as they happen,
    // and a client that wants the current value asks the server, which is exactly what
    // `extended-join` plus `WHOIS` is for.
    assert!(
        !fresh.seen.contains("Carol Example"),
        "no projection frame carries a member realname, so none may be invented: {:?}",
        fresh.seen
    );
    assert!(
        live.since(0).contains("Carol Example"),
        "the live session is still told what it negotiated for: {:?}",
        live.since(0)
    );
    runtime.stop().await;
}
