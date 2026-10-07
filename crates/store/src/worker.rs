//! The owned bounded store worker and its typed async handle.
//!
//! Exactly one thread owns the SQLite connection for the lifetime of a store. Every
//! request crosses one explicitly bounded queue, so store pressure is visible as a
//! typed overload error instead of unbounded memory growth, and blocking database
//! work can never execute on a network owner or downstream Tokio task.
use crate::{
    error::{CommitState, StoreError, StoreErrorKind},
    model::*,
    ops, schema,
};
use i2pr_irc_core::{BufferId, ClientId, HistoryEventId, NetworkId};
use i2pr_irc_wire::IrcTimestamp;
use rusqlite::Connection;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};
use tokio::sync::{mpsc, oneshot};

/// Explicit ingress ceiling. A caller that finds it full gets
/// [`StoreErrorKind::QueueOverloaded`] and must degrade explicitly; it never spins,
/// allocates a secondary unbounded queue, or blocks IRC control traffic.
pub const STORE_QUEUE_CAPACITY: usize = 256;
/// Bounded SQLite busy timeout. Lock contention fails as an explicit error instead of
/// parking a network owner thread indefinitely.
pub const STORE_BUSY_TIMEOUT_MS: u32 = 5_000;

/// One typed store operation. The variant set is closed: a caller cannot submit SQL
/// text or a closure, so network input can never reach the database engine directly.
enum Request {
    LoadNetworks(Reply<Result<Vec<NetworkRecord>, StoreError>>),
    SaveNetwork {
        record: Box<NetworkRecord>,
        reply: Reply<Result<SavedNetwork, StoreError>>,
    },
    RemoveNetwork {
        network: NetworkId,
        reply: Reply<Result<bool, StoreError>>,
    },
    AddDesiredChannel {
        network: NetworkId,
        channel: String,
        reply: Reply<Result<bool, StoreError>>,
    },
    /// The bouncer-owned presentation flag on one desired channel.
    ///
    /// It is its own request rather than a `SaveNetwork` because this is a presentation
    /// decision about membership the bouncer already holds: rewriting the whole record
    /// would race with a concurrent configuration edit and could resurrect stale fields.
    SetDesiredChannelDetached {
        network: NetworkId,
        channel: String,
        detached: bool,
        reply: Reply<Result<bool, StoreError>>,
    },
    RemoveDesiredChannel {
        network: NetworkId,
        channel: String,
        reply: Reply<Result<bool, StoreError>>,
    },
    /// One Network's stored registration actions, in replay order.
    LoadRegistrationActions {
        network: NetworkId,
        reply: Reply<Result<Vec<StoredRegistrationAction>, StoreError>>,
    },
    /// Replaces one Network's stored registration actions wholesale.
    SaveRegistrationActions {
        network: NetworkId,
        actions: Vec<StoredRegistrationAction>,
        reply: Reply<Result<usize, StoreError>>,
    },
    CreateClient {
        login: String,
        reply: Reply<Result<(ClientId, bool), StoreError>>,
    },
    ResolveBuffer {
        network: NetworkId,
        kind: BufferKind,
        target: String,
        reply: Reply<Result<BufferRecord, StoreError>>,
    },
    AppendHistory {
        events: Vec<NewHistoryEvent>,
        reply: Reply<Result<HistoryAppendResult, StoreError>>,
    },
    ResolveMsgId {
        network: NetworkId,
        msgid: String,
        reply: oneshot::Sender<Result<MsgidLookup, StoreError>>,
    },
    NearestEvent {
        buffer: BufferId,
        reference: IrcTimestamp,
        reply: oneshot::Sender<Result<NearestEvent, StoreError>>,
    },
    Search {
        query: Box<SearchQuery>,
        reply: oneshot::Sender<Result<Vec<SearchHit>, StoreError>>,
    },
    QueryHistory {
        query: Box<HistoryQuery>,
        reply: Reply<Result<Vec<HistoryEvent>, StoreError>>,
    },
    /// One bounded window centred on an anchor, for `AROUND`.
    HistoryAround {
        request: Box<HistoryAround>,
        reply: Reply<Result<Vec<HistoryEvent>, StoreError>>,
    },
    GetCursor {
        client: ClientId,
        buffer: BufferId,
        reply: Reply<Result<Option<HistoryEventId>, StoreError>>,
    },
    AdvanceCursor {
        client: ClientId,
        buffer: BufferId,
        to: HistoryEventId,
        reply: Reply<Result<HistoryEventId, StoreError>>,
    },
    GetReadMarker {
        buffer: BufferId,
        reply: Reply<Result<Option<HistoryEventId>, StoreError>>,
    },
    AdvanceReadMarker {
        buffer: BufferId,
        to: HistoryEventId,
        reply: Reply<Result<HistoryEventId, StoreError>>,
    },
    Retain {
        request: Box<RetentionRequest>,
        reply: Reply<Result<RetentionReport, StoreError>>,
    },
    /// Buffers with retained history inside a time window, for `TARGETS`.
    RecentTargets {
        network: NetworkId,
        /// Canonical protocol timestamps, matching what the `server_time` column holds.
        ///
        /// Named for the column, not for a unit: the value is text, and an i64 here would
        /// be compared against text by SQLite type order rather than by time.
        lower_unix_millis: IrcTimestamp,
        upper_unix_millis: IrcTimestamp,
        limit: usize,
        reply: Reply<Result<Vec<RecentTarget>, StoreError>>,
    },
    Health(Reply<Result<StoreHealth, StoreError>>),
    Flush(Reply<Result<(), StoreError>>),
}

