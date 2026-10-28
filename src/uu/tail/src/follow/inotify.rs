// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

// spell-checker:ignore CLOEXEC INTR NOENT NOSPC NOWRITE nsec unwatch wd

//! Change notification for `tail -f` on Linux, driven by an inotify instance
//! that is owned and read by the thread that follows the files.
//!
//! The events are handed out as [`notify::Event`]s, so the follow loop handles
//! them the same way as the events of the other backends.
//!
//! The directory of each followed file is watched to notice the name being
//! removed, renamed or created. In addition, a followed name that refers to a
//! regular file has a watch on that file: it is added when the file is
//! followed or appears under the name, and it is removed as soon as the name
//! stops referring to the file, so a rotated log does not keep a watch busy.

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
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

/// Room for many events, and at least one with a name of the maximal length.
const BUFFER_SIZE: usize = 4096;

/// What a watch is for, which decides the events it asks for and reports.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    /// A directory whose entries are of interest, or a name tail follows
    /// that is not a regular file (a symbolic link, a FIFO): everything that
    /// happens to it is reported.
    Full,
    /// The regular file a followed name refers to: only changes to its data
    /// and attributes are reported. Its directory reports what happens to
    /// the name.
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

/// One event as read from the inotify descriptor.
struct RawEvent {
    wd: i32,
    mask: ReadFlags,
    cookie: u32,
    name: Option<OsString>,
}

pub struct InotifyWatcher {
    fd: OwnedFd,
    /// The watches by descriptor. Several entries share a descriptor when
    /// their paths lead to the same inode (hard links, a link and its target).
    entries: HashMap<i32, Vec<Entry>>,
    /// The followed names that get a [`Role::Content`] watch whenever they
    /// refer to a regular file.
    followed: HashSet<PathBuf>,
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
            pending: VecDeque::new(),
            rename_from: None,
            buffer: vec![MaybeUninit::uninit(); BUFFER_SIZE].into_boxed_slice(),
        })
    }

    /// Watch everything that happens to `path`, and in it if it is a
    /// directory. Devices are not watched: what they report says nothing
    /// about the data they have to read.
    pub fn watch(&mut self, path: &Path) -> notify::Result<()> {
        let path = absolute(path);
        if path.metadata().is_ok_and(|m| {
            let file_type = m.file_type();
            file_type.is_char_device() || file_type.is_block_device() || file_type.is_socket()
        }) {
            return Ok(());
        }
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

    /// Follow the name `path`: watch the regular file it refers to now, and
    /// whichever it refers to later on. The directory must be watched too.
    pub fn follow_file(&mut self, path: &Path) {
        let path = absolute(path);
        self.add_content_watch(&path);
        self.followed.insert(path);
    }

    /// Wait at most `timeout` for an event.
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

    /// Return an event that is already available, if any.
    pub fn try_recv(&mut self) -> Result<notify::Result<Event>, TryRecvError> {
        if self.pending.is_empty()
            && let Err(e) = self.read_events()
        {
            return Ok(Err(Error::io(e.into())));
        }
        self.pending.pop_front().ok_or(TryRecvError::Empty)
    }

    fn add(&mut self, path: &Path, role: Role, is_dir: bool) -> notify::Result<()> {
        // Paths leading to the same inode share the kernel watch, so the
        // mask is extended rather than replaced.
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

    /// Remove the watches of `path` with `role` (or all of them), and the
    /// kernel watch once no path uses it any more. Return whether there was
    /// a watch.
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
            self.entries.remove(&wd);
            // The kernel may have dropped the watch already, with its inode.
            let _ = inotify::remove_watch(&self.fd, wd);
        }
        found
    }

    fn add_content_watch(&mut self, path: &Path) {
        if path.symlink_metadata().is_ok_and(|m| m.is_file()) {
            // Not fatal: the directory watch still reports the changes.
            let _ = self.add(path, Role::Content, false);
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

    /// Read and translate all events that are ready.
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
        // Watches of names that went away are dropped once the events read
        // with them have been translated.
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

    /// Queue `event`, unless it repeats the change just queued: a write to a
    /// followed file is reported both by its directory and by the file.
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
