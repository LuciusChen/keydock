//! The hint overlay: a layer-shell surface drawn with cairo and pango.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use smithay_client_toolkit::compositor::{CompositorState, Region};
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

use crate::config::Config;
use crate::keyboard::{self, Keyboard};
use crate::{Noop, State};

pub enum Mode {
    /// Super+/: shows pinned apps and gives open, unpinned apps number keys.
    Browse,
    /// An unpinned Super+letter: pick an open app to pin to that letter.
    Assign(char),
    /// The trigger key is held: show the keyboard without taking key input.
    /// `numbered` says whether its number keys are the open apps, even if none.
    Peek { numbered: bool },
}

#[derive(Clone, Copy)]
pub struct Rgba(f64, f64, f64, f64);

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
    pub fn alpha(self, a: f64) -> Rgba {
        Rgba(self.0, self.1, self.2, self.3 * a)
    }
    pub fn set(self, cr: &cairo::Context) {
        cr.set_source_rgba(self.0, self.1, self.2, self.3);
    }
}

/// Colors written by the Noctalia template, with dark fallbacks.
pub struct Theme {
    pub surface: Rgba,
    pub on_surface: Rgba,
    pub primary: Rgba,
    pub outline: Rgba,
}

impl Theme {
    fn load() -> Theme {
        let mut t = Theme {
            surface: Rgba(0.11, 0.11, 0.13, 1.0),
            on_surface: Rgba(0.90, 0.90, 0.92, 1.0),
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
    keyboard: Option<Keyboard>,
    layer: Option<LayerSurface>,
    fractional: Option<WpFractionalScaleV1>,
    viewport: Option<WpViewport>,
    /// Preferred scale in 1/120 units; kept across openings as a first guess.
    scale120: u32,
    size: (u32, u32),
    /// Shrinks the drawing where the compositor offers less than `size`.
    fit: f64,
    configured: bool,
    /// Compositors may repeat a configure; only new content needs drawing.
    stale: bool,
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
            keyboard: None,
            layer: None,
            fractional: None,
            viewport: None,
            scale120: 120,
            size: (0, 0),
            fit: 1.0,
            configured: false,
            stale: false,
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

    /// Shows `keyboard` in place of whatever the overlay shows, asking for no
    /// more than `limit`, as wlroots compositors grant whatever is asked.
    pub fn open(
        &mut self,
        qh: &QueueHandle<State>,
        mode: Mode,
        keyboard: Keyboard,
        limit: (u32, u32),
        cfg: &Config,
    ) {
        self.close();
        self.theme = Theme::load();
        self.font = cfg.font.as_deref().unwrap_or("sans").to_string();
        let (w, h) = keyboard.size();
        let (w, h) = (w.ceil() as u32, h.ceil() as u32);
        // A peek only shows; keys keep going to the focused window.
        let input = match mode {
            Mode::Peek { .. } => KeyboardInteractivity::None,
            _ => KeyboardInteractivity::Exclusive,
        };
        self.size = (w, h);
        self.fit = 1.0;
        self.keyboard = Some(keyboard);
        self.stale = true;
        self.mode = Some(mode);
        let g = &self.globals;
        let surface = g.compositor.create_surface(qh);
        let layer =
            g.layer_shell
                .create_layer_surface(qh, surface, Layer::Overlay, Some("keydock"), None);
        layer.set_keyboard_interactivity(input);
        // A peek lets clicks through as well as keys.
        if matches!(self.mode, Some(Mode::Peek { .. }))
            && let Ok(region) = Region::new(&g.compositor)
        {
            layer
                .wl_surface()
                .set_input_region(Some(region.wl_region()));
        }
        layer.set_anchor(Anchor::empty());
        let fit = fit_within((w, h), limit);
        // A zero size would be a protocol error without anchors to opposite edges.
        let asked = |n: u32| (n as f64 * fit).max(1.0) as u32;
        layer.set_size(asked(w), asked(h));
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
        self.keyboard = None;
    }

    /// `bounds` is the size the compositor configured; 0 leaves it to keydock.
    pub fn configured(&mut self, bounds: (u32, u32), shm: &Shm) {
        self.configured = true;
        let fit = fit_within(self.size, bounds);
        if fit != self.fit {
            self.fit = fit;
            self.stale = true;
        }
        if self.stale {
            self.draw(shm);
        }
    }

    pub fn set_scale(&mut self, scale120: u32, shm: &Shm) {
        if scale120 != self.scale120 {
            self.scale120 = scale120;
            if self.configured {
                self.draw(shm);
            }
        }
    }

    fn draw(&mut self, shm: &Shm) {
        self.stale = false;
        let Some(keyboard) = self.keyboard.as_ref() else {
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
        let shown = |n: u32| (n as f64 * self.fit).floor().max(1.0) as i32;
        let (shown_w, shown_h) = (shown(w), shown(h));
        let (bw, bh) = (
            (shown_w as f64 * scale).ceil() as i32,
            (shown_h as f64 * scale).ceil() as i32,
        );
        // Everything inside is drawn at its full size and scaled to fit.
        let scale_in = scale * self.fit;
        let stride = bw * 4;

        let paths = keyboard.icon_paths();
        let px = (scale_in * keyboard::ICON).round() as i32;
        let icons: Vec<Option<cairo::ImageSurface>> = paths
            .into_iter()
            .map(|p| p.and_then(|p| self.icon(&p, px)))
            .collect();

        let image =
            cairo::ImageSurface::create(cairo::Format::ARgb32, bw, bh).expect("cairo surface");
        {
            let cr = cairo::Context::new(&image).expect("cairo context");
            cr.scale(scale_in, scale_in);
            card(&cr, &self.theme, (w as f64, h as f64));
            let keyboard = self.keyboard.as_ref().expect("keyboard");
            keyboard::paint(&cr, &self.theme, &self.font, keyboard, &icons, scale_in);
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
            v.set_destination(shown_w, shown_h);
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

/// How far to shrink `size` to fit `bounds`, where a 0 bound is no limit.
fn fit_within(size: (u32, u32), bounds: (u32, u32)) -> f64 {
    let ratio = |n: u32, bound: u32| match bound {
        0 => 1.0,
        _ => (bound as f64 / n as f64).min(1.0),
    };
    ratio(size.0, bounds.0).min(ratio(size.1, bounds.1))
}

/// The rounded background of the panel.
fn card(cr: &cairo::Context, t: &Theme, (w, h): (f64, f64)) {
    rounded(cr, 0.5, 0.5, w - 1.0, h - 1.0, 16.0);
    t.surface.set(cr);
    cr.fill_preserve().ok();
    t.outline.alpha(0.35).set(cr);
    cr.set_line_width(1.0);
    cr.stroke().ok();
}

/// Draws an icon rendered at device pixels with its top-left at (x, y).
pub fn draw_icon(
    cr: &cairo::Context,
    img: &cairo::ImageSurface,
    x: f64,
    y: f64,
    scale: f64,
    alpha: f64,
) {
    cr.save().ok();
    cr.translate(x, y);
    cr.scale(1.0 / scale, 1.0 / scale);
    cr.set_source_surface(img, 0.0, 0.0).ok();
    cr.paint_with_alpha(alpha).ok();
    cr.restore().ok();
}

pub fn text_layout(
    cr: &cairo::Context,
    font: &str,
    text: &str,
    px: f64,
    bold: bool,
) -> pango::Layout {
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

pub fn rounded(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
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
    fn fits_within_what_the_compositor_offers() {
        assert_eq!(fit_within((800, 300), (0, 0)), 1.0);
        assert_eq!(fit_within((800, 300), (1000, 1000)), 1.0);
        assert_eq!(fit_within((800, 300), (800, 300)), 1.0);
        assert_eq!(fit_within((800, 300), (400, 0)), 0.5);
        assert_eq!(fit_within((800, 300), (400, 75)), 0.25);
    }

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