/// One-shot bounded response path. Dropping it is a canceled request, which is why
/// mutations report an explicit [`CommitState`] rather than assuming rollback.
type Reply<T> = oneshot::Sender<T>;

struct Shared {
    health: Mutex<StoreHealth>,
    closing: AtomicBool,
    /// Test-only: an artificial per-request delay, used to simulate a slow or
    /// stalling disk so liveness under storage pressure can be qualified.
    ///
    /// This is a `#[doc(hidden)]` fixture affordance, not a production setting. It is
    /// never settable from a production path and defaults to zero.
    stall: Mutex<Option<std::time::Duration>>,
}

impl Shared {
    fn set_health(&self, value: StoreHealth) {
        *self.health.lock().expect("store health lock poisoned") = value;
    }
}

/// Typed async handle to the owned worker. Cloning it shares the one worker and the
/// one bounded queue; it never creates a second database connection.
#[derive(Clone)]
pub struct StoreHandle {
    sender: mpsc::Sender<Request>,
    shared: Arc<Shared>,
}

impl StoreHandle {
    /// Submits one typed request across the bounded ingress queue.
    ///
    /// Returns immediately when the queue is full rather than awaiting capacity, so a
    /// storage hiccup cannot stall the network owner's control path. Dropping the
    /// returned future cancels the request; a mutation may still have committed, so
    /// the caller must re-read durable state instead of assuming it rolled back.
    async fn submit<T>(
        &self,
        build: impl FnOnce(Reply<Result<T, StoreError>>) -> Request,
    ) -> Result<T, StoreError> {
        if self.shared.closing.load(Ordering::Acquire) {
            return Err(StoreError::new(StoreErrorKind::Stopped));
        }
        let (reply, response) = oneshot::channel();
        self.sender
            .try_send(build(reply))
            .map_err(|_| StoreError::new(StoreErrorKind::QueueOverloaded))?;
        match response.await {
            Ok(value) => value,
            // The worker ended without answering. A mutation's commit state is
            // genuinely unknown, so that is exactly what the caller is told.
            Err(_) => Err(StoreError::mutating(
                StoreErrorKind::Stopped,
                CommitState::Unknown,
            )),
        }
    }

    pub async fn load_networks(&self) -> Result<Vec<NetworkRecord>, StoreError> {
        self.submit(Request::LoadNetworks).await
    }
    pub async fn save_network(&self, record: &NetworkRecord) -> Result<SavedNetwork, StoreError> {
        self.submit(|reply| Request::SaveNetwork {
            record: Box::new(record.clone()),
            reply,
        })
        .await
    }
    pub async fn remove_network(&self, network: NetworkId) -> Result<bool, StoreError> {
        self.submit(|reply| Request::RemoveNetwork { network, reply })
            .await
    }
    /// Reads one Network's stored registration actions, in replay order.
    ///
    /// Bounded by the store's own ceiling, so a caller replaying them cannot be handed a
    /// list larger than the runtime is willing to emit.
    pub async fn load_registration_actions(
        &self,
        network: NetworkId,
    ) -> Result<Vec<StoredRegistrationAction>, StoreError> {
        self.submit(|reply| Request::LoadRegistrationActions { network, reply })
            .await
    }

