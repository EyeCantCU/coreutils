// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

// spell-checker:ignore CLOEXEC INTR NOENT NOSPC NOWRITE nsec unwatch wd

//! inotify backend for `tail -f` on Linux, yielding [`notify::Event`]s for the shared follow loop.
//!
//! Watches are added and removed from the following (main) thread, as GNU tail does, and a
//! rotated-away file loses its watch so it does not stay busy.

use notify::event::{
    AccessKind, AccessMode, CreateKind, DataChange, EventKind, Flag, MetadataKind, ModifyKind,
    RemoveKind, RenameMode,
};
use notify::{Error, ErrorKind, Event};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fd::OwnedFd;
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use rustix::io::Errno;
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

/// Room for many events, and at least one with a name of the maximal length.
const BUFFER_SIZE: usize = 4096;

/// Which events a watch asks for and reports.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    /// A directory, or a followed name that is not a regular file: all events.
    Full,
    /// The regular file behind a followed name: data and attribute changes only.
    Content,
}

impl Role {
    fn mask(self) -> WatchFlags {
        match self {
            Self::Full => {
                WatchFlags::ATTRIB
                    | WatchFlags::CREATE
                    | WatchFlags::OPEN
                    | WatchFlags::DELETE
                    | WatchFlags::CLOSE_WRITE
                    | WatchFlags::MODIFY
                    | WatchFlags::MOVED_FROM
                    | WatchFlags::MOVED_TO
                    | WatchFlags::DELETE_SELF
                    | WatchFlags::MOVE_SELF
            }
            Self::Content => WatchFlags::MODIFY | WatchFlags::ATTRIB,
        }
    }
}

struct Entry {
    path: PathBuf,
    role: Role,
    is_dir: bool,
}

struct RawEvent {
    wd: i32,
    mask: ReadFlags,
    cookie: u32,
    name: Option<OsString>,
}

pub struct InotifyWatcher {
    fd: OwnedFd,
    /// Several entries share a descriptor when their paths lead to the same inode.
    entries: HashMap<i32, Vec<Entry>>,
    /// Names that get a [`Role::Content`] watch whenever they refer to a regular file.
    followed: HashSet<PathBuf>,
    /// Set once the kernel refused a watch on a file for lack of watch budget: later
    /// file watches are skipped, as the directory watches report the same changes.
    file_watches_exhausted: bool,
    /// Events read but not handed out yet.
    pending: VecDeque<notify::Result<Event>>,
    /// The last name moved away, to pair it with the name it was moved to.
    rename_from: Option<(u32, PathBuf)>,
    buffer: Box<[MaybeUninit<u8>]>,
}

