//! Software rendering of the switcher list.
//!
//! Drawn on the CPU into a wl_shm buffer rather than through the GPU: the whole
//! surface is a few hundred kilobytes of rounded rectangles, icons and two
//! lines of text per row, and avoiding GPU setup keeps the launch latency low,
//! which is what actually matters for a switcher you hold a key down to use.

use cosmic_text::{
    Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache, Weight, Wrap,
};
use tiny_skia::{
    BlendMode, Color, FillRule, Paint, PathBuilder, Pixmap, PixmapPaint, Rect, Transform,
};

use crate::config::{Colors, Config, Layout, Rgba};
use crate::icons::AppInfo;

/// One row's text, already resolved from the toplevel and its desktop entry.
pub struct Row<'a> {
    pub app: &'a AppInfo,
    pub title: &'a str,
    pub minimized: bool,
}

/// Holds the font machinery so it is built once per process, not per frame.
pub struct Renderer {
    fonts: FontSystem,
    cache: SwashCache,
    colors: Colors,
    layout: Layout,
    font_family: Option<String>,
    show_titles: bool,
}

impl Renderer {
    pub fn new(config: &Config) -> Self {
        Self {
            fonts: FontSystem::new(),
            cache: SwashCache::new(),
            colors: config.colors,
            layout: config.layout,
            font_family: config.font_family.clone(),
            show_titles: config.show_titles,
        }
    }

    /// Builds attributes from an already-cloned family name.
    ///
    /// Takes the family by argument rather than reading `self.font_family`, so
    /// the returned `Attrs` does not borrow `self` and can be used alongside
    /// the `&mut self` that shaping requires.
    fn attrs(family: Option<&str>, weight: Weight) -> Attrs<'_> {
        let family = match family {
            Some(name) => Family::Name(name),
            None => Family::SansSerif,
        };
        Attrs::new().family(family).weight(weight)
    }

    /// Draws the list into a fresh pixmap sized in device pixels.
    ///
    /// `selected` indexes into `rows`; the caller is responsible for having
    /// already sliced `rows` down to what fits and for keeping the selection
    /// inside that slice.
    pub fn draw(
        &mut self,
        rows: &[Row<'_>],
        selected: usize,
        width: u32,
        height: u32,
        scale: u32,
    ) -> Option<Pixmap> {
        let mut pixmap = Pixmap::new(width, height)?;
        let scale_f = scale as f32;
        let layout = self.layout;

        // Rounded backdrop. The surface itself is transparent outside it, so
        // the corners show whatever is behind the switcher.
        fill_round_rect(
            &mut pixmap,
            0.0,
            0.0,
            width as f32,
            height as f32,
            layout.corner_radius * scale_f,
            self.colors.background,
        );

        let padding = layout.padding * scale;
        let row_height = layout.row_height * scale;
        let icon_px = layout.icon_size * scale;

        for (index, row) in rows.iter().enumerate() {
            let top = padding + row_height * index as u32;
            let is_selected = index == selected;

            if is_selected {
                fill_round_rect(
                    &mut pixmap,
                    padding as f32 / 2.0,
                    top as f32 + 2.0 * scale_f,
                    width as f32 - padding as f32,
                    row_height as f32 - 4.0 * scale_f,
                    layout.row_corner_radius * scale_f,
                    self.colors.selection,
                );
            }

            let icon_x = layout.icon_x() * scale;
            let icon_y = top + (row_height.saturating_sub(icon_px)) / 2;
            if let Some(icon) = row.app.icon.as_ref() {
                pixmap.draw_pixmap(
                    icon_x as i32,
                    icon_y as i32,
                    icon.as_ref(),
                    &PixmapPaint::default(),
                    Transform::identity(),
                    None,
                );
            }

            let text_x = layout.text_x() * scale;
            let text_width = layout.text_width(width / scale.max(1)) * scale;

            let (name_color, title_color) = if is_selected {
                (self.colors.name_selected, self.colors.title_selected)
            } else {
                (self.colors.name, self.colors.title)
            };

            // Application name, then optionally the window title beneath it.
            let name = if row.minimized {
                format!("{} (minimized)", row.app.name)
            } else {
                row.app.name.clone()
            };
            let show_title = self.show_titles && layout.title_size > 0.0;
            let text_top = top as f32 + row_height as f32 / 2.0
                - layout.text_block_height() * scale_f / 2.0;
            self.draw_text(
                &mut pixmap,
                &name,
                text_x as f32,
                text_top,
                text_width,
                layout.name_size * scale_f,
                Weight::SEMIBOLD,
                name_color,
            );
            if show_title {
                self.draw_text(
                    &mut pixmap,
                    row.title,
                    text_x as f32,
                    text_top + (layout.name_size + 5.0) * scale_f,
                    text_width,
                    layout.title_size * scale_f,
                    Weight::NORMAL,
                    title_color,
                );
            }
        }

        Some(pixmap)
    }

    /// Lays out one line of text and blends its coverage into `pixmap`.
    ///
    /// Wrapping is disabled and over-long text is ellipsized: a window title is
    /// frequently wider than the row, and letting it wrap would spill it into
    /// the row below.
    #[allow(clippy::too_many_arguments)]
    fn draw_text(
        &mut self,
        pixmap: &mut Pixmap,
        text: &str,
        x: f32,
        y: f32,
        max_width: u32,
        size: f32,
        weight: Weight,
        color: Rgba,
    ) {
        if text.is_empty() || max_width == 0 {
            return;
        }

        let color = cosmic_text::Color::rgba(color.r, color.g, color.b, color.a);
        let metrics = Metrics::new(size, size * 1.25);
        let mut buffer = Buffer::new(&mut self.fonts, metrics);
        buffer.set_wrap(Wrap::None);
        buffer.set_size(None, Some(metrics.line_height));

        // Cloned so `attrs` borrows this local rather than `self`.
        let family = self.font_family.clone();
        let attrs = Self::attrs(family.as_deref(), weight);
        let fitted = self.fit(&mut buffer, text, &attrs, max_width as f32);
        buffer.set_text(&fitted, &attrs, Shaping::Advanced, None);

        let pixmap_width = pixmap.width();
        let pixmap_height = pixmap.height();
        // Anything past the text column belongs to the next row's padding.
        let clip_right = (x as i32 + max_width as i32).min(pixmap_width as i32);
        let pixels = pixmap.pixels_mut();

        buffer.draw(&mut self.fonts, &mut self.cache, color, |gx, gy, w, h, gcolor| {
            let alpha = gcolor.a();
            if alpha == 0 {
                return;
            }
            for dy in 0..h as i32 {
                for dx in 0..w as i32 {
                    let px = x as i32 + gx + dx;
                    let py = y as i32 + gy + dy;
                    if px < 0 || py < 0 || px >= clip_right || py >= pixmap_height as i32 {
                        continue;
                    }
                    let index = py as usize * pixmap_width as usize + px as usize;
                    if let Some(slot) = pixels.get_mut(index) {
                        *slot = blend(*slot, gcolor.r(), gcolor.g(), gcolor.b(), alpha);
                    }
                }
            }
        });
    }

    /// Shortens `text` with a trailing ellipsis until it fits `max_width`.
    ///
    /// Binary search over character boundaries, re-shaping each candidate:
    /// glyph advances are not uniform, so a proportional estimate from the full
    /// string's width is not reliable enough to cut on.
    fn fit(&mut self, buffer: &mut Buffer, text: &str, attrs: &Attrs, max_width: f32) -> String {
        let mut measure = |buffer: &mut Buffer, candidate: &str| -> f32 {
            buffer.set_text(candidate, attrs, Shaping::Advanced, None);
            buffer.shape_until_scroll(&mut self.fonts, false);
            buffer.layout_runs().map(|run| run.line_w).fold(0.0, f32::max)
        };

        if measure(buffer, text) <= max_width {
            return text.to_string();
        }

        // Cut only on character boundaries, so multi-byte text stays valid.
        let bounds: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
        let byte_offset = |chars: usize| -> usize {
            bounds.get(chars).copied().unwrap_or(text.len())
        };

        let mut low = 0usize;
        let mut high = bounds.len();
        let mut best = String::from("…");

        while low <= high {
            let mid = low + (high - low) / 2;
            let candidate = format!("{}…", text[..byte_offset(mid)].trim_end());
            if measure(buffer, &candidate) <= max_width {
                best = candidate;
                low = mid + 1;
            } else if mid == 0 {
                break;
            } else {
                high = mid - 1;
            }
        }

        best
    }
}

