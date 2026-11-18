// SPDX-License-Identifier: GPL-3.0-or-later

use std::fmt;

/// Lifecycle of a download (SOP 8). Only the transitions listed in
/// [`TaskState::can_become`] are allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskState {
    /// Accepted, nothing done yet.
    Created,
    /// Asking the server what the resource is.
    Inspecting,
    /// Waiting for a transfer slot.
    Queued,
    /// Bytes are moving.
    Downloading,
    /// Stopping workers before a pause.
    Pausing,
    /// Stopped by the user; can continue later.
    Paused,
    /// Waiting before another attempt.
    Retrying,
    /// Checking length and checksum.
    Verifying,
    /// Committing data and moving the file into place.
    Finalizing,
    /// The file is at its destination and passed every check.
    Completed,
    /// Stopped by an error.
    Failed,
    /// Stopped by the user for good.
    Cancelled,
}

impl TaskState {
    /// True when the state machine allows `self` to be followed by `next`.
    pub fn can_become(self, next: TaskState) -> bool {
        use TaskState::*;
        matches!(
            (self, next),
            (Created, Inspecting | Failed | Cancelled)
                | (Inspecting, Queued | Failed | Cancelled)
                | (Queued, Downloading | Paused | Failed | Cancelled)
                | (Downloading, Pausing | Retrying | Verifying | Failed | Cancelled)
                | (Pausing, Paused | Failed | Cancelled)
                | (Paused, Queued | Cancelled)
                | (Retrying, Downloading | Pausing | Failed | Cancelled)
                | (Verifying, Finalizing | Failed | Cancelled)
                // Finalizing cannot be cancelled: the move either happens or fails.
                | (Finalizing, Completed | Failed)
                // A failed task may be queued again by the user.
                | (Failed, Queued)
        )
    }

    /// True for states that end a run.
    pub fn is_terminal(self) -> bool {
        matches!(self, TaskState::Completed | TaskState::Failed | TaskState::Cancelled)
    }
}

impl fmt::Display for TaskState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

#[cfg(test)]
mod tests {
    use super::TaskState::{self, *};

    const ALL: [TaskState; 12] =
        [Created, Inspecting, Queued, Downloading, Pausing, Paused, Retrying, Verifying, Finalizing, Completed, Failed, Cancelled];

    #[test]
    fn the_normal_path_is_allowed() {
        let path = [Created, Inspecting, Queued, Downloading, Verifying, Finalizing, Completed];
        for pair in path.windows(2) {
            assert!(pair[0].can_become(pair[1]), "{} -> {}", pair[0], pair[1]);
        }
    }

    #[test]
    fn pause_goes_through_pausing() {
        assert!(!Downloading.can_become(Paused));
        assert!(Downloading.can_become(Pausing));
        assert!(Pausing.can_become(Paused));
        assert!(Paused.can_become(Queued));
    }

    #[test]
    fn completed_and_cancelled_are_final() {
        for next in ALL {
            assert!(!Completed.can_become(next), "Completed -> {next}");
            assert!(!Cancelled.can_become(next), "Cancelled -> {next}");
        }
    }

    #[test]
    fn nothing_skips_verification() {
        for from in ALL {
            if from != Finalizing {
                assert!(!from.can_become(Completed), "{from} -> Completed");
            }
        }
        assert!(!Downloading.can_become(Finalizing));
    }

    #[test]
    fn no_state_follows_itself() {
        for s in ALL {
            assert!(!s.can_become(s), "{s} -> {s}");
        }
    }
}
