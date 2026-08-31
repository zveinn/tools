//! Cheap Linux extras: names, xattrs, inode flags, XFS fsxattr/dioinfo.

use std::ffi::{CStr, CString, OsStr};
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

// --- ioctl request codes (x86_64 / LP64) ------------------------------------

const fn ior(ty: u8, nr: u8, size: usize) -> libc::c_ulong {
    ((2u32 << 30) | ((size as u32) << 16) | ((ty as u32) << 8) | nr as u32) as libc::c_ulong
}

const FS_IOC_GETFLAGS: libc::c_ulong = ior(b'f', 1, 8);
const FS_IOC_GETVERSION: libc::c_ulong = ior(b'v', 1, 8);
const FS_IOC_FSGETXATTR: libc::c_ulong = ior(b'X', 31, 28);
const XFS_IOC_DIOINFO: libc::c_ulong = ior(b'X', 30, 12);

// FS_IOC_GETFLAGS bits
const FS_SECRM_FL: u32 = 0x0000_0001;
const FS_UNRM_FL: u32 = 0x0000_0002;
const FS_COMPR_FL: u32 = 0x0000_0004;
const FS_SYNC_FL: u32 = 0x0000_0008;
const FS_IMMUTABLE_FL: u32 = 0x0000_0010;
const FS_APPEND_FL: u32 = 0x0000_0020;
const FS_NODUMP_FL: u32 = 0x0000_0040;
const FS_NOATIME_FL: u32 = 0x0000_0080;
const FS_ENCRYPT_FL: u32 = 0x0000_0800;
const FS_JOURNAL_DATA_FL: u32 = 0x0000_4000;
const FS_NOTAIL_FL: u32 = 0x0000_8000;
const FS_DIRSYNC_FL: u32 = 0x0001_0000;
const FS_TOPDIR_FL: u32 = 0x0002_0000;
const FS_EXTENT_FL: u32 = 0x0008_0000;
const FS_VERITY_FL: u32 = 0x0010_0000;
const FS_NOCOW_FL: u32 = 0x0080_0000;
const FS_DAX_FL: u32 = 0x0200_0000;
const FS_PROJINHERIT_FL: u32 = 0x2000_0000;
const FS_CASEFOLD_FL: u32 = 0x4000_0000;