    /// Replaces one Network's stored registration actions wholesale.
    pub async fn save_registration_actions(
        &self,
        network: NetworkId,
        actions: &[StoredRegistrationAction],
    ) -> Result<usize, StoreError> {
        self.submit(|reply| Request::SaveRegistrationActions {
            network,
            actions: actions.to_vec(),
            reply,
        })
        .await
    }

    pub async fn add_desired_channel(
        &self,
        network: NetworkId,
        channel: &str,
    ) -> Result<bool, StoreError> {
        self.submit(|reply| Request::AddDesiredChannel {
            network,
            channel: channel.to_owned(),
            reply,
        })
        .await
    }
    /// Records or clears one desired channel's detached presentation flag.
    ///
    /// Returns false when the channel is not desired on this Network. An ambiguous
    /// commit is reported as [`CommitState::Unknown`], never as success: the caller must
    /// re-read durable state to learn whether the flag landed.
    pub async fn set_desired_channel_detached(
        &self,
        network: NetworkId,
        channel: &str,
        detached: bool,
    ) -> Result<bool, StoreError> {
        self.submit(|reply| Request::SetDesiredChannelDetached {
            network,
            channel: channel.to_owned(),
            detached,
            reply,
        })
        .await
    }
    pub async fn remove_desired_channel(
        &self,
        network: NetworkId,
        channel: &str,
    ) -> Result<bool, StoreError> {
        self.submit(|reply| Request::RemoveDesiredChannel {
            network,
            channel: channel.to_owned(),
            reply,
        })
        .await
    }
    pub async fn create_client(&self, login: &str) -> Result<(ClientId, bool), StoreError> {
        self.submit(|reply| Request::CreateClient {
            login: login.to_owned(),
            reply,
        })
        .await
    }
    pub async fn resolve_buffer(
        &self,
        network: NetworkId,
        kind: BufferKind,
        target: &str,
    ) -> Result<BufferRecord, StoreError> {
        self.submit(|reply| Request::ResolveBuffer {
            network,
            kind,
            target: target.to_owned(),
            reply,
        })
        .await
    }
    pub async fn append_history(
        &self,
        events: &[NewHistoryEvent],
    ) -> Result<HistoryAppendResult, StoreError> {
        self.submit(|reply| Request::AppendHistory {
            events: events.to_vec(),
            reply,
        })
        .await
    }
    /// Resolves a `msgid=` reference within one Network.
    pub async fn resolve_msgid(
        &self,
        network: NetworkId,
        msgid: &str,
    ) -> Result<MsgidLookup, StoreError> {
        self.submit(|reply| Request::ResolveMsgId {
            network,
            msgid: msgid.to_owned(),
            reply,
        })
        .await
    }

    /// Finds the events bracketing one canonical protocol timestamp in one buffer.
    pub async fn nearest_event(
        &self,
        buffer: BufferId,
        reference: IrcTimestamp,
    ) -> Result<NearestEvent, StoreError> {
        self.submit(|reply| Request::NearestEvent {
            buffer,
            reference,
            reply,
        })
        .await
    }

    /// Runs one bounded search over one Network's retained history.
    pub async fn search(&self, query: &SearchQuery) -> Result<Vec<SearchHit>, StoreError> {
        self.submit(|reply| Request::Search {
            query: Box::new(query.clone()),
            reply,
        })
        .await
    }

    pub async fn query_history(
        &self,
        query: &HistoryQuery,
    ) -> Result<Vec<HistoryEvent>, StoreError> {
        self.submit(|reply| Request::QueryHistory {
            query: Box::new(*query),
            reply,
        })
        .await
    }

