//! Process-wide FIFO pacing for safe registration recovery commands.
//!
//! The gate deliberately only schedules idempotent setup work (registration actions and
//! desired JOIN restoration). PING/PONG and ordinary live control traffic never wait on
//! it. Tokio's mutex is FIFO, so one Network restoring many channels cannot starve another
//! Network that has just registered.
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;

/// At most ten recovery commands per second across the process.
pub const RECOVERY_COMMAND_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone)]
pub struct CommandPacer(Arc<Mutex<tokio::time::Instant>>);

impl Default for CommandPacer {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(tokio::time::Instant::now())))
    }
}

impl CommandPacer {
    /// Waits for this command's process-wide slot. FIFO lock acquisition provides
    /// fairness between active Networks; the permit is released before the caller writes.
    pub async fn wait(&self) {
        let mut next = self.0.lock().await;
        let now = tokio::time::Instant::now();
        if *next > now {
            tokio::time::sleep_until(*next).await;
        }
        *next = tokio::time::Instant::now() + RECOVERY_COMMAND_INTERVAL;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn recovery_slots_are_shared_by_clones_and_spaced() {
        let pacer = CommandPacer::default();
        pacer.wait().await;
        let clone = pacer.clone();
        let second_clone = pacer.clone();
        let (tx, mut rx) = tokio::sync::mpsc::channel(2);
        let first = tokio::spawn({
            let tx = tx.clone();
            async move {
                clone.wait().await;
                tx.send(1).await.expect("first sender");
            }
        });
        tokio::task::yield_now().await;
        let second = tokio::spawn(async move {
            second_clone.wait().await;
            tx.send(2).await.expect("second sender");
        });
        tokio::task::yield_now().await;
        assert!(rx.try_recv().is_err());
        tokio::time::advance(RECOVERY_COMMAND_INTERVAL).await;
        assert_eq!(rx.recv().await, Some(1), "FIFO waiter gets the first slot");
        tokio::time::advance(RECOVERY_COMMAND_INTERVAL).await;
        assert_eq!(rx.recv().await, Some(2), "next Network gets the next slot");
        first.await.expect("first pacing task completes");
        second.await.expect("second pacing task completes");
    }
}