/// Source-over blend of a straight-alpha color onto a premultiplied pixel.
fn blend(
    dst: tiny_skia::PremultipliedColorU8,
    r: u8,
    g: u8,
    b: u8,
    a: u8,
) -> tiny_skia::PremultipliedColorU8 {
    let sa = a as u32;
    let inv = 255 - sa;
    let mix = |src: u8, dst: u8| -> u8 {
        (((src as u32 * sa) + (dst as u32 * inv)) / 255).min(255) as u8
    };
    tiny_skia::PremultipliedColorU8::from_rgba(
        mix(r, dst.red()),
        mix(g, dst.green()),
        mix(b, dst.blue()),
        (sa + (dst.alpha() as u32 * inv) / 255).min(255) as u8,
    )
    .unwrap_or(dst)
}

/// Fills a rounded rectangle, replacing rather than blending so the backdrop's
/// translucency is not compounded by the selection drawn on top of it.
fn fill_round_rect(
    pixmap: &mut Pixmap,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    radius: f32,
    color: Rgba,
) {
    let Some(rect) = Rect::from_xywh(x, y, width, height) else { return };
    let radius = radius.min(width / 2.0).min(height / 2.0);

    let mut builder = PathBuilder::new();
    let (l, t, r, b) = (rect.left(), rect.top(), rect.right(), rect.bottom());
    builder.move_to(l + radius, t);
    builder.line_to(r - radius, t);
    builder.quad_to(r, t, r, t + radius);
    builder.line_to(r, b - radius);
    builder.quad_to(r, b, r - radius, b);
    builder.line_to(l + radius, b);
    builder.quad_to(l, b, l, b - radius);
    builder.line_to(l, t + radius);
    builder.quad_to(l, t, l + radius, t);
    builder.close();
    let Some(path) = builder.finish() else { return };

    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(color.r, color.g, color.b, color.a));
    paint.anti_alias = true;
    // Source, not SourceOver: these rectangles define the surface's own alpha.
    paint.blend_mode = BlendMode::Source;
    pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
}