// FS_IOC_FSGETXATTR xflags
const FS_XFLAG_REALTIME: u32 = 0x0000_0001;
const FS_XFLAG_PREALLOC: u32 = 0x0000_0002;
const FS_XFLAG_IMMUTABLE: u32 = 0x0000_0008;
const FS_XFLAG_APPEND: u32 = 0x0000_0010;
const FS_XFLAG_SYNC: u32 = 0x0000_0020;
const FS_XFLAG_NOATIME: u32 = 0x0000_0040;
const FS_XFLAG_NODUMP: u32 = 0x0000_0080;
const FS_XFLAG_RTINHERIT: u32 = 0x0000_0100;
const FS_XFLAG_PROJINHERIT: u32 = 0x0000_0200;
const FS_XFLAG_NOSYMLINKS: u32 = 0x0000_0400;
const FS_XFLAG_EXTSIZE: u32 = 0x0000_0800;
const FS_XFLAG_EXTSZINHERIT: u32 = 0x0000_1000;
const FS_XFLAG_NODEFRAG: u32 = 0x0000_2000;
const FS_XFLAG_FILESTREAM: u32 = 0x0000_4000;
const FS_XFLAG_DAX: u32 = 0x0000_8000;
const FS_XFLAG_COWEXTSIZE: u32 = 0x0001_0000;
const FS_XFLAG_HASATTR: u32 = 0x8000_0000;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FsxAttr {
    xflags: u32,
    extsize: u32,
    nextents: u32,
    projid: u32,
    cowextsize: u32,
    pad: [u8; 8],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DioAttr {
    mem: u32,
    miniosz: u32,
    maxiosz: u32,
}

#[derive(Debug, Clone)]
pub struct XfsInfo {
    pub xflags: Vec<&'static str>,
    pub extsize: u32,
    pub nextents: u32,
    pub projid: u32,
    pub cowextsize: u32,
    pub dio_mem: Option<u32>,
    pub dio_min: Option<u32>,
    pub dio_max: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct LinuxExtras {
    pub flags: Vec<&'static str>,
    pub inode_gen: Option<u32>,
    pub xattrs: Vec<String>,
    pub has_acl: bool,
    pub xfs: Option<XfsInfo>,
}

pub fn user_name(uid: u32) -> String {
    unsafe {
        let mut buf = [0i8; 1024];
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut result = std::ptr::null_mut();
        if libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result) == 0
            && !result.is_null()
        {
            return CStr::from_ptr(pwd.pw_name).to_string_lossy().into_owned();
        }
    }
    uid.to_string()
}

pub fn group_name(gid: u32) -> String {
    unsafe {
        let mut buf = [0i8; 1024];
        let mut grp: libc::group = std::mem::zeroed();
        let mut result = std::ptr::null_mut();
        if libc::getgrgid_r(gid, &mut grp, buf.as_mut_ptr(), buf.len(), &mut result) == 0
            && !result.is_null()
        {
            return CStr::from_ptr(grp.gr_name).to_string_lossy().into_owned();
        }
    }
    gid.to_string()
}

pub fn dev_major_minor(dev: u64) -> (u32, u32) {
    let d = dev as libc::dev_t;
    (libc::major(d) as u32, libc::minor(d) as u32)
}

/// Non-XFS extras always safe to ask for. `want_xfs` adds cheap XFS ioctls.
pub fn linux_extras(path: &Path, want_xfs: bool) -> LinuxExtras {
    let mut out = LinuxExtras {
        xattrs: list_xattrs(path),
        ..Default::default()
    };
    out.has_acl = out.xattrs.iter().any(|a| a.starts_with("system.posix_acl"));

    let Ok(file) = open_for_ioctl(path) else {
        return out;
    };
    let fd = file.as_raw_fd();

    if let Some(flags) = ioctl_u32(fd, FS_IOC_GETFLAGS) {
        out.flags = decode_fs_flags(flags);
    }
    if let Some(igen) = ioctl_u32(fd, FS_IOC_GETVERSION) {
        out.inode_gen = Some(igen);
    }

    if want_xfs {
        out.xfs = read_xfs(fd);
    }

    out
}

fn open_for_ioctl(path: &Path) -> io::Result<File> {
    // O_NOFOLLOW: never follow symlinks — extras must describe the entry itself.
    // Symlinks typically fail open here; caller then skips ioctl fields.
    let c = CString::new(path.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd_owned(fd) })
}

/// Helper: own an fd from raw without closing twice.
trait FromRawFdOwned {
    unsafe fn from_raw_fd_owned(fd: libc::c_int) -> Self;
}

impl FromRawFdOwned for File {
    unsafe fn from_raw_fd_owned(fd: libc::c_int) -> Self {
        use std::os::fd::FromRawFd;
        unsafe { File::from_raw_fd(fd) }
    }
}

fn ioctl_u32(fd: libc::c_int, req: libc::c_ulong) -> Option<u32> {
    let mut val: libc::c_long = 0;
    let rc = unsafe { libc::ioctl(fd, req, &mut val as *mut _) };
    if rc == 0 {
        Some(val as u32)
    } else {
        None
    }
}