    /// A bounded window centred on one anchor event.
    pub async fn history_around(
        &self,
        request: &HistoryAround,
    ) -> Result<Vec<HistoryEvent>, StoreError> {
        self.submit(|reply| Request::HistoryAround {
            request: Box::new(*request),
            reply,
        })
        .await
    }
    /// Buffers whose newest retained event falls inside a time window.
    pub async fn recent_targets(
        &self,
        network: NetworkId,
        lower: IrcTimestamp,
        upper: IrcTimestamp,
        limit: usize,
    ) -> Result<Vec<RecentTarget>, StoreError> {
        self.submit(|reply| Request::RecentTargets {
            network,
            lower_unix_millis: lower,
            upper_unix_millis: upper,
            limit,
            reply,
        })
        .await
    }
    pub async fn get_cursor(
        &self,
        client: ClientId,
        buffer: BufferId,
    ) -> Result<Option<HistoryEventId>, StoreError> {
        self.submit(|reply| Request::GetCursor {
            client,
            buffer,
            reply,
        })
        .await
    }
    pub async fn advance_cursor(
        &self,
        client: ClientId,
        buffer: BufferId,
        to: HistoryEventId,
    ) -> Result<HistoryEventId, StoreError> {
        self.submit(|reply| Request::AdvanceCursor {
            client,
            buffer,
            to,
            reply,
        })
        .await
    }
    pub async fn get_read_marker(
        &self,
        buffer: BufferId,
    ) -> Result<Option<HistoryEventId>, StoreError> {
        self.submit(|reply| Request::GetReadMarker { buffer, reply })
            .await
    }
    pub async fn advance_read_marker(
        &self,
        buffer: BufferId,
        to: HistoryEventId,
    ) -> Result<HistoryEventId, StoreError> {
        self.submit(|reply| Request::AdvanceReadMarker { buffer, to, reply })
            .await
    }
    pub async fn retain(&self, request: &RetentionRequest) -> Result<RetentionReport, StoreError> {
        self.submit(|reply| Request::Retain {
            request: Box::new(*request),
            reply,
        })
        .await
    }
    /// Current bounded store health. Never reports a payload, endpoint, or secret.
    pub async fn health(&self) -> Result<StoreHealth, StoreError> {
        self.submit(Request::Health).await
    }
    /// Waits until every request queued before this call has completed.
    ///
    /// Because the queue is FIFO, a barrier submitted now can only be answered after
    /// all earlier work. A test can therefore use this to reach a deterministic
    /// point without sleeping.
    pub async fn flush(&self) -> Result<(), StoreError> {
        self.submit(Request::Flush).await
    }
    /// Installs or clears the test-only per-request stall.
    #[doc(hidden)]
    pub fn set_stall(&self, stall: Option<std::time::Duration>) {
        *self.shared.stall.lock().expect("stall lock poisoned") = stall;
    }

    /// Remaining ingress capacity, for bounded-load assertions.
    pub fn queue_capacity(&self) -> usize {
        self.sender.capacity()
    }
    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }
}

/// Owns the worker thread and the database it opens.
pub struct Store {
    handle: StoreHandle,
    shutdown: Option<std::sync::mpsc::SyncSender<()>>,
    worker: Option<JoinHandle<()>>,
}

impl Store {
    /// Opens (creating if absent) the database and starts the owned worker.
    ///
    /// Startup validates application identity and schema compatibility *before* any
    /// normal request is served. A database this build cannot serve is a startup
    /// failure, not a runtime condition the bouncer tries to work around.
    pub fn open(path: &StorePath) -> Result<Self, StoreError> {
        Self::open_with(path, STORE_BUSY_TIMEOUT_MS)
    }

    pub fn open_with(path: &StorePath, busy_timeout_ms: u32) -> Result<Self, StoreError> {
        Self::open_stalled(path, busy_timeout_ms, None)
    }

