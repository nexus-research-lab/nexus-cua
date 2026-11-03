//! Content-free activity timestamps and dispatch lifetimes shared by a session.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use crate::{DriverError, DriverErrorKind};

pub(crate) const INPUT_QUIET_PERIOD: Duration = Duration::from_secs(2);
pub(crate) const INPUT_WAIT_LIMIT: Duration = Duration::from_secs(5);

#[derive(Default)]
pub(crate) struct InputActivity {
    pub(crate) last_input: Option<Instant>,
    pub(crate) input_held: bool,
    pub(crate) active_dispatches: usize,
}

impl InputActivity {
    pub(crate) fn remaining_quiet(&self, quiet_period: Duration) -> Duration {
        if self.input_held {
            return quiet_period;
        }
        self.last_input.map_or(Duration::ZERO, |last| {
            quiet_period.saturating_sub(last.elapsed())
        })
    }

    pub(crate) fn check_observation(
        &self,
        observed_at: Option<Instant>,
    ) -> Result<(), DriverError> {
        if observed_at.is_some_and(|observed| self.last_input.is_some_and(|last| last >= observed))
        {
            return Err(DriverError::new(
                DriverErrorKind::StaleObservation,
                "user input changed the desktop after this observation; observe again within the existing session",
            ).retryable("observe_window").mutation_not_dispatched());
        }
        Ok(())
    }
}

// The last native actor clone releases this lease, including when a caller's
// future is cancelled before its actor job finishes. No timer clears a dispatch.
pub(crate) struct DispatchLease {
    pub(crate) activity: Arc<Mutex<InputActivity>>,
    pub(crate) dispatched: AtomicBool,
}

impl Drop for DispatchLease {
    fn drop(&mut self) {
        if self.dispatched.load(Ordering::Acquire) {
            let mut activity = self
                .activity
                .lock()
                .expect("activity state contains no native objects");
            activity.active_dispatches -= 1;
        }
    }
}
