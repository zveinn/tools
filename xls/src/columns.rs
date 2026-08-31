//! Display / sort column definitions.

/// A listing column (also used as a sort key).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Mtime,
    /// Triads + type: `[rwx][r-x][r-x] dir`.
    Perms,
    /// Owner identity: `user`, or `user/group` when they differ.
    User,
    /// Group name only (optional detail column).
    Group,
    /// Other permission triad (optional detail column).
    Other,
    Size,
    Name,
    Nlink,
    Blocks,
    Sparse,
    Ino,
    Dev,
    Atime,
    Ctime,
    Birth,
    Flags,
    Xattrs,
    Xfs,
}

impl Column {
    /// Default column set.
    pub fn defaults() -> Vec<Self> {
        vec![Self::User, Self::Perms, Self::Size, Self::Name]
    }

    /// Every column, in a sensible display order.
    pub fn all() -> Vec<Self> {
        vec![
            Self::Mtime,
            Self::Nlink,
            Self::User,
            Self::Perms,
            Self::Group,
            Self::Other,
            Self::Size,
            Self::Blocks,
            Self::Sparse,
            Self::Ino,
            Self::Dev,
            Self::Atime,
            Self::Ctime,
            Self::Birth,
            Self::Flags,
            Self::Xattrs,
            Self::Xfs,
            Self::Name,
        ]
    }

    pub fn header(self) -> &'static str {
        match self {
            Self::Mtime => "MTIME",
            Self::Perms => "PERMS",
            Self::User => "USER",
            Self::Group => "GROUP",
            Self::Other => "OTHER",
            Self::Size => "SIZE",
            Self::Name => "NAME",
            Self::Nlink => "N",
            Self::Blocks => "BLOCKS",
            Self::Sparse => "S",
            Self::Ino => "INO:IGEN",
            Self::Dev => "DEV",
            Self::Atime => "ATIME",
            Self::Ctime => "CTIME",
            Self::Birth => "BIRTH",
            Self::Flags => "FLAGS",
            Self::Xattrs => "XATTRS",
            Self::Xfs => "XFS",
        }
    }

    /// All column names for help / errors.
    pub fn names() -> &'static [&'static str] {
        &[
            "MTIME", "PERMS", "USER", "GROUP", "OTHER", "SIZE", "NAME", "N", "BLOCKS", "S",
            "INO:IGEN", "DEV", "ATIME", "CTIME", "BIRTH", "FLAGS", "XATTRS", "XFS",
        ]
    }

    /// Parse one column name (case-insensitive, aliases allowed).
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_uppercase().as_str() {
            "MTIME" | "MODIFIED" => Ok(Self::Mtime),
            "PERMS" | "MODE" | "PERMISSIONS" => Ok(Self::Perms),
            "USER" | "OWNER" | "UID" => Ok(Self::User),
            "GROUP" | "GID" => Ok(Self::Group),
            "OTHER" | "OTH" | "WORLD" => Ok(Self::Other),
            "SIZE" => Ok(Self::Size),
            "NAME" => Ok(Self::Name),
            "N" | "NLINK" | "LINKS" => Ok(Self::Nlink),
            "BLOCKS" | "ALLOC" => Ok(Self::Blocks),
            "S" | "SPARSE" => Ok(Self::Sparse),
            "INO" | "INODE" | "INO:IGEN" | "IGEN" => Ok(Self::Ino),
            "DEV" | "DEVICE" => Ok(Self::Dev),
            "ATIME" | "ACCESSED" => Ok(Self::Atime),
            "CTIME" | "CHANGED" => Ok(Self::Ctime),
            "BIRTH" | "BTIME" | "CREATED" => Ok(Self::Birth),
            "FLAGS" | "FL" => Ok(Self::Flags),
            "XATTRS" | "XA" | "XATTR" => Ok(Self::Xattrs),
            "XFS" => Ok(Self::Xfs),
            other => Err(format!(
                "unknown column '{other}' (try: {})",
                Self::names().join(", ")
            )),
        }
    }

    /// Parse a comma-separated column list (order preserved).
    pub fn parse_list(s: &str) -> Result<Vec<Self>, String> {
        let cols: Result<Vec<_>, _> = s
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(Self::parse)
            .collect();
        let cols = cols?;
        if cols.is_empty() {
            return Err("--columns requires at least one column".into());
        }
        Ok(cols)
    }

    /// Minimum collection detail: 0 = stat, 1 = extras, 2 = + XFS.
    pub fn min_detail(self) -> u8 {
        match self {
            Self::Mtime
            | Self::Perms
            | Self::User
            | Self::Group
            | Self::Other
            | Self::Size
            | Self::Name
            | Self::Nlink
            | Self::Blocks
            | Self::Sparse
            | Self::Dev
            | Self::Atime
            | Self::Ctime
            | Self::Birth => 0,
            Self::Ino | Self::Flags | Self::Xattrs => 1,
            Self::Xfs => 2,
        }
    }

    pub fn max_detail(cols: &[Self]) -> u8 {
        cols.iter().map(|c| c.min_detail()).max().unwrap_or(0)
    }
}
