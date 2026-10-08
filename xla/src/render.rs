//! Software rendering of the launcher panel.
//!
//! Drawn on the CPU into a wl_shm buffer rather than through the GPU: the whole
//! surface is a few hundred kilobytes of rounded rectangles, icons and two
//! lines of text per row, and avoiding GPU setup keeps the launch latency low,
//! which is what actually matters for something you summon with a keystroke.
//!
//! This is xsw's renderer with a query row added above the list, and the two
//! draw the same panel: same backdrop, same row metrics, same selection
//! highlight.

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache, Weight, Wrap};
use tiny_skia::{
    BlendMode, Color, FillRule, LineCap, Paint, PathBuilder, Pixmap, PixmapPaint, Rect, Stroke,
    Transform,
};

use crate::config::{Colors, Config, Layout, Rgba};

/// Shown in place of the list when the query matches nothing. An empty panel
/// looks like a launcher that has broken rather than one that has understood.
const NO_MATCHES: &str = "No matching applications";

/// One row, already resolved from a desktop entry.
pub struct Row<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub icon: Option<&'a Pixmap>,
}

/// Holds the font machinery so it is built once per process, not per frame.
pub struct Renderer {
    fonts: FontSystem,
    cache: SwashCache,
    colors: Colors,
    layout: Layout,
    font_family: Option<String>,
    show_descriptions: bool,
    placeholder: String,
}

impl Renderer {
    pub fn new(config: &Config) -> Self {
        Self {
            fonts: FontSystem::new(),
            cache: SwashCache::new(),
            colors: config.colors,
            layout: config.layout,
            font_family: config.font_family.clone(),
            show_descriptions: config.show_descriptions,
            placeholder: config.placeholder.clone(),
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

    /// Draws the panel into a fresh pixmap sized in device pixels.
    ///
    /// `selected` indexes into `rows`; the caller is responsible for having
    /// already sliced `rows` down to what fits and for keeping the selection
    /// inside that slice. An empty `rows` draws the "no matches" line instead.
    pub fn draw(
        &mut self,
        query: &str,
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
        // the corners show whatever is behind the launcher.
        fill_round_rect(
            &mut pixmap,
            0.0,
            0.0,
            width as f32,
            height as f32,
            layout.corner_radius * scale_f,
            self.colors.background,
            // Source, not SourceOver: this rectangle defines the surface's own
            // alpha rather than blending onto anything.
            BlendMode::Source,
        );

        let padding = layout.padding * scale;
        let row_height = layout.row_height * scale;
        let icon_px = layout.icon_size * scale;
        let icon_x = layout.icon_x() * scale;
        let text_x = layout.text_x() * scale;
        // Logical width is what the config was validated against, so scale the
        // answer rather than the inputs.
        let text_width = layout.text_width(width / scale.max(1)) * scale;

        self.draw_query(&mut pixmap, query, padding, icon_x, icon_px, text_x, text_width, scale);

        // Hairline under the query row, in the row area's top pixel.
        let divider_y = layout.rows_top() * scale;
        fill_round_rect(
            &mut pixmap,
            padding as f32,
            divider_y as f32 - scale_f,
            (width - padding * 2) as f32,
            scale_f,
            0.0,
            self.colors.divider,
            // Over, so a translucent divider tints the backdrop instead of
            // punching a hole through to the desktop.
            BlendMode::SourceOver,
        );

        if rows.is_empty() {
            let top = divider_y as f32 + row_height as f32 / 2.0
                - Layout::line_height(layout.name_size) * scale_f / 2.0;
            self.draw_text(
                &mut pixmap,
                NO_MATCHES,
                text_x as f32,
                top,
                text_width,
                layout.name_size * scale_f,
                Weight::NORMAL,
                self.colors.placeholder,
            );
            return Some(pixmap);
        }

        for (index, row) in rows.iter().enumerate() {
            let top = divider_y + row_height * index as u32;
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
                    BlendMode::Source,
                );
            }

            let icon_y = top + (row_height.saturating_sub(icon_px)) / 2;
            if let Some(icon) = row.icon {
                pixmap.draw_pixmap(
                    icon_x as i32,
                    icon_y as i32,
                    icon.as_ref(),
                    &PixmapPaint::default(),
                    Transform::identity(),
                    None,
                );
            }

            let (name_color, description_color) = if is_selected {
                (self.colors.name_selected, self.colors.description_selected)
            } else {
                (self.colors.name, self.colors.description)
            };

            let show_description = self.show_descriptions
                && layout.description_size > 0.0
                && !row.description.is_empty();
            // Centre the block of one or two lines in the row.
            let block = if show_description {
                layout.text_block_height()
            } else {
                layout.name_size
            };
            let text_top = top as f32 + row_height as f32 / 2.0 - block * scale_f / 2.0;

            self.draw_text(
                &mut pixmap,
                row.name,
                text_x as f32,
                text_top,
                text_width,
                layout.name_size * scale_f,
                Weight::SEMIBOLD,
                name_color,
            );
            if show_description {
                self.draw_text(
                    &mut pixmap,
                    row.description,
                    text_x as f32,
                    text_top + (layout.name_size + 5.0) * scale_f,
                    text_width,
                    layout.description_size * scale_f,
                    Weight::NORMAL,
                    description_color,
                );
            }
        }

        Some(pixmap)
    }

