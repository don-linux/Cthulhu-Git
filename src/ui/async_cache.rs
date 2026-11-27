//! Results of slow git reads, keyed by what was asked for: a commit's
//! details for the card and the sidebar, one file's patch for the changes
//! modal.
//!
//! One read runs at a time, on a worker thread. A key asked for while it
//! runs waits, and only the newest such key is kept. Finished reads,
//! failures included, stay until [`MAX_ENTRIES`] other keys push them out.

use std::borrow::Borrow;
use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use cthulhu_git::git::{Git, GitError};
use eframe::egui::Context;

/// Keys kept. Every frame asks again for what it shows, so what is on screen
/// stays recent and the keys nobody asked for in the longest time leave.
pub(super) const MAX_ENTRIES: usize = 256;

/// A key whose value comes from one git command.
pub trait GitRead<V>: Clone + Eq + Hash + Send + 'static {
    /// Shown when the worker ends without an answer.
    const LOST: &'static str;

    fn read(&self, git: &Git, root: &Path) -> Result<V, GitError>;
}

/// What [`AsyncCache::cached`] found for one key.
pub enum Cached<'a, V> {
    Pending,
    Ready(&'a V),
    Failed(&'a str),
}

enum State<V> {
    Loading,
    Ready(V),
    Failed(String),
}

pub struct AsyncCache<K, V> {
    entries: HashMap<K, State<V>>,
    /// Least recently asked for first.
    order: VecDeque<K>,
    inflight: Option<(K, Receiver<Result<V, String>>)>,
    /// Asked for while another read was running. Only the newest is kept.
    wanted: Option<K>,
}

impl<K, V> Default for AsyncCache<K, V> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            inflight: None,
            wanted: None,
        }
    }
}

impl<K: GitRead<V>, V: Send + 'static> AsyncCache<K, V> {
    /// Collects a finished read and starts the one waiting.
    pub fn poll(&mut self, ctx: &Context, git: &Git, root: &Path) {
        if let Some((key, receiver)) = self.inflight.take() {
            let state = match receiver.try_recv() {
                Ok(Ok(value)) => State::Ready(value),
                Ok(Err(message)) => State::Failed(message),
                Err(TryRecvError::Empty) => {
                    self.inflight = Some((key, receiver));
                    return;
                }
                Err(TryRecvError::Disconnected) => State::Failed(K::LOST.to_owned()),
            };
            self.insert(key, state);
        }
        if let Some(key) = self.wanted.take() {
            self.start(ctx, git, root, key);
        }
    }

    /// Asks for `key` unless it is stored or in flight. Call it every frame
    /// the value is on screen: that is what keeps a key in the cache.
    pub fn request<Q>(&mut self, key: &Q)
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ToOwned<Owned = K> + ?Sized,
    {
        if self.entries.contains_key(key) {
            self.touch(key);
        } else if self
            .wanted
            .as_ref()
            .is_none_or(|wanted| <K as Borrow<Q>>::borrow(wanted) != key)
        {
            self.wanted = Some(key.to_owned());
        }
    }

    fn start(&mut self, ctx: &Context, git: &Git, root: &Path, key: K) {
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        let git = git.clone();
        let root = root.to_path_buf();
        let worker_key = key.clone();
        thread::spawn(move || {
            let _ = sender.send(
                worker_key
                    .read(&git, &root)
                    .map_err(|error| error.to_string()),
            );
            ctx.request_repaint();
        });
        self.insert(key.clone(), State::Loading);
        self.inflight = Some((key, receiver));
    }
}

impl<K: Clone + Eq + Hash, V> AsyncCache<K, V> {
    /// The read for `key`, when one has been asked for.
    pub fn cached<Q>(&self, key: &Q) -> Option<Cached<'_, V>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.entries.get(key).map(|state| match state {
            State::Loading => Cached::Pending,
            State::Ready(value) => Cached::Ready(value),
            State::Failed(message) => Cached::Failed(message),
        })
    }

    /// Stores a finished read so a test can draw without git.
    #[cfg(test)]
    pub fn store_ready(&mut self, key: K, value: V) {
        self.insert(key, State::Ready(value));
    }

    fn insert(&mut self, key: K, state: State<V>) {
        if self.entries.insert(key.clone(), state).is_some() {
            self.touch(&key);
            return;
        }
        self.order.push_back(key);
        if self.order.len() > MAX_ENTRIES
            && let Some(oldest) = self.order.pop_front()
        {
            self.entries.remove(&oldest);
        }
    }

    /// Moves `key` to the recent end. The key asked for every frame is
    /// usually there already.
    fn touch<Q>(&mut self, key: &Q)
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        let matches = |stored: &K| <K as Borrow<Q>>::borrow(stored) == key;
        if self.order.back().is_some_and(matches) {
            return;
        }
        if let Some(index) = self.order.iter().position(matches)
            && let Some(stored) = self.order.remove(index)
        {
            self.order.push_back(stored);
        }
    }
}

#[cfg(test)]
mod tests {
    use cthulhu_git::git::CommitHover;

    use super::*;

    #[test]
    fn only_the_newest_request_waits_while_one_is_running() {
        let mut cache = AsyncCache::<String, CommitHover>::default();
        let (_sender, receiver) = mpsc::channel();
        let running = "a".repeat(40);
        cache.insert(running.clone(), State::Loading);
        cache.inflight = Some((running.clone(), receiver));
        cache.request(running.as_str());
        assert_eq!(cache.wanted, None);
        cache.request("b".repeat(40).as_str());
        cache.request("c".repeat(40).as_str());
        assert_eq!(cache.wanted, Some("c".repeat(40)));
        // A poll while the read runs keeps it and starts nothing.
        cache.poll(
            &Context::default(),
            &crate::ui::test_support::absent_git(),
            Path::new("/repos"),
        );
        let running = cache.inflight.as_ref().map(|(key, _)| key.as_str());
        assert_eq!(running, Some("a".repeat(40).as_str()));
        assert_eq!(cache.wanted, Some("c".repeat(40)));
    }
}