impl InotifyWatcher {
    pub fn new() -> std::io::Result<Self> {
        let fd = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)?;
        Ok(Self {
            fd,
            entries: HashMap::new(),
            followed: HashSet::new(),
            file_watches_exhausted: false,
            pending: VecDeque::new(),
            rename_from: None,
            buffer: vec![MaybeUninit::uninit(); BUFFER_SIZE].into_boxed_slice(),
        })
    }

    pub fn watch(&mut self, path: &Path) -> notify::Result<()> {
        let path = absolute(path);
        let is_dir = path.is_dir();
        if !is_dir {
            self.followed.insert(path.clone());
        }
        self.add(&path, Role::Full, is_dir)
    }

    /// Stop watching `path` and stop following the name.
    pub fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
        let path = absolute(path);
        self.followed.remove(&path);
        if self.remove(&path, None) {
            Ok(())
        } else {
            Err(Error::watch_not_found().add_path(path))
        }
    }

    /// Watch the regular file behind the name `path`, now and after it is replaced.
    pub fn follow_file(&mut self, path: &Path) {
        let path = absolute(path);
        self.add_content_watch(&path);
        self.followed.insert(path);
    }

    pub fn recv_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<notify::Result<Event>, RecvTimeoutError> {
        let deadline = Instant::now().checked_add(timeout);
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Ok(event);
            }
            let remaining = deadline.map(|d| d.saturating_duration_since(Instant::now()));
            let timespec = remaining.map(|r| Timespec {
                tv_sec: i64::try_from(r.as_secs()).unwrap_or(i64::MAX),
                tv_nsec: i64::from(r.subsec_nanos()),
            });
            let mut fds = [PollFd::new(&self.fd, PollFlags::IN)];
            match poll(&mut fds, timespec.as_ref()) {
                Ok(0) => return Err(RecvTimeoutError::Timeout),
                Ok(_) | Err(Errno::INTR) => {}
                Err(e) => return Ok(Err(Error::io(e.into()))),
            }
            if let Err(e) = self.read_events() {
                return Ok(Err(Error::io(e.into())));
            }
        }
    }

    pub fn try_recv(&mut self) -> Result<notify::Result<Event>, TryRecvError> {
        if self.pending.is_empty()
            && let Err(e) = self.read_events()
        {
            return Ok(Err(Error::io(e.into())));
        }
        self.pending.pop_front().ok_or(TryRecvError::Empty)
    }

    fn add(&mut self, path: &Path, role: Role, is_dir: bool) -> notify::Result<()> {
        // Paths to one inode share a kernel watch: extend its mask rather than replace it.
        let wd = inotify::add_watch(&self.fd, path, role.mask() | WatchFlags::MASK_ADD).map_err(
            |e| {
                match e {
                    Errno::NOSPC => Error::new(ErrorKind::MaxFilesWatch),
                    Errno::NOENT => Error::new(ErrorKind::PathNotFound),
                    e => Error::io(e.into()),
                }
                .add_path(path.to_owned())
            },
        )?;
        let list = self.entries.entry(wd).or_default();
        list.retain(|e| e.role != role || e.path != path);
        list.push(Entry {
            path: path.to_owned(),
            role,
            is_dir,
        });
        Ok(())
    }

    /// Remove `path`'s watches with `role` (or all), and the kernel watch once unused.
    /// Returns whether any existed.
    fn remove(&mut self, path: &Path, role: Option<Role>) -> bool {
        let mut found = false;
        let mut unused = Vec::new();
        for (wd, list) in &mut self.entries {
            let before = list.len();
            list.retain(|e| e.path != path || role.is_some_and(|r| r != e.role));
            if list.len() != before {
                found = true;
                if list.is_empty() {
                    unused.push(*wd);
                }
            }
        }
        for wd in unused {
            // A freed watch makes room again.
            self.file_watches_exhausted = false;
            self.entries.remove(&wd);
            // The kernel may have dropped the watch already, with its inode.
            let _ = inotify::remove_watch(&self.fd, wd);
        }
        found
    }

    fn add_content_watch(&mut self, path: &Path) {
        if !self.file_watches_exhausted && path.symlink_metadata().is_ok_and(|m| m.is_file()) {
            let result = self.add(path, Role::Content, false);
            self.note_content_watch_result(&result);
        }
    }

    /// A file watch is best-effort: the directory watch still reports the changes. When the
    /// watch budget is used up, stop adding file watches so that directory watches can succeed.
    fn note_content_watch_result(&mut self, result: &notify::Result<()>) {
        if matches!(result, Err(e) if matches!(e.kind, ErrorKind::MaxFilesWatch)) {
            self.file_watches_exhausted = true;
        }
    }

    /// A name now refers to another file, or to none.
    fn name_gone(&mut self, path: &Path) {
        self.remove(path, Some(Role::Content));
    }

    /// A name refers to a new file.
    fn name_appeared(&mut self, path: &Path) {
        if self.followed.contains(path) {
            self.remove(path, Some(Role::Content));
            self.add_content_watch(path);
        }
    }

    fn read_events(&mut self) -> rustix::io::Result<()> {
        let mut raw = Vec::new();
        let mut reader = inotify::Reader::new(&self.fd, &mut self.buffer);
        let result = loop {
            match reader.next() {
                Ok(event) => raw.push(RawEvent {
                    wd: event.wd(),
                    mask: event.events(),
                    cookie: event.cookie(),
                    name: event
                        .file_name()
                        .map(|n| OsStr::from_bytes(n.to_bytes()).to_owned()),
                }),
                Err(Errno::AGAIN) => break Ok(()),
                Err(Errno::INTR) => {}
                Err(e) => break Err(e),
            }
        };
        let mut removed = Vec::new();
        for event in raw {
            self.translate(&event, &mut removed);
        }
        // Only drop watches of vanished names after the whole batch is translated.
        for path in removed {
            self.remove(&path, Some(Role::Full));
        }
        result
    }

    fn translate(&mut self, event: &RawEvent, removed: &mut Vec<PathBuf>) {
        if event.mask.contains(ReadFlags::QUEUE_OVERFLOW) {
            self.push(Event::new(EventKind::Other).set_flag(Flag::Rescan));
        }
        if event.mask.contains(ReadFlags::IGNORED) {
            // The kernel dropped this watch.
            self.entries.remove(&event.wd);
            return;
        }
        let Some(list) = self.entries.get(&event.wd) else {
            return;
        };
        let targets: Vec<(PathBuf, Role, bool)> = list
            .iter()
            .map(|e| {
                let path = match &event.name {
                    Some(name) => e.path.join(name),
                    None => e.path.clone(),
                };
                (path, e.role, e.is_dir)
            })
            .collect();
        for (path, role, is_dir) in targets {
            match role {
                Role::Full => self.translate_full(event, path, is_dir, removed),
                Role::Content => self.translate_content(event, path),
            }
        }
    }

    fn translate_content(&mut self, event: &RawEvent, path: PathBuf) {
        if event.mask.contains(ReadFlags::MODIFY) {
            self.push(
                Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Any)))
                    .add_path(path.clone()),
            );
        }
        if event.mask.contains(ReadFlags::ATTRIB) {
            self.push(
                Event::new(EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any)))
                    .add_path(path),
            );
        }
    }

    fn translate_full(
        &mut self,
        event: &RawEvent,
        path: PathBuf,
        is_dir: bool,
        removed: &mut Vec<PathBuf>,
    ) {
        let mask = event.mask;
        let tracker = event.cookie as usize;
        let names_dir = mask.contains(ReadFlags::ISDIR);
        if mask.contains(ReadFlags::MOVED_FROM) {
            self.name_gone(&path);
            removed.push(path.clone());
            self.rename_from = Some((event.cookie, path.clone()));
            self.push(
                Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::From)))
                    .add_path(path.clone())
                    .set_tracker(tracker),
            );
        } else if mask.contains(ReadFlags::MOVED_TO) {
            self.name_appeared(&path);
            self.push(
                Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::To)))
                    .set_tracker(tracker)
                    .add_path(path.clone()),
            );
            if self
                .rename_from
                .as_ref()
                .is_some_and(|(cookie, _)| *cookie == event.cookie)
            {
                let from = self.rename_from.take().map(|(_, p)| p);
                self.push(
                    Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
                        .set_tracker(tracker)
                        .add_some_path(from)
                        .add_path(path.clone()),
                );
            }
        }
        if mask.contains(ReadFlags::MOVE_SELF) {
            self.push(
                Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::From)))
                    .add_path(path.clone()),
            );
        }
        if mask.contains(ReadFlags::CREATE) {
            self.name_appeared(&path);
            let kind = if names_dir {
                CreateKind::Folder
            } else {
                CreateKind::File
            };
            self.push(Event::new(EventKind::Create(kind)).add_path(path.clone()));
        }
        if mask.contains(ReadFlags::DELETE) {
            self.name_gone(&path);
            let kind = if names_dir {
                RemoveKind::Folder
            } else {
                RemoveKind::File
            };
            self.push(Event::new(EventKind::Remove(kind)).add_path(path.clone()));
            removed.push(path.clone());
        }
        if mask.contains(ReadFlags::DELETE_SELF) {
            let kind = if is_dir {
                RemoveKind::Folder
            } else {
                RemoveKind::File
            };
            self.push(Event::new(EventKind::Remove(kind)).add_path(path.clone()));
            removed.push(path.clone());
        }
        let others = [
            (
                ReadFlags::MODIFY,
                EventKind::Modify(ModifyKind::Data(DataChange::Any)),
            ),
            (
                ReadFlags::CLOSE_WRITE,
                EventKind::Access(AccessKind::Close(AccessMode::Write)),
            ),
            (
                ReadFlags::CLOSE_NOWRITE,
                EventKind::Access(AccessKind::Close(AccessMode::Read)),
            ),
            (
                ReadFlags::ATTRIB,
                EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any)),
            ),
            (
                ReadFlags::OPEN,
                EventKind::Access(AccessKind::Open(AccessMode::Any)),
            ),
        ];
        for (flag, kind) in others {
            if mask.contains(flag) {
                self.push(Event::new(kind).add_path(path.clone()));
            }
        }
    }

    /// Skips a repeat of the last change: the directory and the file both report a write.
    fn push(&mut self, event: Event) {
        let repeated = matches!(
            event.kind,
            EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Metadata(_))
        ) && matches!(self.pending.back(), Some(Ok(last)) if last.kind == event.kind && last.paths == event.paths);
        if !repeated {
            self.pending.push_back(Ok(event));
        }
    }
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_relative()
        && let Ok(dir) = std::env::current_dir()
    {
        return dir.join(path);
    }
    path.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausted_watch_budget_skips_file_watches() {
        let dir = std::env::temp_dir().join(format!("tail-inotify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("f");
        std::fs::write(&file, "x").unwrap();
        let mut watcher = InotifyWatcher::new().unwrap();

        // Other errors do not stop file watches.
        watcher.note_content_watch_result(&Err(Error::new(ErrorKind::PathNotFound)));
        assert!(!watcher.file_watches_exhausted);

        watcher.note_content_watch_result(&Err(Error::new(ErrorKind::MaxFilesWatch)));
        assert!(watcher.file_watches_exhausted);
        watcher.follow_file(&file);
        assert!(watcher.entries.is_empty());

        // The directory watch is still made, and the budget is usable again after a removal.
        watcher.watch(&dir).unwrap();
        assert_eq!(watcher.entries.len(), 1);
        assert!(watcher.remove(&dir, None));
        assert!(!watcher.file_watches_exhausted);
        watcher.follow_file(&file);
        assert_eq!(watcher.entries.len(), 1);

        std::fs::remove_file(&file).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn devices_are_watched() {
        let mut watcher = InotifyWatcher::new().unwrap();
        watcher.watch(Path::new("/dev/null")).unwrap();
        assert_eq!(watcher.entries.len(), 1);
    }
}
