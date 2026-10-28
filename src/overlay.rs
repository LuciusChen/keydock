//! The hint overlay: a layer-shell surface drawn with cairo and pango.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use smithay_client_toolkit::compositor::CompositorState;
use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerSurface,
};
use smithay_client_toolkit::shm::{Shm, slot::SlotPool};
use wayland_client::protocol::{wl_shm, wl_surface::WlSurface};
use wayland_client::{Connection, QueueHandle};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};

use crate::{Noop, State};

pub enum Mode {
    /// Super+/: shows pinned apps and gives open, unpinned apps number keys.
    Browse,
    /// An unpinned Super+letter: pick an open app to pin to that letter.
    Assign(char),
}

pub struct Row {
    pub key: char,
    pub name: String,
    pub icon: Option<PathBuf>,
    pub running: bool,
    /// Number keys only work inside the overlay; letters are global.
    pub temporary: bool,
}

pub struct Section {
    pub title: String,
    pub rows: Vec<Row>,
}

pub struct Content {
    pub title: Option<String>,
    pub sections: Vec<Section>,
    pub hint: String,
    /// Shown in a section without rows.
    pub empty: String,
}

#[derive(Clone, Copy)]
struct Rgba(f64, f64, f64, f64);

impl Rgba {
    fn hex(s: &str) -> Option<Rgba> {
        let s = s.trim().trim_start_matches('#');
        let v = |i: usize| {
            u8::from_str_radix(s.get(i..i + 2)?, 16)
                .ok()
                .map(|x| x as f64 / 255.0)
        };
        let a = if s.len() >= 8 { v(6)? } else { 1.0 };
        Some(Rgba(v(0)?, v(2)?, v(4)?, a))
    }
    fn alpha(self, a: f64) -> Rgba {
        Rgba(self.0, self.1, self.2, self.3 * a)
    }
    fn set(self, cr: &cairo::Context) {
        cr.set_source_rgba(self.0, self.1, self.2, self.3);
    }
}

/// Colors written by the Noctalia template, with dark fallbacks.
struct Theme {
    surface: Rgba,
    on_surface: Rgba,
    on_surface_variant: Rgba,
    primary: Rgba,
    outline: Rgba,
}

impl Theme {
    fn load() -> Theme {
        let mut t = Theme {
            surface: Rgba(0.11, 0.11, 0.13, 1.0),
            on_surface: Rgba(0.90, 0.90, 0.92, 1.0),
            on_surface_variant: Rgba(0.62, 0.62, 0.66, 1.0),
            primary: Rgba(0.55, 0.70, 1.0, 1.0),
            outline: Rgba(0.35, 0.35, 0.40, 1.0),
        };
        let path = crate::config::Config::path().with_file_name("colors.toml");
        let Ok(text) = std::fs::read_to_string(path) else {
            return t;
        };
        let Ok(table) = text.parse::<toml::Table>() else {
            return t;
        };
        let get = |k: &str| table.get(k).and_then(|v| v.as_str()).and_then(Rgba::hex);
        for (k, slot) in [
            ("surface", &mut t.surface),
            ("on_surface", &mut t.on_surface),
            ("on_surface_variant", &mut t.on_surface_variant),
            ("primary", &mut t.primary),
            ("outline", &mut t.outline),
        ] {
            if let Some(c) = get(k) {
                *slot = c;
            }
        }
        t
    }
}

// Logical-pixel metrics.
const PAD: f64 = 16.0;
const ROW_H: f64 = 34.0;
const BADGE: f64 = 24.0;
const ICON: f64 = 22.0;
const GAP: f64 = 10.0;
const SECTION_H: f64 = 26.0;
const TITLE_H: f64 = 30.0;
const HINT_H: f64 = 26.0;
const MIN_W: f64 = 300.0;
const MAX_NAME_W: f64 = 360.0;

/// Globals the overlay needs; fractional scaling is optional.
pub struct Globals {
    pub compositor: CompositorState,
    pub layer_shell: LayerShell,
    pub fractional: Option<WpFractionalScaleManagerV1>,
    pub viewporter: Option<WpViewporter>,
}

pub struct Overlay {
    globals: Globals,
    pub mode: Option<Mode>,
    content: Option<Content>,
    layer: Option<LayerSurface>,
    fractional: Option<WpFractionalScaleV1>,
    viewport: Option<WpViewport>,
    /// Preferred scale in 1/120 units; kept across openings as a first guess.
    scale120: u32,
    size: (u32, u32),
    configured: bool,
    pool: Option<SlotPool>,
    icons: HashMap<(PathBuf, i32), Option<cairo::ImageSurface>>,
    font: String,
    theme: Theme,
}

