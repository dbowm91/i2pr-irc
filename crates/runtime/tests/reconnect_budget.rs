//! Plan 016 qualification: the process-wide reconnect budget.
//!
//! The claims here are about *concurrency and timing*, so the tests measure actual
//! concurrency rather than inferring it from backoff values: they hold permits open,
//! count how many exist at once, and release them deliberately.
//!
//! The production policy is deliberately small and slow. Every test injects its own
//! budget so nothing here depends on wall-clock production values.
use i2pr_irc_core::NetworkId;
use i2pr_irc_runtime::reconnect::{
    AcquireRefused, MAX_RECONNECT_WAITERS, ReconnectBudget, ReconnectScheduler,
    SchedulerDiagnostics, jitter_entropy,
};
use std::{sync::Arc, time::Duration};
use tokio::{sync::oneshot, task::JoinHandle, time::timeout};

/// Acquires every permit concurrently, so a ceiling is exercised as a *concurrency*
/// limit rather than a sequential one.
async fn futures_join<F: std::future::Future>(a: F, b: F, c: F) -> [F::Output; 3] {
    let (a, b, c) = tokio::join!(a, b, c);
    [a, b, c]
}

/// A budget fast enough for tests but still exercising both gates.
fn test_budget(max_in_flight: usize, max_burst: u32, max_waiters: usize) -> ReconnectBudget {
    ReconnectBudget {
        max_in_flight,
        max_burst,
        // Long enough that a test can never accidentally outrun it by accident, so the
        // rate gate is only opened by an explicit refill helper.
        token_interval: Duration::from_secs(3600),
        max_waiters,
        seed: 0x0123_4567_89ab_cdef,
    }
}

fn scheduler(max_in_flight: usize, max_burst: u32, max_waiters: usize) -> ReconnectScheduler {
    ReconnectScheduler::new(test_budget(max_in_flight, max_burst, max_waiters))
        .expect("budget is bounded")
}

/// Collects the permit back from a spawned acquire so the test controls its lifetime.
async fn spawn_acquire(
    scheduler: ReconnectScheduler,
    network: NetworkId,
) -> (oneshot::Receiver<bool>, JoinHandle<()>) {
    let (tx, rx) = oneshot::channel();
    let handle = tokio::spawn(async move {
        // Holding the permit is the whole point of the task; it drops when this ends.
        let permit = scheduler.acquire(network).await;
        let _ = tx.send(permit.is_ok());
        drop(permit);
    });
    (rx, handle)
}

// ------------------------------------------------------------------- ceilings

#[tokio::test]
async fn the_in_flight_ceiling_is_never_exceeded() {
    let scheduler = scheduler(3, 8, 64);
    // Held for the whole test: dropping them would be what frees a slot.
    let held = futures_join(
        scheduler.acquire(NetworkId(1)),
        scheduler.acquire(NetworkId(2)),
        scheduler.acquire(NetworkId(3)),
    )
    .await;
    let mut permits = Vec::new();
    for permit in held {
        permits.push(permit.expect("within the in-flight ceiling"));
    }
    assert_eq!(scheduler.diagnostics().in_flight, 3);
    assert_eq!(scheduler.diagnostics().peak_in_flight, 3);

    // The fourth must wait, not proceed.
    let (mut tx, waiter) = spawn_acquire(scheduler.clone(), NetworkId(99)).await;
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(
        tx.try_recv().is_err(),
        "a fourth concurrent connect must not be admitted"
    );
    assert_eq!(scheduler.diagnostics().in_flight, 3);
    assert_eq!(scheduler.diagnostics().pending_waiters, 1);
    waiter.abort();
    let _ = waiter.await;
    drop(permits);
}