fn read_xfs(fd: libc::c_int) -> Option<XfsInfo> {
    let mut fsx = FsxAttr::default();
    let rc = unsafe { libc::ioctl(fd, FS_IOC_FSGETXATTR, &mut fsx as *mut _) };
    if rc != 0 {
        return None;
    }

    let mut info = XfsInfo {
        xflags: decode_xflags(fsx.xflags),
        extsize: fsx.extsize,
        nextents: fsx.nextents,
        projid: fsx.projid,
        cowextsize: fsx.cowextsize,
        dio_mem: None,
        dio_min: None,
        dio_max: None,
    };

    let mut dio = DioAttr::default();
    if unsafe { libc::ioctl(fd, XFS_IOC_DIOINFO, &mut dio as *mut _) } == 0 {
        // Dirs often return zeros; still record if any field is set.
        if dio.mem != 0 || dio.miniosz != 0 || dio.maxiosz != 0 {
            info.dio_mem = Some(dio.mem);
            info.dio_min = Some(dio.miniosz);
            info.dio_max = Some(dio.maxiosz);
        }
    }

    Some(info)
}

fn list_xattrs(path: &Path) -> Vec<String> {
    let c = match CString::new(path.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    // llistxattr: do not follow symlinks.
    let size = unsafe { libc::llistxattr(c.as_ptr(), std::ptr::null_mut(), 0) };
    if size <= 0 {
        return Vec::new();
    }
    let mut buf = vec![0u8; size as usize];
    let n = unsafe { libc::llistxattr(c.as_ptr(), buf.as_mut_ptr() as *mut _, buf.len()) };
    if n <= 0 {
        return Vec::new();
    }
    buf.truncate(n as usize);

    buf.split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| OsStr::from_bytes(s).to_string_lossy().into_owned())
        .collect()
}

fn decode_fs_flags(f: u32) -> Vec<&'static str> {
    flag_names(
        f,
        &[
            (FS_SECRM_FL, "secrm"),
            (FS_UNRM_FL, "unrm"),
            (FS_COMPR_FL, "compr"),
            (FS_SYNC_FL, "sync"),
            (FS_IMMUTABLE_FL, "immutable"),
            (FS_APPEND_FL, "append"),
            (FS_NODUMP_FL, "nodump"),
            (FS_NOATIME_FL, "noatime"),
            (FS_ENCRYPT_FL, "encrypt"),
            (FS_JOURNAL_DATA_FL, "journal"),
            (FS_NOTAIL_FL, "notail"),
            (FS_DIRSYNC_FL, "dirsync"),
            (FS_TOPDIR_FL, "topdir"),
            (FS_EXTENT_FL, "extent"),
            (FS_VERITY_FL, "verity"),
            (FS_NOCOW_FL, "nocow"),
            (FS_DAX_FL, "dax"),
            (FS_PROJINHERIT_FL, "projinherit"),
            (FS_CASEFOLD_FL, "casefold"),
        ],
    )
}

fn decode_xflags(f: u32) -> Vec<&'static str> {
    flag_names(
        f,
        &[
            (FS_XFLAG_REALTIME, "realtime"),
            (FS_XFLAG_PREALLOC, "prealloc"),
            (FS_XFLAG_IMMUTABLE, "immutable"),
            (FS_XFLAG_APPEND, "append"),
            (FS_XFLAG_SYNC, "sync"),
            (FS_XFLAG_NOATIME, "noatime"),
            (FS_XFLAG_NODUMP, "nodump"),
            (FS_XFLAG_RTINHERIT, "rtinherit"),
            (FS_XFLAG_PROJINHERIT, "projinherit"),
            (FS_XFLAG_NOSYMLINKS, "nosymlinks"),
            (FS_XFLAG_EXTSIZE, "extsize"),
            (FS_XFLAG_EXTSZINHERIT, "extszinherit"),
            (FS_XFLAG_NODEFRAG, "nodefrag"),
            (FS_XFLAG_FILESTREAM, "filestream"),
            (FS_XFLAG_DAX, "dax"),
            (FS_XFLAG_COWEXTSIZE, "cowextsize"),
            (FS_XFLAG_HASATTR, "hasattr"),
        ],
    )
}

fn flag_names(f: u32, table: &[(u32, &'static str)]) -> Vec<&'static str> {
    table
        .iter()
        .filter_map(|&(bit, name)| (f & bit != 0).then_some(name))
        .collect()
}
