//! Turns a Wayland `app_id` into a human readable name and an icon bitmap.
//!
//! An `app_id` is not a desktop file name, it just usually looks like one:
//! `chromium` and `com.system76.CosmicFiles` both match a file directly, but
//! plenty of apps report something with different casing or a `.desktop`
//! basename that differs entirely. So we try the cheap direct paths first and
//! only build an index of every installed desktop file if those miss, since
//! that index costs a directory walk we would rather not pay on every launch.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tiny_skia::Pixmap;

/// Resolved presentation data for one application.
#[derive(Debug, Clone, Default)]
pub struct AppInfo {
    /// `Name=` from the desktop file, or the raw app_id if there is none.
    pub name: String,
    /// Rasterized icon, absent when nothing could be found or decoded.
    pub icon: Option<Pixmap>,
}

/// Caches lookups so the three Chromium windows share one icon decode.
pub struct IconCache {
    theme: String,
    scale: u32,
    /// Logical edge length to rasterize to, before the output scale.
    size: u32,
    by_app_id: HashMap<String, AppInfo>,
    /// Icons resolved by name rather than by app id, for title rules that
    /// override the icon.
    by_icon_name: HashMap<String, Option<Pixmap>>,
    /// Lazily built map of lowercased desktop id -> path, plus StartupWMClass
    /// entries pointing at the same files.
    index: Option<HashMap<String, PathBuf>>,
}

impl IconCache {
    pub fn new(theme: &str, scale: u32, size: u32) -> Self {
        Self {
            theme: theme.to_string(),
            scale: scale.max(1),
            size: size.max(1),
            by_app_id: HashMap::new(),
            by_icon_name: HashMap::new(),
            index: None,
        }
    }

    /// Resolves a bare icon name, for a title rule's `icon:` override.
    pub fn by_name(&mut self, name: &str) -> Option<Pixmap> {
        if let Some(hit) = self.by_icon_name.get(name) {
            return hit.clone();
        }
        let icon = self.load_icon(name);
        self.by_icon_name.insert(name.to_string(), icon.clone());
        icon
    }

    /// Looks up an app_id, decoding and caching on first use.
    pub fn get(&mut self, app_id: &str) -> AppInfo {
        if let Some(hit) = self.by_app_id.get(app_id) {
            return hit.clone();
        }

        let entry = self.find_desktop_entry(app_id);
        let name = entry
            .as_ref()
            .and_then(|path| desktop_field(path, "Name"))
            .unwrap_or_else(|| prettify(app_id));

        // Fall back to the app_id as an icon name: hicolor is often populated
        // by apps that ship no desktop file we can match.
        let icon_name =
            entry.as_ref().and_then(|path| desktop_field(path, "Icon")).unwrap_or_else(|| app_id.to_string());
        let icon = self.load_icon(&icon_name);

        let info = AppInfo { name, icon };
        self.by_app_id.insert(app_id.to_string(), info.clone());
        info
    }

    /// Finds the desktop file for an app_id, cheapest strategy first.
    fn find_desktop_entry(&mut self, app_id: &str) -> Option<PathBuf> {
        // Exact and lowercased basenames cover the overwhelming majority.
        for candidate in [app_id.to_string(), app_id.to_lowercase()] {
            for dir in data_dirs() {
                let path = dir.join("applications").join(format!("{candidate}.desktop"));
                if path.is_file() {
                    return Some(path);
                }
            }
        }

        // Only now pay for the directory walk.
        let index = self.index.get_or_insert_with(build_index);
        index.get(&app_id.to_lowercase()).cloned()
    }

    /// Resolves an `Icon=` value to a pixmap.
    fn load_icon(&self, icon: &str) -> Option<Pixmap> {
        // An absolute path is allowed by the spec and used by some apps.
        let path = if icon.starts_with('/') {
            let path = PathBuf::from(icon);
            path.is_file().then_some(path)?
        } else {
            // Ask for the logical size; the theme may only have a smaller or
            // larger one, which rasterize() scales to fit.
            freedesktop_icons::lookup(icon)
                .with_size(self.size as u16)
                .with_scale(self.scale as u16)
                .with_theme(&self.theme)
                // hicolor is the spec-mandated fallback and is searched
                // automatically, but Adwaita carries far more app icons.
                .with_theme("Adwaita")
                .with_cache()
                .find()?
        };
        rasterize(&path, self.size * self.scale)
    }
}

