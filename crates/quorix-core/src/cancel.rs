// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Arc;

use tokio::sync::watch;

const RUNNING: u8 = 0;
const PAUSE: u8 = 1;
const CANCEL: u8 = 2;

/// What the caller asked a running download to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopKind {
    /// Stop and keep the progress so the download can continue later.
    Pause,
    /// Stop and delete the partial data.
    Cancel,
}

/// Asks a running download to stop.
///
/// Clones share one signal. Both commands are idempotent: repeating one has the
/// same effect as sending it once. Cancel is stronger than pause: a paused
/// request can still be turned into a cancel, never the other way round.
#[derive(Debug, Clone)]
pub struct CancelToken {
    tx: Arc<watch::Sender<u8>>,
    rx: watch::Receiver<u8>,
}

impl CancelToken {
    /// Creates a token with no request pending.
    pub fn new() -> Self {
        let (tx, rx) = watch::channel(RUNNING);
        Self { tx: Arc::new(tx), rx }
    }

    /// Stop and keep the partial file and its checkpoint.
    pub fn pause(&self) {
        self.raise(PAUSE);
    }

    /// Stop and delete the partial file.
    pub fn cancel(&self) {
        self.raise(CANCEL);
    }

    fn raise(&self, level: u8) {
        self.tx.send_if_modified(|current| {
            let changed = level > *current;
            if changed {
                *current = level;
            }
            changed
        });
    }

    /// True once cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        *self.rx.borrow() == CANCEL
    }

    /// True once a pause was requested and no cancel followed it.
    pub fn is_paused(&self) -> bool {
        *self.rx.borrow() == PAUSE
    }

    /// Completes when a stop is requested. Never completes otherwise.
    pub(crate) async fn stopped(&self) -> StopKind {
        let mut rx = self.rx.clone();
        // The sender lives in `self`, so `wait_for` can only end with a request present.
        let level = rx.wait_for(|level| *level != RUNNING).await.map(|level| *level).unwrap_or(CANCEL);
        if level == PAUSE {
            StopKind::Pause
        } else {
            StopKind::Cancel
        }
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancel_is_seen_by_clones_and_is_idempotent() {
        let token = CancelToken::new();
        let clone = token.clone();
        assert!(!clone.is_cancelled());
        token.cancel();
        token.cancel();
        assert!(clone.is_cancelled());
        assert_eq!(clone.stopped().await, StopKind::Cancel); // must return immediately
    }

    #[tokio::test]
    async fn cancel_overrides_pause_but_pause_never_overrides_cancel() {
        let token = CancelToken::new();
        token.pause();
        token.pause();
        assert!(token.is_paused());
        assert_eq!(token.stopped().await, StopKind::Pause);

        token.cancel();
        token.pause();
        assert!(token.is_cancelled() && !token.is_paused());
        assert_eq!(token.stopped().await, StopKind::Cancel);
    }
}
