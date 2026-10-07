//! Turning an `Icon=` value into a bitmap the renderer can blit.
//!
//! Icons are resolved lazily, only for the rows actually on screen, and cached
//! by name. Rasterizing an SVG costs a millisecond or so, and a launcher
//! showing eight of a few hundred applications should pay for eight of them —
//! decoding the whole list up front would be most of the startup time and
//! nearly all of it wasted.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tiny_skia::Pixmap;

/// Caches lookups, so scrolling back up a list costs nothing.
pub struct IconCache {
    theme: String,
    scale: u32,
    /// Logical edge length to rasterize to, before the output scale.
    size: u32,
    /// `None` for a name that resolved to nothing, so a missing icon is looked
    /// for once rather than on every frame.
    by_name: HashMap<String, Option<Pixmap>>,
}

impl IconCache {
    pub fn new(theme: &str, scale: u32, size: u32) -> Self {
        Self {
            theme: theme.to_string(),
            scale: scale.max(1),
            size: size.max(1),
            by_name: HashMap::new(),
        }
    }

    /// The icon for an `Icon=` value: a theme icon name or an absolute path.
    pub fn get(&mut self, name: &str) -> Option<Pixmap> {
        if let Some(hit) = self.by_name.get(name) {
            return hit.clone();
        }
        let icon = self.load(name);
        self.by_name.insert(name.to_string(), icon.clone());
        icon
    }

    fn load(&self, icon: &str) -> Option<Pixmap> {
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
    let decoded = decoded.resize(size, size, image::imageops::FilterType::CatmullRom).to_rgba8();

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
