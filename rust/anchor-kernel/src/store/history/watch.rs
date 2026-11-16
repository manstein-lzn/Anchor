//! One process-wide mutation queue, with an independent cursor per cache.
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, Reader, WatchFlags};
use rustix::io::Errno;
use std::collections::BTreeMap;
use std::fs::File;
use std::mem::MaybeUninit;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

#[derive(Default)]
struct Token {
    generation: AtomicU64,
    revoked: AtomicBool,
    subscribers: AtomicUsize,
}

struct Hub {
    file: Option<File>,
    entries: BTreeMap<i32, Weak<Token>>,
}

static HUB: Mutex<Hub> = Mutex::new(Hub {
    file: None,
    entries: BTreeMap::new(),
});

#[cfg(test)]
pub(super) static TEST_LOCK: Mutex<()> = Mutex::new(());

impl Hub {
    fn invalidate(&mut self) {
        for token in self.entries.values().filter_map(Weak::upgrade) {
            token.revoked.store(true, Ordering::Relaxed);
        }
        self.entries.clear();
        self.file = None;
    }

    fn event(&mut self, wd: i32, flags: ReadFlags) -> bool {
        if flags.contains(ReadFlags::QUEUE_OVERFLOW) {
            return false;
        }
        let Some(token) = self.entries.get(&wd).and_then(Weak::upgrade) else {
            // rm_watch emits IGNORED after its final cache handle is gone.
            return flags == ReadFlags::IGNORED;
        };
        if (flags - (ReadFlags::MODIFY | ReadFlags::ATTRIB)).is_empty() {
            if token.generation.fetch_add(1, Ordering::Relaxed) == u64::MAX {
                token.revoked.store(true, Ordering::Relaxed);
            }
        } else {
            token.revoked.store(true, Ordering::Relaxed);
        }
        if flags.contains(ReadFlags::IGNORED) {
            self.entries.remove(&wd);
        }
        true
    }

    fn drain(&mut self) -> bool {
        if self.file.is_none() {
            return true;
        }
        let mut buffer = [MaybeUninit::uninit(); 4096];
        let mut events = Vec::new();
        let mut reader = Reader::new(self.file.as_ref().unwrap(), &mut buffer);
        let clean = loop {
            match reader.next() {
                Ok(event) => events.push((event.wd(), event.events())),
                Err(Errno::INTR) => continue,
                Err(Errno::AGAIN) => break true,
                Err(_) => break false,
            }
            // A partial Reader contains prefetched events. On any early stop
            // invalidate every token instead of silently discarding that tail.
            if events.len() >= 4096 {
                break false;
            }
        };
        if !clean || events.into_iter().any(|(wd, flags)| !self.event(wd, flags)) {
            self.invalidate();
            return false;
        }
        true
    }
}

pub(super) struct HistoryWatch {
    wd: i32,
    token: Arc<Token>,
    observed: u64,
}

pub(super) fn open(file: &File) -> Option<HistoryWatch> {
    open_with(
        file,
        || {
            inotify::init(CreateFlags::NONBLOCK | CreateFlags::CLOEXEC)
                .ok()
                .map(Into::into)
        },
        |inotify, path| {
            inotify::add_watch(
                inotify,
                path,
                WatchFlags::MODIFY
                    | WatchFlags::ATTRIB
                    | WatchFlags::DELETE_SELF
                    | WatchFlags::MOVE_SELF,
            )
            .ok()
        },
    )
}

fn open_with(
    file: &File,
    init: impl FnOnce() -> Option<File>,
    add: impl FnOnce(&File, String) -> Option<i32>,
) -> Option<HistoryWatch> {
    let mut hub = HUB.lock().expect("history watch hub");
    hub.drain();
    if hub.file.is_none() {
        hub.file = Some(init()?);
    }
    let result = add(
        hub.file.as_ref().unwrap(),
        format!("/proc/self/fd/{}", file.as_raw_fd()),
    );
    let wd = match result {
        Some(wd) => wd,
        None => {
            if hub.entries.is_empty() {
                hub.file = None;
            }
            return None;
        }
    };
    let token = match hub.entries.get(&wd).and_then(Weak::upgrade) {
        Some(token) if !token.revoked.load(Ordering::Relaxed) => token,
        _ => {
            let token = Arc::new(Token::default());
            hub.entries.insert(wd, Arc::downgrade(&token));
            token
        }
    };
    let observed = token.generation.load(Ordering::Relaxed);
    token.subscribers.fetch_add(1, Ordering::Relaxed);
    Some(HistoryWatch {
        wd,
        token,
        observed,
    })
}

impl HistoryWatch {
    pub(super) fn unchanged(&self) -> bool {
        let mut hub = HUB.lock().expect("history watch hub");
        hub.drain()
            && !self.token.revoked.load(Ordering::Relaxed)
            && self.observed == self.token.generation.load(Ordering::Relaxed)
    }

    pub(super) fn written(mut self) -> Option<Self> {
        let mut hub = HUB.lock().expect("history watch hub");
        let valid = hub.drain() && !self.token.revoked.load(Ordering::Relaxed);
        if valid {
            self.observed = self.token.generation.load(Ordering::Relaxed);
        }
        // Drop reacquires HUB, so release it before dropping a revoked handle.
        drop(hub);
        valid.then_some(self)
    }
}