    /// Draws the query row: the search glyph, the text, and the caret.
    #[allow(clippy::too_many_arguments)]
    fn draw_query(
        &mut self,
        pixmap: &mut Pixmap,
        query: &str,
        padding: u32,
        icon_x: u32,
        icon_px: u32,
        text_x: u32,
        text_width: u32,
        scale: u32,
    ) {
        let scale_f = scale as f32;
        let input_height = self.layout.input_height * scale;
        let size = self.layout.input_size * scale_f;
        let middle = padding as f32 + input_height as f32 / 2.0;

        // The glyph dims until something is typed, so the row reads as a
        // prompt rather than as a result with a blank name.
        let color = if query.is_empty() { self.colors.placeholder } else { self.colors.query };
        draw_search_glyph(
            pixmap,
            icon_x as f32 + icon_px as f32 / 2.0,
            middle,
            icon_px as f32,
            color,
        );

        let caret_width = (1.5 * scale_f).max(1.0);
        // With something typed, the text starts where the names below it do and
        // the caret follows it. With nothing typed the caret is what comes
        // first, and the placeholder steps aside for it rather than being
        // drawn underneath.
        let (text, caret_x, indent) = if query.is_empty() {
            (self.placeholder.clone(), text_x as f32, caret_width + 3.0 * scale_f)
        } else {
            // The tail is what matters in a query: it is where the caret is and
            // where the character just typed went.
            let shown = self.fit_tail(query, size, text_width as f32);
            let caret_x =
                text_x as f32 + self.measure(&shown, size, Weight::NORMAL) + 2.0 * scale_f;
            (shown, caret_x, 0.0)
        };

        self.draw_text(
            pixmap,
            &text,
            text_x as f32 + indent,
            middle - Layout::line_height(size) / 2.0,
            text_width - indent as u32,
            size,
            Weight::NORMAL,
            color,
        );

        // Caret. Not animated: nothing else here is, and a blink would mean
        // keeping the process awake to redraw it.
        if caret_x < (text_x + text_width) as f32 {
            fill_round_rect(
                pixmap,
                caret_x,
                middle - size * 0.55,
                caret_width,
                size * 1.1,
                0.0,
                self.colors.query,
                BlendMode::SourceOver,
            );
        }
    }

    /// Lays out one line of text and blends its coverage into `pixmap`.
    ///
    /// Wrapping is disabled and over-long text is ellipsized: a description is
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
        let metrics = Metrics::new(size, Layout::line_height(size));
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
        // Anything past the text column belongs to the panel's padding.
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

