use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::Notify;

#[derive(Clone)]
/// An interrupt flag with notification capabilities
///
/// Whenever the flag is set, a signal is sent to interrupt waiters for action.
pub(crate) struct Interrupt {
    flag: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl Default for Interrupt {
    fn default() -> Self {
        Self::new()
    }
}

impl Interrupt {
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
        }
    }

    /// Sets the interrupt flag to true and notifies any waiters
    pub fn set(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// Checks if the interrupt has been set
    pub fn is_set(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Waits asynchronously until the interrupt is set
    pub async fn wait(&self) {
        if self.is_set() {
            return;
        }

        // Otherwise wait to be notified
        self.notify.notified().await;
    }
}
