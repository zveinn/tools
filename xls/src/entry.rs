//! Gather everything we can about a directory entry.

use std::fs::{self, FileType};
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::time::SystemTime;

use crate::sys::{self, LinuxExtras, XfsInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    Fifo,
    Socket,
    Block,
    Char,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub kind: Kind,
    pub executable: bool,
    pub mode: u32,
    pub nlink: u64,
    pub user: String,
    pub group: String,
    pub size: u64,
    pub blocks: u64,
    pub blksize: u64,
    pub sparse: bool,
    pub ino: u64,
    pub dev_major: u32,
    pub dev_minor: u32,
    pub rdev_major: u32,
    pub rdev_minor: u32,
    pub atime: Option<SystemTime>,
    pub mtime: Option<SystemTime>,
    pub ctime_secs: i64,
    pub birth: Option<SystemTime>,
    pub symlink: Option<String>,
    pub broken_symlink: bool,
    pub extras: LinuxExtras,
}

impl Entry {
    /// `detail`: 0 = basic (stat only), 1 = all non-XFS, 2 = + cheap XFS.
    pub fn collect(path: PathBuf, name: String, detail: u8) -> io::Result<Self> {
        let meta = fs::symlink_metadata(&path)?;
        let ft = meta.file_type();
        let kind = kind_of(ft);
        let mode = meta.permissions().mode();
        let executable = mode & 0o111 != 0;

        let (symlink, broken_symlink) = if kind == Kind::Symlink {
            match fs::read_link(&path) {
                Ok(t) => {
                    let target = t.to_string_lossy().into_owned();
                    let broken = fs::metadata(&path).is_err();
                    (Some(target), broken)
                }
                Err(_) => (None, true),
            }
        } else {
            (None, false)
        };

        let size = meta.len();
        let blocks = meta.blocks();
        // st_blocks is in 512-byte units.
        let allocated = blocks.saturating_mul(512);
        let sparse = kind == Kind::File && allocated < size;

        let dev = meta.dev();
        let (dev_major, dev_minor) = sys::dev_major_minor(dev);
        let rdev = meta.rdev();
        let (rdev_major, rdev_minor) = sys::dev_major_minor(rdev);

        let user = sys::user_name(meta.uid());
        let group = sys::group_name(meta.gid());

        let extras = if detail >= 1 {
            sys::linux_extras(&path, detail >= 2)
        } else {
            LinuxExtras::default()
        };

        Ok(Self {
            name,
            kind,
            executable,
            mode,
            nlink: meta.nlink(),
            user,
            group,
            size,
            blocks,
            blksize: meta.blksize(),
            sparse,
            ino: meta.ino(),
            dev_major,
            dev_minor,
            rdev_major,
            rdev_minor,
            atime: meta.accessed().ok(),
            mtime: meta.modified().ok(),
            ctime_secs: meta.ctime(),
            birth: meta.created().ok(),
            symlink,
            broken_symlink,
            extras,
        })
    }

    pub fn xfs(&self) -> Option<&XfsInfo> {
        self.extras.xfs.as_ref()
    }
}

fn kind_of(ft: FileType) -> Kind {
    if ft.is_symlink() {
        Kind::Symlink
    } else if ft.is_dir() {
        Kind::Dir
    } else if ft.is_file() {
        Kind::File
    } else if ft.is_fifo() {
        Kind::Fifo
    } else if ft.is_socket() {
        Kind::Socket
    } else if ft.is_block_device() {
        Kind::Block
    } else if ft.is_char_device() {
        Kind::Char
    } else {
        Kind::Unknown
    }
}