#[tokio::test]
async fn a_permit_is_released_by_dropping_it() {
    // A wide burst: this test is about in-flight capacity, and with a one-hour token
    // interval a single token would never regenerate.
    let scheduler = scheduler(1, 8, 8);
    let held = scheduler.acquire(NetworkId(1)).await.expect("first");
    assert_eq!(scheduler.diagnostics().in_flight, 1);

    let (mut tx, _waiter) = spawn_acquire(scheduler.clone(), NetworkId(2)).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(tx.try_recv().is_err(), "no capacity is free yet");

    drop(held);
    let received = timeout(Duration::from_secs(2), tx)
        .await
        .expect("a released permit must admit the waiter")
        .expect("sender alive");
    assert!(received, "a released permit must admit the waiter");
    assert_eq!(scheduler.diagnostics().peak_in_flight, 1);
}

// --------------------------------------------------------------------- rate

#[tokio::test]
async fn the_burst_ceiling_bounds_attempt_starts() {
    // Four in flight is allowed, but only two may *start*: the token bucket is the
    // independent second gate, and a test must be able to tell the two apart.
    let scheduler = scheduler(4, 2, 64);
    let first = scheduler.acquire(NetworkId(1)).await.expect("start 1");
    let second = scheduler.acquire(NetworkId(2)).await.expect("start 2");
    assert_eq!(scheduler.diagnostics().in_flight, 2);

    let (mut tx, _waiter) = spawn_acquire(scheduler.clone(), NetworkId(3)).await;
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(
        tx.try_recv().is_err(),
        "the burst ceiling must stop a third start even though in-flight capacity is free"
    );
    drop(first);
    drop(second);
    let _ = timeout(Duration::from_millis(200), tx).await;
}

// ------------------------------------------------------------------ fairness

#[tokio::test]
async fn a_repeatedly_failing_network_does_not_starve_another() {
    // A wide burst so this test measures queue order only; the rate gate is exercised
    // separately, and with a one-hour token interval it would otherwise never refill.
    let scheduler = scheduler(1, 8, 64);
    // One Network holds the only slot; a second queues behind it.
    let held = scheduler.acquire(NetworkId(1)).await.expect("held");
    let (tx, _waiter) = spawn_acquire(scheduler.clone(), NetworkId(2)).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(scheduler.diagnostics().pending_waiters, 1);

    // The releasing Network immediately asks again. FIFO means the Network that was
    // *already waiting* is served, not the one that just freed the slot -- otherwise a
    // permanently failing Network could overtake a healthy one forever.
    drop(held);
    // The queued waiter is served first, so it already holds the slot; the retrier then
    // waits behind it rather than overtaking.
    assert!(
        timeout(Duration::from_secs(2), tx)
            .await
            .expect("the queued Network is served on release")
            .expect("alive"),
        "the Network that was already waiting must be served on release"
    );
    // Only after the queued Network releases does the retrier get in, so a permanently
    // failing Network can never overtake one that was already waiting.
    let retrier = scheduler.acquire(NetworkId(1)).await;
    assert!(retrier.is_ok(), "the retrier is then served in turn");
}

#[tokio::test]
async fn waiters_are_served_in_arrival_order() {
    let scheduler = scheduler(1, 8, 64);
    let held = scheduler.acquire(NetworkId(1)).await.expect("held");
    let mut receivers = Vec::new();
    let mut waiters = Vec::new();
    for network in 2..=5u64 {
        let (tx, rx) = oneshot::channel();
        let scheduler = scheduler.clone();
        let handle = tokio::spawn(async move {
            let _permit = scheduler.acquire(NetworkId(network)).await;
            let _ = tx.send(network);
        });
        receivers.push(rx);
        waiters.push(handle);
        // Give each waiter time to enqueue so arrival order is deterministic.
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(scheduler.diagnostics().pending_waiters, 4);

    drop(held);
    let mut order = Vec::new();
    for rx in receivers {
        order.push(
            timeout(Duration::from_secs(3), rx)
                .await
                .expect("served")
                .expect("alive"),
        );
    }
    assert_eq!(
        order,
        vec![2, 3, 4, 5],
        "FIFO admission is what makes the queue starvation-free"
    );
    for handle in waiters {
        let _ = handle.await;
    }
}

#[tokio::test]
async fn a_duplicate_acquire_coalesces_instead_of_adding_a_waiter() {
    let scheduler = scheduler(1, 8, 64);
    let held = scheduler.acquire(NetworkId(7)).await.expect("held");

    // The same Network asks twice concurrently.
    let (tx_a, _a) = spawn_acquire(scheduler.clone(), NetworkId(7)).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    let (tx_b, _b) = spawn_acquire(scheduler.clone(), NetworkId(7)).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        scheduler.diagnostics().pending_waiters,
        1,
        "one Network may hold at most one queue entry"
    );

    drop(held);
    let _ = timeout(Duration::from_secs(2), tx_a).await;
    let _ = timeout(Duration::from_secs(2), tx_b).await;
}