impl Overlay {
    pub fn new(globals: Globals) -> Self {
        Overlay {
            globals,
            mode: None,
            content: None,
            layer: None,
            fractional: None,
            viewport: None,
            scale120: 120,
            size: (0, 0),
            configured: false,
            pool: None,
            icons: HashMap::new(),
            font: "sans".into(),
            theme: Theme::load(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.layer.is_some()
    }

    pub fn owns(&self, surface: &WlSurface) -> bool {
        self.layer
            .as_ref()
            .is_some_and(|l| l.wl_surface() == surface)
    }

    /// Shows `content`, or replaces what an open overlay shows.
    pub fn open(
        &mut self,
        qh: &QueueHandle<State>,
        mode: Mode,
        content: Content,
        font: Option<&str>,
    ) {
        self.theme = Theme::load();
        self.font = font.unwrap_or("sans").to_string();
        let (w, h) = self.measure(&content);
        self.size = (w, h);
        self.content = Some(content);
        self.mode = Some(mode);
        if let Some(layer) = &self.layer {
            layer.set_size(w, h);
            layer.commit();
            return;
        }
        let g = &self.globals;
        let surface = g.compositor.create_surface(qh);
        let layer =
            g.layer_shell
                .create_layer_surface(qh, surface, Layer::Overlay, Some("keydock"), None);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        layer.set_anchor(Anchor::empty());
        layer.set_size(w, h);
        if let (Some(f), Some(v)) = (&g.fractional, &g.viewporter) {
            self.fractional = Some(f.get_fractional_scale(layer.wl_surface(), qh, Scale));
            self.viewport = Some(v.get_viewport(layer.wl_surface(), qh, Noop));
        }
        layer.commit();
        self.layer = Some(layer);
        self.configured = false;
    }

    pub fn close(&mut self) {
        if let Some(f) = self.fractional.take() {
            f.destroy();
        }
        if let Some(v) = self.viewport.take() {
            v.destroy();
        }
        self.layer = None; // dropping destroys the layer surface
        self.mode = None;
        self.content = None;
    }

    pub fn configured(&mut self, shm: &Shm) {
        self.configured = true;
        self.draw(shm);
    }

    pub fn set_scale(&mut self, scale120: u32, shm: &Shm) {
        if scale120 != self.scale120 {
            self.scale120 = scale120;
            if self.configured {
                self.draw(shm);
            }
        }
    }

    fn measure(&self, content: &Content) -> (u32, u32) {
        let surface =
            cairo::ImageSurface::create(cairo::Format::ARgb32, 1, 1).expect("cairo surface");
        let cr = cairo::Context::new(&surface).expect("cairo context");
        let text_w = |t: &str, px: f64, bold: bool| {
            text_layout(&cr, &self.font, t, px, bold).pixel_size().0 as f64
        };
        let mut name_w: f64 = 0.0;
        let mut h = PAD * 2.0 + HINT_H;
        let mut other_w = text_w(&content.hint, 11.5, false);
        if let Some(title) = &content.title {
            h += TITLE_H;
            other_w = other_w.max(text_w(title, 15.0, true));
        }
        for s in &content.sections {
            h += SECTION_H + ROW_H * s.rows.len().max(1) as f64;
            for r in &s.rows {
                name_w = name_w.max(text_w(&r.name, 13.5, false).min(MAX_NAME_W));
            }
        }
        let row_w = BADGE + GAP + ICON + GAP + name_w + GAP * 2.0 + 6.0;
        let w = (PAD * 2.0 + row_w.max(other_w)).max(MIN_W);
        (w.ceil() as u32, h.ceil() as u32)
    }

    fn draw(&mut self, shm: &Shm) {
        let Some(content) = self.content.as_ref() else {
            return;
        };
        if self.layer.is_none() {
            return;
        }
        let (w, h) = self.size;
        let scale = self.scale120 as f64 / 120.0;
        // Without viewporter, fall back to integer buffer scaling.
        let scale = if self.viewport.is_some() {
            scale
        } else {
            scale.ceil()
        };
        let (bw, bh) = (
            (w as f64 * scale).ceil() as i32,
            (h as f64 * scale).ceil() as i32,
        );
        let stride = bw * 4;

        let px = (scale * ICON).round() as i32;
        let paths: Vec<Option<PathBuf>> = content
            .sections
            .iter()
            .flat_map(|s| s.rows.iter())
            .map(|r| r.icon.clone())
            .collect();
        let icons: Vec<Option<cairo::ImageSurface>> = paths
            .into_iter()
            .map(|p| p.and_then(|p| self.icon(&p, px)))
            .collect();

        let image =
            cairo::ImageSurface::create(cairo::Format::ARgb32, bw, bh).expect("cairo surface");
        {
            let cr = cairo::Context::new(&image).expect("cairo context");
            cr.scale(scale, scale);
            let content = self.content.as_ref().expect("content");
            paint(
                &cr,
                &self.theme,
                &self.font,
                content,
                (w as f64, h as f64),
                &icons,
                scale,
            );
        }
        image.flush();
        let src_stride = image.stride() as usize;
        let data = image.take_data().expect("image data");

        let pool = match &mut self.pool {
            Some(pool) => pool,
            None => self
                .pool
                .insert(SlotPool::new((stride * bh) as usize, shm).expect("shm pool")),
        };
        let Ok((buffer, canvas)) = pool.create_buffer(bw, bh, stride, wl_shm::Format::Argb8888)
        else {
            return;
        };
        for y in 0..bh as usize {
            let (s, d) = (y * src_stride, y * stride as usize);
            canvas[d..d + stride as usize].copy_from_slice(&data[s..s + stride as usize]);
        }
        let layer = self.layer.as_ref().expect("layer");
        let surface = layer.wl_surface();
        if let Some(v) = &self.viewport {
            v.set_destination(w as i32, h as i32);
        } else {
            surface.set_buffer_scale(scale as i32);
        }
        surface.damage_buffer(0, 0, bw, bh);
        if buffer.attach_to(surface).is_ok() {
            layer.commit();
        }
    }

    fn icon(&mut self, path: &Path, px: i32) -> Option<cairo::ImageSurface> {
        self.icons
            .entry((path.to_path_buf(), px))
            .or_insert_with(|| render_icon(path, px))
            .clone()
    }
}

fn paint(
    cr: &cairo::Context,
    t: &Theme,
    font: &str,
    c: &Content,
    (w, h): (f64, f64),
    icons: &[Option<cairo::ImageSurface>],
    scale: f64,
) {
    let layout = |text: &str, px: f64, bold: bool| text_layout(cr, font, text, px, bold);
    // Card.
    rounded(cr, 0.5, 0.5, w - 1.0, h - 1.0, 16.0);
    t.surface.set(cr);
    cr.fill_preserve().ok();
    t.outline.alpha(0.35).set(cr);
    cr.set_line_width(1.0);
    cr.stroke().ok();

    let mut y = PAD;
    let text = |l: &pango::Layout, x: f64, y: f64, color: Rgba| {
        color.set(cr);
        cr.move_to(x, y);
        pangocairo::functions::show_layout(cr, l);
    };
    if let Some(title) = &c.title {
        let l = layout(title, 15.0, true);
        text(&l, PAD, y + 2.0, t.on_surface);
        y += TITLE_H;
    }
    let mut icon_i = 0;
    for s in &c.sections {
        let l = layout(&s.title, 11.5, true);
        text(&l, PAD + 2.0, y + 6.0, t.on_surface_variant);
        y += SECTION_H;
        if s.rows.is_empty() {
            let l = layout(&c.empty, 13.0, false);
            text(
                &l,
                PAD + 2.0,
                y + (ROW_H - l.pixel_size().1 as f64) / 2.0,
                t.on_surface_variant.alpha(0.7),
            );
            y += ROW_H;
        }
        for r in &s.rows {
            let mid = y + ROW_H / 2.0;
            // Key badge: accent for pinned letters, neutral for temporary numbers.
            rounded(cr, PAD, mid - BADGE / 2.0, BADGE, BADGE, 7.0);
            if r.temporary {
                t.on_surface.alpha(0.10)
            } else {
                t.primary.alpha(0.20)
            }
            .set(cr);
            cr.fill().ok();
            let key = r.key.to_ascii_uppercase().to_string();
            let l = layout(&key, 13.0, true);
            let (kw, kh) = l.pixel_size();
            text(
                &l,
                PAD + (BADGE - kw as f64) / 2.0,
                mid - kh as f64 / 2.0,
                if r.temporary { t.on_surface } else { t.primary },
            );

            let x_icon = PAD + BADGE + GAP;
            if let Some(Some(img)) = icons.get(icon_i) {
                cr.save().ok();
                cr.translate(x_icon, mid - ICON / 2.0);
                cr.scale(1.0 / scale, 1.0 / scale);
                cr.set_source_surface(img, 0.0, 0.0).ok();
                cr.paint_with_alpha(if r.running { 1.0 } else { 0.55 }).ok();
                cr.restore().ok();
            }
            icon_i += 1;

            let l = layout(&r.name, 13.5, false);
            l.set_width((MAX_NAME_W * pango::SCALE as f64) as i32);
            l.set_ellipsize(pango::EllipsizeMode::End);
            let nh = l.pixel_size().1 as f64;
            let color = if r.running {
                t.on_surface
            } else {
                t.on_surface_variant.alpha(0.8)
            };
            text(&l, x_icon + ICON + GAP, mid - nh / 2.0, color);

            if r.running {
                t.primary.set(cr);
                cr.arc(w - PAD - 3.0, mid, 3.0, 0.0, std::f64::consts::TAU);
                cr.fill().ok();
            }
            y += ROW_H;
        }
    }
    let l = layout(&c.hint, 11.5, false);
    text(&l, PAD + 2.0, y + 8.0, t.on_surface_variant.alpha(0.85));
}

fn text_layout(cr: &cairo::Context, font: &str, text: &str, px: f64, bold: bool) -> pango::Layout {
    let layout = pangocairo::functions::create_layout(cr);
    let mut fd = pango::FontDescription::from_string(font);
    fd.set_absolute_size(px * pango::SCALE as f64);
    if bold {
        fd.set_weight(pango::Weight::Bold);
    }
    layout.set_font_description(Some(&fd));
    layout.set_text(text);
    layout
}

fn rounded(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::FRAC_PI_2 as Q;
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -Q, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, Q);
    cr.arc(x + r, y + h - r, r, Q, 2.0 * Q);
    cr.arc(x + r, y + r, r, 2.0 * Q, 3.0 * Q);
    cr.close_path();
}

/// Renders an SVG or PNG icon to a premultiplied ARGB32 cairo surface.
fn render_icon(path: &Path, px: i32) -> Option<cairo::ImageSurface> {
    use resvg::{tiny_skia, usvg};
    let data = std::fs::read(path).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(px as u32, px as u32)?;
    if path.extension().is_some_and(|e| e == "png") {
        let src = tiny_skia::Pixmap::decode_png(&data).ok()?;
        let s = px as f32 / src.width().max(src.height()) as f32;
        pixmap.draw_pixmap(
            0,
            0,
            src.as_ref(),
            &tiny_skia::PixmapPaint {
                quality: tiny_skia::FilterQuality::Bicubic,
                ..Default::default()
            },
            tiny_skia::Transform::from_scale(s, s),
            None,
        );
    } else {
        let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).ok()?;
        let size = tree.size();
        let s = px as f32 / size.width().max(size.height());
        resvg::render(
            &tree,
            tiny_skia::Transform::from_scale(s, s),
            &mut pixmap.as_mut(),
        );
    }
    // tiny-skia is premultiplied RGBA; cairo ARGB32 is premultiplied BGRA in memory.
    let mut bytes = pixmap.take();
    for p in bytes.as_chunks_mut::<4>().0 {
        p.swap(0, 2);
    }
    cairo::ImageSurface::create_for_data(bytes, cairo::Format::ARgb32, px, px, px * 4).ok()
}

/// User data for the overlay's fractional-scale object.
pub struct Scale;

impl Dispatch2<WpFractionalScaleV1, State> for Scale {
    fn event(
        &self,
        state: &mut State,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            state.overlay.set_scale(scale, &state.shm);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_colors() {
        let rgba = |c: Rgba| (c.0, c.1, c.2, c.3);
        assert_eq!(rgba(Rgba::hex("#ff0000").unwrap()), (1.0, 0.0, 0.0, 1.0));
        assert_eq!(
            rgba(Rgba::hex("00ff0080").unwrap()),
            (0.0, 1.0, 0.0, 128.0 / 255.0)
        );
        assert!(Rgba::hex("#fff").is_none());
        assert!(Rgba::hex("not a color").is_none());
    }
}