    /// Width of `text` in device pixels, once shaped.
    fn measure(&mut self, text: &str, size: f32, weight: Weight) -> f32 {
        let metrics = Metrics::new(size, Layout::line_height(size));
        let mut buffer = Buffer::new(&mut self.fonts, metrics);
        buffer.set_wrap(Wrap::None);
        buffer.set_size(None, Some(metrics.line_height));
        let family = self.font_family.clone();
        let attrs = Self::attrs(family.as_deref(), weight);
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.fonts, false);
        buffer.layout_runs().map(|run| run.line_w).fold(0.0, f32::max)
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
        let byte_offset = |chars: usize| -> usize { bounds.get(chars).copied().unwrap_or(text.len()) };

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

    /// Drops characters off the *front* of `text` until it fits, marking the
    /// cut with a leading ellipsis.
    ///
    /// The query row needs the opposite of [`Renderer::fit`]: trimming the end
    /// of a long query would hide the character just typed and the caret with
    /// it.
    fn fit_tail(&mut self, text: &str, size: f32, max_width: f32) -> String {
        if text.is_empty() || self.measure(text, size, Weight::NORMAL) <= max_width {
            return text.to_string();
        }

        // Linear from the front rather than a binary search: getting here needs
        // a query longer than the panel is wide, which happens a character at a
        // time, so all but the first cut is one step.
        for (offset, _) in text.char_indices().skip(1) {
            let candidate = format!("…{}", &text[offset..]);
            if self.measure(&candidate, size, Weight::NORMAL) <= max_width {
                return candidate;
            }
        }
        String::from("…")
    }
}

/// Draws a magnifying glass, centered on `(x, y)` and sized to fit a box of
/// `size` device pixels.
///
/// Stroked out of two paths rather than taken from an icon theme: it has to
/// line up with the rows below at any scale, and the themes do not agree on
/// what a search icon is called.
fn draw_search_glyph(pixmap: &mut Pixmap, x: f32, y: f32, size: f32, color: Rgba) {
    let radius = size * 0.3;
    // Nudged up and left so the handle's diagonal is what fills the rest of
    // the box, which keeps the whole glyph optically centered.
    let (cx, cy) = (x - size * 0.08, y - size * 0.08);

    let mut builder = PathBuilder::new();
    builder.push_circle(cx, cy, radius);
    let edge = radius * std::f32::consts::FRAC_1_SQRT_2;
    builder.move_to(cx + edge, cy + edge);
    builder.line_to(x + size * 0.42, y + size * 0.42);
    let Some(path) = builder.finish() else { return };

    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(color.r, color.g, color.b, color.a));
    paint.anti_alias = true;
    let stroke = Stroke {
        width: (size * 0.075).max(1.0),
        line_cap: LineCap::Round,
        ..Stroke::default()
    };
    pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
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
    let mix =
        |src: u8, dst: u8| -> u8 { (((src as u32 * sa) + (dst as u32 * inv)) / 255).min(255) as u8 };
    tiny_skia::PremultipliedColorU8::from_rgba(
        mix(r, dst.red()),
        mix(g, dst.green()),
        mix(b, dst.blue()),
        (sa + (dst.alpha() as u32 * inv) / 255).min(255) as u8,
    )
    .unwrap_or(dst)
}