    /// Opens a store that delays every request by `stall`.
    ///
    /// Test-only: it exists so integrated qualification can prove that a stalled
    /// store degrades storage without starving IRC control traffic. It is not part of
    /// the production request surface and no production path can set it.
    #[doc(hidden)]
    pub fn open_stalled(
        path: &StorePath,
        busy_timeout_ms: u32,
        stall: Option<std::time::Duration>,
    ) -> Result<Self, StoreError> {
        let mut connection = match path {
            StorePath::Memory => Connection::open_in_memory(),
            StorePath::File(location) => {
                if let Some(parent) = location.parent()
                    && !parent.as_os_str().is_empty()
                    && !parent.exists()
                {
                    return Err(StoreError::new(StoreErrorKind::Open));
                }
                Connection::open(location)
            }
        }
        .map_err(|_| StoreError::new(StoreErrorKind::Open))?;
        schema::open_and_migrate(&connection, busy_timeout_ms)?;
        let shared = Arc::new(Shared {
            health: Mutex::new(StoreHealth::Ready),
            closing: AtomicBool::new(false),
            stall: Mutex::new(stall),
        });
        let (sender, mut receiver) = mpsc::channel::<Request>(STORE_QUEUE_CAPACITY);
        // A dedicated capacity-1 wakeup channel guarantees shutdown can always reach
        // the worker, even while the bounded request queue is full or other handle
        // clones are still alive.
        let (shutdown_tx, shutdown_rx) = std::sync::mpsc::sync_channel(1);
        let worker = {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("i2pr-irc-store".to_owned())
                .spawn(move || {
                    run_worker(&mut connection, &mut receiver, shutdown_rx, &shared);
                })
                .map_err(|_| StoreError::new(StoreErrorKind::Open))?
        };
        Ok(Self {
            handle: StoreHandle { sender, shared },
            shutdown: Some(shutdown_tx),
            worker: Some(worker),
        })
    }

    pub fn handle(&self) -> &StoreHandle {
        &self.handle
    }

    /// Clones the shared typed handle. Every clone shares the one bounded queue and
    /// the one connection.
    pub fn handle_clone(&self) -> StoreHandle {
        self.handle.clone()
    }

    /// Stops the worker after draining queued work, then joins it so no database
    /// call outlives the store.
    pub fn shutdown(mut self) -> Result<(), StoreError> {
        self.close()
    }

    fn close(&mut self) -> Result<(), StoreError> {
        self.handle.shared.closing.store(true, Ordering::Release);
        // New requests are refused from here on. The wakeup tells the worker to
        // finish whatever it is already running, answer everything queued ahead of
        // the stop, and exit, so no committed work is lost and none outlives it.
        let _ = self.shutdown.take().map(|sender| sender.try_send(()));
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| StoreError::new(StoreErrorKind::Stopped))?;
        }
        Ok(())
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

/// Longest the worker parks before rechecking the shutdown wakeup.
///
/// The request queue cannot itself signal shutdown: it stays connected while other
/// `StoreHandle` clones exist, and a parked receive would never observe the stop. The
/// bounded park is therefore what guarantees `Store::close` can always join this
/// thread, at the cost of a small bounded shutdown latency.
const WORKER_PARK: std::time::Duration = std::time::Duration::from_millis(25);

/// The single owner of the connection. It never leaves this loop.
fn run_worker(
    connection: &mut Connection,
    receiver: &mut mpsc::Receiver<Request>,
    shutdown: std::sync::mpsc::Receiver<()>,
    shared: &Shared,
) {
    // Requests are taken with a blocking receive so the owned thread parks without a
    // reactor and no SQLite call ever runs on a Tokio worker.
    let stopping = false;
    loop {
        let request = match receiver.try_recv() {
            Ok(request) => request,
            Err(mpsc::error::TryRecvError::Empty) => {
                if stopping {
                    break;
                }
                match shutdown.recv_timeout(WORKER_PARK) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                }
            }
            Err(mpsc::error::TryRecvError::Disconnected) => break,
        };
        // A stop signal that arrived while the queue was busy is honored before the
        // next receive, so a shutdown can never wait on an idle queue.
        if stopping || shutdown.try_recv().is_ok() {
            // Answer everything already accepted before stopping, so a stop never
            // silently discards work the caller was told was queued.
            while let Ok(queued) = receiver.try_recv() {
                execute(connection, queued);
            }
            break;
        }
        // A test-only stall simulates a slow disk. It is applied after a request has
        // actually been received, so it slows real work rather than parking the
        // worker in a way a shutdown could not interrupt.
        if let Some(stall) = *shared.stall.lock().expect("stall lock poisoned") {
            let _ = shutdown.try_recv();
            std::thread::sleep(stall);
        }
        // A canceled caller, a typed storage error, and an overload rejection are
        // ordinary outcomes: none of them may look like a dead store.
        execute(connection, request);
    }
    shared.set_health(StoreHealth::Stopped);
}

