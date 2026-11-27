//! One result computed off the UI thread.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use eframe::egui::Context;

/// Work running on its own thread. Dropping the task drops its result, so
/// whoever owns the task decides where a late answer may land.
pub struct Task<T> {
    receiver: Receiver<T>,
}

/// The worker ended without an answer: it panicked.
#[derive(Debug)]
pub struct Stopped;

impl<T: Send + 'static> Task<T> {
    pub fn spawn(ctx: &Context, work: impl FnOnce() -> T + Send + 'static) -> Self {
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        thread::spawn(move || {
            // Sent before the repaint, so the frame it wakes finds the result.
            let _ = sender.send(work());
            ctx.request_repaint();
        });
        Self { receiver }
    }
}

impl<T> Task<T> {
    /// `None` while the work runs. Drop the task once this returns `Some`.
    pub fn poll(&self) -> Option<Result<T, Stopped>> {
        match self.receiver.try_recv() {
            Ok(value) => Some(Ok(value)),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(Stopped)),
        }
    }

    /// A task whose worker answers through the sender: sending finishes it,
    /// dropping the sender stops it.
    #[cfg(test)]
    pub fn pending() -> (Self, mpsc::Sender<T>) {
        let (sender, receiver) = mpsc::channel();
        (Self { receiver }, sender)
    }

    #[cfg(test)]
    pub fn ready(value: T) -> Self {
        let (task, sender) = Self::pending();
        let _ = sender.send(value);
        task
    }
}