/// Decodes an icon file to a square pixmap of `size` device pixels.
fn rasterize(path: &Path, size: u32) -> Option<Pixmap> {
    let data = std::fs::read(path).ok()?;

    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg")) {
        let tree = resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default()).ok()?;
        let mut pixmap = Pixmap::new(size, size)?;
        // Uniform scale so a non-square viewBox is letterboxed rather than
        // stretched, then centered.
        let svg_size = tree.size();
        let scale = (size as f32 / svg_size.width()).min(size as f32 / svg_size.height());
        let transform = resvg::tiny_skia::Transform::from_translate(
            (size as f32 - svg_size.width() * scale) / 2.0,
            (size as f32 - svg_size.height() * scale) / 2.0,
        )
        .pre_scale(scale, scale);
        resvg::render(&tree, transform, &mut pixmap.as_mut());
        return Some(pixmap);
    }

    let decoded = image::load_from_memory(&data).ok()?;
    let decoded = decoded
        .resize(size, size, image::imageops::FilterType::CatmullRom)
        .to_rgba8();

    let mut pixmap = Pixmap::new(size, size)?;
    let (w, h) = decoded.dimensions();
    // Center, since resize preserves aspect ratio and may leave one axis short.
    let off_x = (size - w.min(size)) / 2;
    let off_y = (size - h.min(size)) / 2;
    for (x, y, px) in decoded.enumerate_pixels() {
        let [r, g, b, a] = px.0;
        let target = ((y + off_y) * size + (x + off_x)) as usize;
        if let Some(slot) = pixmap.pixels_mut().get_mut(target) {
            // Pixmap holds premultiplied alpha; PNG is straight alpha.
            *slot = tiny_skia::PremultipliedColorU8::from_rgba(
                mul(r, a),
                mul(g, a),
                mul(b, a),
                a,
            )?;
        }
    }
    Some(pixmap)
}

fn mul(channel: u8, alpha: u8) -> u8 {
    ((channel as u16 * alpha as u16) / 255) as u8
}

/// Every directory that may hold an `applications/` subdirectory.
fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Some(home) = std::env::var_os("XDG_DATA_HOME") {
        dirs.push(PathBuf::from(home));
    } else if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share"));
    }

    let system = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    dirs.extend(system.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));

    dirs
}

/// Indexes installed desktop files by basename and by `StartupWMClass`.
///
/// `StartupWMClass` is the field apps use to declare the `app_id` they will
/// report, so it is the one reliable bridge when the basename does not match.
fn build_index() -> HashMap<String, PathBuf> {
    let mut index = HashMap::new();

    for dir in data_dirs() {
        let Ok(entries) = std::fs::read_dir(dir.join("applications")) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.extension().is_some_and(|e| e == "desktop") {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                // Earlier data dirs win, matching XDG precedence.
                index.entry(stem.to_lowercase()).or_insert_with(|| path.clone());
            }
            if let Some(class) = desktop_field(&path, "StartupWMClass") {
                index.entry(class.to_lowercase()).or_insert(path);
            }
        }
    }

    index
}

/// Reads one key from the `[Desktop Entry]` group.
///
/// Localised variants like `Name[de]` are skipped: we want the key exactly, and
/// a prefix match would let `Name[de]` shadow `Name`.
fn desktop_field(path: &Path, key: &str) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    let mut in_entry = false;

    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else { continue };
        if name.trim() == key {
            let value = value.trim();
            return (!value.is_empty()).then(|| value.to_string());
        }
    }
    None
}

/// Last-resort display name: `com.system76.CosmicFiles` -> `CosmicFiles`.
fn prettify(app_id: &str) -> String {
    if app_id.is_empty() {
        return "Unknown".to_string();
    }
    app_id.rsplit('.').next().unwrap_or(app_id).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Writes a desktop file into a fresh temporary directory.
    fn desktop_file(name: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xsw-test-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.desktop"));
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        path
    }

    #[test]
    fn reads_keys_from_the_desktop_entry_group() {
        let path = desktop_file(
            "basic",
            "[Desktop Entry]\nName=Web Browser\nIcon=browser\nStartupWMClass=chromium\n",
        );
        assert_eq!(desktop_field(&path, "Name").as_deref(), Some("Web Browser"));
        assert_eq!(desktop_field(&path, "Icon").as_deref(), Some("browser"));
        assert_eq!(desktop_field(&path, "StartupWMClass").as_deref(), Some("chromium"));
        assert_eq!(desktop_field(&path, "Absent"), None);
    }

    #[test]
    fn ignores_keys_outside_the_desktop_entry_group() {
        // A Desktop Action carries its own Name= that must not win.
        let path = desktop_file(
            "actions",
            "[Desktop Entry]\nName=Real\n\n[Desktop Action new-window]\nName=New Window\n",
        );
        assert_eq!(desktop_field(&path, "Name").as_deref(), Some("Real"));
    }

    #[test]
    fn ignores_localised_variants() {
        // `Name[de]` must not shadow `Name`, which a prefix match would do.
        let path = desktop_file("l10n", "[Desktop Entry]\nName[de]=Netz\nName=Net\n");
        assert_eq!(desktop_field(&path, "Name").as_deref(), Some("Net"));
    }

    #[test]
    fn empty_values_are_treated_as_absent() {
        let path = desktop_file("empty", "[Desktop Entry]\nIcon=\n");
        assert_eq!(desktop_field(&path, "Icon"), None);
    }

    #[test]
    fn missing_file_is_not_an_error() {
        assert_eq!(desktop_field(Path::new("/nonexistent/xsw.desktop"), "Name"), None);
    }

    #[test]
    fn prettify_falls_back_to_the_last_component() {
        assert_eq!(prettify("com.system76.CosmicFiles"), "CosmicFiles");
        assert_eq!(prettify("chromium"), "chromium");
        assert_eq!(prettify(""), "Unknown");
    }
}