macro_rules! answer {
    ($reply:expr, $value:expr) => {{
        // A canceled caller simply drops its reply. The operation already ran, which
        // is exactly why mutations report their own commit state.
        let _ = $reply.send($value);
    }};
}

/// Runs one request against the owned connection.
fn execute(connection: &mut Connection, request: Request) {
    match request {
        Request::LoadNetworks(reply) => answer!(reply, ops::load_networks(connection)),
        Request::SaveNetwork { record, reply } => {
            answer!(reply, ops::save_network(connection, &record))
        }
        Request::RemoveNetwork { network, reply } => {
            answer!(reply, ops::remove_network(connection, network))
        }
        Request::AddDesiredChannel {
            network,
            channel,
            reply,
        } => answer!(
            reply,
            ops::add_desired_channel(connection, network, &channel)
        ),
        Request::SetDesiredChannelDetached {
            network,
            channel,
            detached,
            reply,
        } => answer!(
            reply,
            ops::set_desired_channel_detached(connection, network, &channel, detached)
        ),
        Request::RemoveDesiredChannel {
            network,
            channel,
            reply,
        } => answer!(
            reply,
            ops::remove_desired_channel(connection, network, &channel)
        ),
        Request::LoadRegistrationActions { network, reply } => {
            answer!(reply, ops::load_registration_actions(connection, network))
        }
        Request::SaveRegistrationActions {
            network,
            actions,
            reply,
        } => answer!(
            reply,
            ops::save_registration_actions(connection, network, &actions)
        ),
        Request::CreateClient { login, reply } => {
            answer!(reply, ops::create_client(connection, &login))
        }
        Request::ResolveBuffer {
            network,
            kind,
            target,
            reply,
        } => answer!(
            reply,
            ops::resolve_buffer(connection, network, kind, &target)
        ),
        Request::AppendHistory { events, reply } => {
            answer!(reply, ops::append_history(connection, &events))
        }
        Request::ResolveMsgId {
            network,
            msgid,
            reply,
        } => {
            answer!(reply, ops::resolve_msgid(connection, network, &msgid))
        }
        Request::NearestEvent {
            buffer,
            reference,
            reply,
        } => {
            answer!(reply, ops::nearest_event(connection, buffer, &reference))
        }
        Request::Search { query, reply } => {
            answer!(reply, ops::search(connection, query.as_ref()))
        }
        Request::QueryHistory { query, reply } => {
            answer!(reply, ops::query_history(connection, &query))
        }
        Request::HistoryAround { request, reply } => {
            answer!(reply, ops::history_around(connection, &request))
        }
        Request::GetCursor {
            client,
            buffer,
            reply,
        } => answer!(reply, ops::get_cursor(connection, client, buffer)),
        Request::AdvanceCursor {
            client,
            buffer,
            to,
            reply,
        } => answer!(reply, ops::advance_cursor(connection, client, buffer, to)),
        Request::GetReadMarker { buffer, reply } => {
            answer!(reply, ops::get_read_marker(connection, buffer))
        }
        Request::AdvanceReadMarker { buffer, to, reply } => {
            answer!(reply, ops::advance_read_marker(connection, buffer, to))
        }
        Request::RecentTargets {
            network,
            lower_unix_millis,
            upper_unix_millis,
            limit,
            reply,
        } => {
            answer!(
                reply,
                ops::recent_targets(
                    connection,
                    network,
                    &lower_unix_millis,
                    &upper_unix_millis,
                    limit
                )
            )
        }
        Request::Retain { request, reply } => answer!(reply, ops::retain(connection, &request)),
        Request::Health(reply) => {
            let _ = reply.send(Ok(StoreHealth::Ready));
        }
        // The queue is FIFO, so answering this proves every earlier request has
        // already been answered. That makes it a deterministic test barrier.
        Request::Flush(reply) => {
            let _ = reply.send(Ok(()));
        }
    }
}