/// Fills a rounded rectangle.
///
/// `blend` is [`BlendMode::Source`] for the panel and the selected row, which
/// define the surface's own alpha and must not compound the backdrop's
/// translucency, and [`BlendMode::SourceOver`] for anything drawn on top of
/// them.
#[allow(clippy::too_many_arguments)]
fn fill_round_rect(
    pixmap: &mut Pixmap,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    radius: f32,
    color: Rgba,
    blend: BlendMode,
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
    paint.blend_mode = blend;
    pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Renders a panel at the default metrics, as `ui.rs` would.
    fn panel(query: &str, rows: &[Row<'_>], selected: usize) -> (Pixmap, Config) {
        let config = Config::default();
        let mut renderer = Renderer::new(&config);
        let width = config.width;
        let height = config.layout.height_for(rows.len().max(1));
        let pixmap = renderer.draw(query, rows, selected, width, height, 1).expect("drawn");
        (pixmap, config)
    }

    fn row<'a>(name: &'a str, description: &'a str) -> Row<'a> {
        Row { name, description, icon: None }
    }

    /// Whether a pixel is the panel's own backdrop rather than anything drawn
    /// on top of it.
    fn is_background(pixmap: &Pixmap, x: u32, y: u32, colors: &Colors) -> bool {
        let px = pixmap.pixel(x, y).expect("inside the panel");
        // The backdrop is filled with BlendMode::Source, so it is stored
        // premultiplied by its own alpha.
        let expect = |channel: u8| (channel as u32 * colors.background.a as u32 / 255) as u8;
        px.alpha() == colors.background.a
            && px.red().abs_diff(expect(colors.background.r)) <= 1
            && px.green().abs_diff(expect(colors.background.g)) <= 1
            && px.blue().abs_diff(expect(colors.background.b)) <= 1
    }

    #[test]
    fn the_panel_is_the_size_it_was_asked_for_and_has_rounded_corners() {
        let (pixmap, config) = panel("", &[row("Files", "File manager")], 0);
        assert_eq!(pixmap.width(), config.width);
        assert_eq!(pixmap.height(), config.layout.height_for(1));

        // The very corner is outside the rounded backdrop, so it shows
        // whatever is behind the launcher.
        assert_eq!(pixmap.pixel(0, 0).unwrap().alpha(), 0, "top left corner is transparent");
        let (w, h) = (pixmap.width(), pixmap.height());
        assert_eq!(pixmap.pixel(w - 1, h - 1).unwrap().alpha(), 0, "bottom right too");
        // Just inside it is the backdrop.
        assert!(is_background(&pixmap, w / 2, 2, &config.colors));
    }

    #[test]
    fn the_selected_row_is_highlighted_and_the_others_are_not() {
        let rows = [row("Alpha", "First"), row("Beta", "Second")];
        let (pixmap, config) = panel("", &rows, 1);
        let layout = config.layout;

        // Sampled in the gap between the icon column and the text, which no
        // glyph or icon reaches, so only the row background is there.
        let x = layout.text_x() - layout.icon_gap / 2;
        let unselected = layout.rows_top() + layout.row_height / 2;
        let selected = unselected + layout.row_height;

        assert!(is_background(&pixmap, x, unselected, &config.colors), "row 0 is plain");
        assert!(!is_background(&pixmap, x, selected, &config.colors), "row 1 is highlighted");
        assert_eq!(
            pixmap.pixel(x, selected).unwrap().alpha(),
            config.colors.selection.a,
            "the highlight replaces the backdrop's alpha rather than compounding it"
        );
    }

    #[test]
    fn text_is_drawn_in_the_text_column() {
        let query_only = panel("firefox", &[], 0).0;
        let (pixmap, config) = panel("", &[row("Firefox", "Web browser")], 0);

        // Something non-background in the name's band means glyphs landed.
        let band = |pixmap: &Pixmap, top: u32, bottom: u32| {
            (config.layout.text_x()..config.width - config.layout.padding * 2)
                .any(|x| (top..bottom).any(|y| !is_background(pixmap, x, y, &config.colors)))
        };
        assert!(band(&pixmap, config.layout.rows_top(), config.layout.height_for(1)), "row text");
        assert!(band(&query_only, config.layout.padding, config.layout.rows_top()), "query text");
    }

    #[test]
    fn an_empty_result_set_still_draws_a_row() {
        // A panel that collapsed to the query row would read as broken.
        let (pixmap, config) = panel("zzzz", &[], 0);
        assert_eq!(pixmap.height(), config.layout.height_for(1));
    }

    #[test]
    fn a_long_query_keeps_its_tail_visible() {
        let config = Config::default();
        let mut renderer = Renderer::new(&config);
        let width = config.layout.text_width(config.width) as f32;
        let long = "a".repeat(400);

        let shown = renderer.fit_tail(&long, config.layout.input_size, width);
        assert!(shown.starts_with('…'), "cut from the front: {shown}");
        assert!(shown.len() < long.len());
        assert!(renderer.measure(&shown, config.layout.input_size, Weight::NORMAL) <= width);
        // Short queries are left alone.
        assert_eq!(renderer.fit_tail("firefox", config.layout.input_size, width), "firefox");
    }
}