impl Drop for HistoryWatch {
    fn drop(&mut self) {
        let mut hub = HUB.lock().unwrap_or_else(|error| error.into_inner());
        // Process queued changes while the old descriptor still has its token.
        // Otherwise its pending MODIFY would look like an unknown descriptor.
        hub.drain();
        let last = self.token.subscribers.fetch_sub(1, Ordering::Relaxed) == 1;
        let same = hub
            .entries
            .get(&self.wd)
            .is_some_and(|entry| entry.ptr_eq(&Arc::downgrade(&self.token)));
        if same && last {
            hub.entries.remove(&self.wd);
            if let Some(file) = hub.file.as_ref() {
                let _ = inotify::remove_watch(file, self.wd);
            }
            if hub.entries.is_empty() {
                hub.file = None;
            } else {
                hub.drain();
            }
        }
        if hub.entries.is_empty() {
            hub.file = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_preserves_other_files_and_other_caches_of_the_same_inode() {
        let _test = TEST_LOCK.lock().unwrap();
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a");
        let b = root.path().join("b");
        std::fs::write(&a, []).unwrap();
        std::fs::write(&b, []).unwrap();
        let first = open(&File::open(&a).unwrap()).unwrap();
        let second = open(&File::open(&a).unwrap()).unwrap();
        let other = open(&File::open(&b).unwrap()).unwrap();
        assert!(Arc::ptr_eq(&first.token, &second.token));
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let first = first.written().unwrap();
        assert!(first.unchanged());
        assert!(!second.unchanged());
        assert!(!other.unchanged());
        let descriptor = second.wd;
        drop(first);
        assert!(HUB.lock().unwrap().entries.contains_key(&descriptor));
        drop(second);
        assert!(!HUB.lock().unwrap().entries.contains_key(&descriptor));
        // Removing one history must not reset the remaining cache.
        let other = other.written().unwrap();
        assert!(other.unchanged());
    }

    #[test]
    fn queued_events_across_many_buffers_are_all_dispatched() {
        let _test = TEST_LOCK.lock().unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut paths = Vec::new();
        let mut watches = Vec::new();
        for index in 0..300 {
            let path = root.path().join(index.to_string());
            std::fs::write(&path, []).unwrap();
            watches.push(open(&File::open(&path).unwrap()).unwrap());
            paths.push(path);
        }
        for path in paths {
            std::fs::write(path, b"changed").unwrap();
        }
        assert!(watches.iter().all(|watch| !watch.unchanged()));
    }

    #[test]
    fn overflow_read_errors_and_revocation_never_revalidate_old_tokens() {
        let _test = TEST_LOCK.lock().unwrap();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("a");
        std::fs::write(&path, []).unwrap();
        for error in [ReadFlags::QUEUE_OVERFLOW, ReadFlags::IGNORED] {
            let watch = open(&File::open(&path).unwrap()).unwrap();
            {
                let mut hub = HUB.lock().unwrap();
                let wd = if error == ReadFlags::QUEUE_OVERFLOW {
                    -1
                } else {
                    watch.wd
                };
                if !hub.event(wd, error) {
                    hub.invalidate();
                }
            }
            let replacement = open(&File::open(&path).unwrap()).unwrap();
            assert!(!watch.unchanged());
            assert!(watch.written().is_none());
            assert!(replacement.unchanged());
        }
        let watch = open(&File::open(&path).unwrap()).unwrap();
        {
            let mut hub = HUB.lock().unwrap();
            // A regular EOF descriptor exercises Reader's error path.
            hub.file = Some(File::open(&path).unwrap());
            assert!(!hub.drain());
        }
        assert!(!watch.unchanged());
        assert!(watch.written().is_none());
    }

    #[test]
    fn explicit_revoke_and_parallel_last_drop_do_not_leak_or_remove_replacements() {
        let _test = TEST_LOCK.lock().unwrap();
        let file = tempfile::tempfile().unwrap();
        let first = open(&file).unwrap();
        let second = open(&file).unwrap();
        let old = first.wd;
        std::thread::scope(|scope| {
            scope.spawn(move || drop(first));
            scope.spawn(move || drop(second));
        });
        assert!(!HUB.lock().unwrap().entries.contains_key(&old));
        let revoked = open(&file).unwrap();
        {
            let hub = HUB.lock().unwrap();
            inotify::remove_watch(hub.file.as_ref().unwrap(), revoked.wd).unwrap();
        }
        let replacement = open(&file).unwrap();
        assert!(revoked.written().is_none());
        assert!(replacement.unchanged());
        drop(replacement);
        assert!(HUB.lock().unwrap().file.is_none());
    }

    #[test]
    fn unavailable_instance_or_watch_allocation_does_not_break_other_caches() {
        let _test = TEST_LOCK.lock().unwrap();
        let file = tempfile::tempfile().unwrap();
        assert!(HUB.lock().unwrap().file.is_none());
        assert!(open_with(&file, || None, |_, _| panic!("init failed")).is_none());
        assert!(open_with(&file, || tempfile::tempfile().ok(), |_, _| None).is_none());
        assert!(HUB.lock().unwrap().file.is_none());
        let existing = open(&file).unwrap();
        assert!(open_with(&file, || panic!("existing instance"), |_, _| None).is_none());
        assert!(existing.unchanged());
        drop(existing);
        assert!(HUB.lock().unwrap().file.is_none());
    }
}