#[tokio::test]
async fn the_waiter_ceiling_is_refused_rather_than_grown() {
    let max = 4usize;
    let scheduler = scheduler(1, 8, max);
    let held = scheduler.acquire(NetworkId(1)).await.expect("held");

    let mut refused = Vec::new();
    for network in 0..max {
        let (tx, _waiter) = spawn_acquire(scheduler.clone(), NetworkId(network as u64 + 100)).await;
        refused.push(tx);
    }
    for receiver in refused.iter_mut() {
        assert!(
            receiver.try_recv().is_err(),
            "a queued waiter must wait its turn"
        );
    }
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(scheduler.diagnostics().pending_waiters, max);

    // The next one is refused outright.
    assert_eq!(
        scheduler.acquire(NetworkId(999)).await.err(),
        Some(AcquireRefused::WaitersFull)
    );
    drop(held);
    for receiver in refused {
        let _ = timeout(Duration::from_secs(2), receiver).await;
    }
}

#[tokio::test]
async fn the_waiter_ceiling_matches_the_supervised_network_ceiling() {
    // The plan ties the two together: one waiter per Network, bounded by the catalog.
    assert_eq!(
        MAX_RECONNECT_WAITERS, 64,
        "the waiter ceiling must equal the supervised-Network ceiling"
    );
}

// -------------------------------------------------------------- cancellation

#[tokio::test]
async fn cancelling_while_waiting_removes_the_waiter() {
    let scheduler = scheduler(1, 8, 64);
    let _held = scheduler.acquire(NetworkId(1)).await.expect("held");
    let (mut tx, waiter) = spawn_acquire(scheduler.clone(), NetworkId(2)).await;
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert_eq!(scheduler.diagnostics().pending_waiters, 1);

    // Cancellation is dropping the acquire future, which is what a stopped Network does.
    waiter.abort();
    let _ = waiter.await;
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(
        scheduler.diagnostics().pending_waiters,
        0,
        "a cancelled Network must not leave a permanent claim on the queue"
    );
    assert!(tx.try_recv().is_err());
}

#[tokio::test]
async fn a_cancelled_connect_attempt_releases_its_permit() {
    let scheduler = scheduler(1, 8, 8);
    let permit = scheduler.acquire(NetworkId(1)).await.expect("held");
    assert_eq!(scheduler.diagnostics().in_flight, 1);
    // The owner drops the permit whether the attempt succeeded, failed, or was stopped.
    drop(permit);
    assert_eq!(scheduler.diagnostics().in_flight, 0);
    assert!(
        timeout(Duration::from_secs(1), scheduler.acquire(NetworkId(2)))
            .await
            .expect("capacity was released")
            .is_ok()
    );
}

// ------------------------------------------------------ terminal classification

#[tokio::test]
async fn a_terminal_network_consumes_no_further_permits() {
    let scheduler = scheduler(4, 4, 64);
    scheduler.mark_terminal(NetworkId(5));
    assert_eq!(
        scheduler.acquire(NetworkId(5)).await.err(),
        Some(AcquireRefused::Terminal),
        "a terminally failed Network must not keep spending the shared budget"
    );
    let diagnostics = scheduler.diagnostics();
    assert_eq!(diagnostics.admitted, 0);
    assert!(diagnostics.terminal_suppressed > 0);
    // A healthy Network is unaffected.
    assert!(scheduler.acquire(NetworkId(6)).await.is_ok());
}

#[tokio::test]
async fn reconciliation_rearms_a_terminal_network() {
    let scheduler = scheduler(4, 4, 64);
    let terminal = NetworkId(5);
    scheduler.mark_terminal(terminal);
    assert!(scheduler.is_terminal(terminal));
    assert!(scheduler.acquire(terminal).await.is_err());

    scheduler.rearm(terminal);
    assert!(!scheduler.is_terminal(terminal));
    assert!(
        scheduler.acquire(terminal).await.is_ok(),
        "a reconciled Network must be able to try again"
    );
}

// --------------------------------------------------------------------- jitter

#[test]
fn equal_attempt_counts_get_independent_jitter() {
    // Two Networks on the same generation must not receive the same jitter solely
    // because their generations match: that is the correlated herd this defeats.
    let seed = 0x0123_4567_89ab_cdef;
    let first = jitter_entropy(NetworkId(1), 3, seed);
    let second = jitter_entropy(NetworkId(2), 3, seed);
    assert_ne!(
        first, second,
        "two Networks must not share a jitter sequence"
    );
}

#[test]
fn jitter_is_reproducible_for_a_fixed_seed() {
    let seed = 0x0123_4567_89ab_cdef;
    let runs: Vec<u64> = (0..4)
        .map(|_| jitter_entropy(NetworkId(9), 2, seed))
        .collect();
    assert!(
        runs.windows(2).all(|pair| pair[0] == pair[1]),
        "a deterministic seed must give a reproducible sequence"
    );
    assert_ne!(
        jitter_entropy(NetworkId(9), 2, seed ^ 1),
        runs[0],
        "a different seed must decorrelate"
    );
}

#[test]
fn jitter_varies_with_the_attempt() {
    let seed = 0x0123_4567_89ab_cdef;
    assert_ne!(
        jitter_entropy(NetworkId(1), 1, seed),
        jitter_entropy(NetworkId(1), 2, seed)
    );
}

// ------------------------------------------------------------------- policy

#[test]
fn an_unlimited_policy_is_refused_rather_than_treated_as_a_default() {
    let mut budget = test_budget(4, 4, 64);
    budget.max_in_flight = 0;
    assert!(ReconnectScheduler::new(budget).is_err());

    let mut budget = test_budget(4, 4, 64);
    budget.max_burst = 0;
    assert!(ReconnectScheduler::new(budget).is_err());

    let mut budget = test_budget(4, 4, 64);
    budget.token_interval = Duration::ZERO;
    assert!(ReconnectScheduler::new(budget).is_err());

    let mut budget = test_budget(4, 4, 64);
    budget.max_waiters = 0;
    assert!(ReconnectScheduler::new(budget).is_err());
}

#[test]
fn the_production_policy_is_bounded_and_documented() {
    let budget = ReconnectBudget::default();
    assert!(budget.validate().is_ok());
    assert!(budget.max_in_flight > 0);
    assert!(budget.max_burst > 0);
    assert!(!budget.token_interval.is_zero());
    assert!(budget.max_waiters > 0);
}

#[tokio::test]
async fn an_idle_scheduler_does_not_spin() {
    // Nothing waiting and nothing in flight must mean no wakeups: the scheduler only
    // notifies when capacity is actually released.
    let scheduler = scheduler(4, 4, 64);
    let before = scheduler.diagnostics();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let after = scheduler.diagnostics();
    assert_eq!(before, after, "an idle scheduler must not churn");
}

#[tokio::test]
async fn diagnostics_carry_counts_only() {
    // Every field is a count, so nothing sensitive can appear in them by construction.
    let scheduler = scheduler(2, 2, 8);
    let _permit = scheduler.acquire(NetworkId(1)).await.expect("held");
    let rendered = format!("{:?}", scheduler.diagnostics());
    for forbidden in ["password", "sasl", ".i2p", "hunter2", "bot"] {
        assert!(!rendered.contains(forbidden), "{forbidden} in {rendered}");
    }
    let _: SchedulerDiagnostics = scheduler.diagnostics();
    let _ = Arc::new(scheduler);
}

// ------------------------------------------------------------- token gate wakeup

/// A waiter blocked *only* on the token gate is admitted when its token comes due.
///
/// The two admission gates release differently, and only one of them signals anything.
/// In-flight capacity frees when a permit is dropped, which calls `notify_waiters`. The
/// token gate frees on a clock, and no code path notifies for a clock -- the bucket is
/// refilled lazily, when a waiter re-checks it. A waiter that parked only on the
/// notification therefore slept until some *unrelated* release happened to touch the queue.
///
/// On a cold start that is every Network past the burst, and nothing else: the burst is
/// spent, no further permit is ever dropped, and no timer exists. The rate limiter built
/// to stop a startup herd from becoming a simultaneous connect would instead leave those
/// Networks permanently unconnected. In-flight capacity is deliberately left free here, so
/// the token is provably the only thing standing in the way.
///
/// Virtual time, so "ten seconds" is instant and the assertion is about the wakeup rather
/// than about the machine being fast.
#[tokio::test(start_paused = true)]
async fn a_waiter_blocked_only_on_the_token_gate_is_admitted_when_its_token_is_due() {
    const TOKEN_INTERVAL: Duration = Duration::from_secs(10);

    let scheduler = ReconnectScheduler::new(ReconnectBudget {
        // Generous in-flight capacity, single-token burst: the token is the only gate.
        max_in_flight: 4,
        max_burst: 1,
        token_interval: TOKEN_INTERVAL,
        max_waiters: MAX_RECONNECT_WAITERS,
        seed: 0x0123_4567_89ab_cdef,
    })
    .expect("bounded budget");

    let held = scheduler
        .acquire(NetworkId(1))
        .await
        .expect("the first attempt spends the only token");

    let waiter = tokio::spawn({
        let scheduler = scheduler.clone();
        async move { scheduler.acquire(NetworkId(2)).await }
    });

    let permit = timeout(TOKEN_INTERVAL * 3, waiter)
        .await
        .expect(
            "a rate-limited waiter must wake on its own token deadline, not wait for an \
             unrelated event to release a permit",
        )
        .expect("the waiter task joins")
        .expect("the waiter is admitted once its token is due");
    drop(permit);
    drop(held);

    let diagnostics = scheduler.diagnostics();
    assert_eq!(
        diagnostics.admitted, 2,
        "both attempts were admitted, in order"
    );
}

/// The token gate still holds the start rate: two waiters cannot both outrun one interval.
///
/// The mirror of the test above. Waking on the clock must not become waking immediately --
/// if the fix degenerated into "ignore the rate gate", this fails while the other passes.
#[tokio::test(start_paused = true)]
async fn the_token_gate_holds_the_start_rate_even_though_waiters_wake_on_time() {
    const TOKEN_INTERVAL: Duration = Duration::from_secs(10);

    let scheduler = ReconnectScheduler::new(ReconnectBudget {
        max_in_flight: 4,
        max_burst: 1,
        token_interval: TOKEN_INTERVAL,
        max_waiters: MAX_RECONNECT_WAITERS,
        seed: 0x0123_4567_89ab_cdef,
    })
    .expect("bounded budget");

    let held = scheduler
        .acquire(NetworkId(1))
        .await
        .expect("the first attempt spends the only token");

    let second = tokio::spawn({
        let scheduler = scheduler.clone();
        async move { scheduler.acquire(NetworkId(2)).await }
    });
    let third = tokio::spawn({
        let scheduler = scheduler.clone();
        async move { scheduler.acquire(NetworkId(3)).await }
    });

    // Strict FIFO means the second waiter cannot take the token the third waiter's deadline
    // would otherwise produce, so advancing past one interval admits exactly one of them.
    let permit = timeout(TOKEN_INTERVAL * 2, second)
        .await
        .expect("the head of the queue is admitted after one interval")
        .expect("task joins")
        .expect("admitted");
    drop(permit);

    assert!(
        !third.is_finished(),
        "the third waiter must still be waiting: one interval buys one token, not two"
    );
    drop(held);
    let _ = third.await;
}
