//! Interactive fullscreen alternate-screen terminal viewer for Kitty graphics, Braille, and ASCII.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, SetAttribute};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute, queue};
use image::RgbaImage;
use rayon::prelude::*;

use dith_core::animate::AnimFrame;
use dith_core::output::{
    KittyOptions, encode_fast_png_bytes, encode_kitty_png, kitty_clear_sequence, render_ascii,
    render_braille,
};

/// Rendering backend used by the fullscreen viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerBackend {
    Kitty,
    Braille,
    Ascii,
}

/// Content displayed in the fullscreen alternate-screen viewer.
#[derive(Debug, Clone)]
pub struct FullscreenViewerInput<'a> {
    /// Primary image to display (normally the dithered output).
    pub primary: &'a RgbaImage,
    /// Optional untouched source image for instant `[r]` before/after toggling.
    pub raw_source: Option<&'a RgbaImage>,
    /// Short caption displayed in the bottom status bar (e.g. `"sphere · risograph"`).
    pub title: String,
    /// Start in raw view instead of dithered view.
    pub start_raw: bool,
    /// Terminal renderer backend.
    pub backend: ViewerBackend,
}

/// Computed centered placement inside the terminal viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FullscreenLayout {
    pub col: u16,
    pub row: u16,
    pub disp_cols: u16,
    pub disp_rows: u16,
    pub kitty_cols: Option<u32>,
    pub kitty_rows: Option<u32>,
    pub effective_fit: bool,
}

/// Compute exact centered placement and aspect-preserving Kitty cell constraint.
///
/// Leaves 1 row at the bottom for the status bar. Uses real terminal pixel dimensions when
/// reported by the terminal (`TIOCGWINSZ`), falling back to a 1:2 cell aspect ratio (`10x20` px).
#[must_use]
pub fn compute_fullscreen_layout(
    img_w: u32,
    img_h: u32,
    term_cols: u16,
    term_rows: u16,
    term_px_w: u16,
    term_px_h: u16,
    fit_to_screen: bool,
) -> FullscreenLayout {
    let avail_cols = term_cols.max(1);
    let avail_rows = term_rows.saturating_sub(1).max(1);

    let (cell_w, cell_h) = if term_px_w > 0 && term_px_h > 0 && term_cols > 0 && term_rows > 0 {
        (
            f32::from(term_px_w) / f32::from(term_cols),
            f32::from(term_px_h) / f32::from(term_rows),
        )
    } else {
        (10.0, 20.0)
    };

    let iw = img_w.max(1) as f32;
    let ih = img_h.max(1) as f32;
    let box_px_w = f32::from(avail_cols) * cell_w;
    let box_px_h = f32::from(avail_rows) * cell_h;

    let nat_cols = (iw / cell_w).ceil() as u32;
    let nat_rows = (ih / cell_h).ceil() as u32;
    let fits_natively = nat_cols <= u32::from(avail_cols) && nat_rows <= u32::from(avail_rows);

    if !fit_to_screen && fits_natively {
        let disp_cols = (nat_cols as u16).max(1);
        let disp_rows = (nat_rows as u16).max(1);
        return FullscreenLayout {
            col: avail_cols.saturating_sub(disp_cols) / 2,
            row: avail_rows.saturating_sub(disp_rows) / 2,
            disp_cols,
            disp_rows,
            kitty_cols: None,
            kitty_rows: None,
            effective_fit: false,
        };
    }

    let scale_x = box_px_w / iw;
    let scale_y = box_px_h / ih;
    let width_constrained = scale_x <= scale_y;
    let scale = scale_x.min(scale_y);

    let disp_cols = ((iw * scale) / cell_w)
        .round()
        .clamp(1.0, f32::from(avail_cols)) as u16;
    let disp_rows = ((ih * scale) / cell_h)
        .round()
        .clamp(1.0, f32::from(avail_rows)) as u16;

    // Supply only the constraining axis to Kitty so the terminal preserves 100% exact pixel aspect.
    let (kitty_cols, kitty_rows) = if width_constrained {
        (Some(u32::from(disp_cols)), None)
    } else {
        (None, Some(u32::from(disp_rows)))
    };

    FullscreenLayout {
        col: avail_cols.saturating_sub(disp_cols) / 2,
        row: avail_rows.saturating_sub(disp_rows) / 2,
        disp_cols,
        disp_rows,
        kitty_cols,
        kitty_rows,
        effective_fit: true,
    }
}

/// RAII guard that enters raw + alternate-screen mode and restores the terminal cleanly on drop.
struct FullscreenGuard {
    tmux: bool,
    clear_kitty: bool,
}

impl FullscreenGuard {
    fn enter(tmux: bool, clear_kitty: bool) -> Result<Self> {
        terminal::enable_raw_mode().context("enabling terminal raw mode for fullscreen preview")?;
        let mut stdout = io::stdout();
        if let Err(err) = execute!(stdout, EnterAlternateScreen, cursor::Hide) {
            let _ = terminal::disable_raw_mode();
            return Err(err).context("entering alternate screen");
        }
        Ok(Self { tmux, clear_kitty })
    }
}

impl Drop for FullscreenGuard {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        if self.clear_kitty {
            let _ = stdout.write_all(kitty_clear_sequence(self.tmux).as_bytes());
        }
        let _ = execute!(stdout, cursor::Show, LeaveAlternateScreen);
        let _ = stdout.flush();
        let _ = terminal::disable_raw_mode();
    }
}

/// Launch the interactive fullscreen terminal viewer.
///
/// Keybindings:
/// - `q`, `Esc`, `Space`, `Enter`, `Ctrl+C`: exit cleanly and restore scrollback
/// - `r`: toggle between dithered and original raw image (when source is available)
/// - `f`: toggle between fit-to-screen and 1:1 native pixel scale
/// - Window resize events automatically re-center and rescale the image.
///
/// # Errors
/// Returns an error if terminal I/O or PNG encoding fails.
pub fn run_fullscreen_viewer(input: &FullscreenViewerInput<'_>) -> Result<()> {
    let tmux = KittyOptions::should_use_tmux_passthrough();
    let is_kitty = input.backend == ViewerBackend::Kitty;

    // Pre-encode lossless PNG payloads once for instantaneous redraws on resize / toggle.
    let primary_png = if is_kitty {
        Some(encode_fast_png_bytes(input.primary)?)
    } else {
        None
    };
    let raw_png = if is_kitty {
        input.raw_source.map(encode_fast_png_bytes).transpose()?
    } else {
        None
    };

    let _guard = FullscreenGuard::enter(tmux, is_kitty)?;
    let mut show_raw = input.start_raw && input.raw_source.is_some();
    let mut fit_to_screen = true;
    let mut needs_redraw = true;

    loop {
        if needs_redraw {
            let (term_cols, term_rows, term_px_w, term_px_h) = query_terminal_geometry();
            let active_img = if show_raw {
                input.raw_source.unwrap_or(input.primary)
            } else {
                input.primary
            };
            let active_png = if show_raw {
                raw_png.as_deref().or(primary_png.as_deref())
            } else {
                primary_png.as_deref()
            };

            let layout = compute_fullscreen_layout(
                active_img.width(),
                active_img.height(),
                term_cols,
                term_rows,
                term_px_w,
                term_px_h,
                fit_to_screen,
            );

            draw_frame(&FrameContext {
                img: active_img,
                png_bytes: active_png,
                backend: input.backend,
                title: &input.title,
                show_raw,
                can_toggle_raw: input.raw_source.is_some(),
                layout,
                term_cols,
                term_rows,
                tmux,
            })?;
            needs_redraw = false;
        }

        match event::read().context("reading terminal event")? {
            Event::Resize(_, _) => {
                needs_redraw = true;
            }
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Char('q' | 'Q' | ' ') | KeyCode::Esc | KeyCode::Enter => {
                    break;
                }
                KeyCode::Char('c' | 'C') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    break;
                }
                KeyCode::Char('r' | 'R') if input.raw_source.is_some() => {
                    show_raw = !show_raw;
                    needs_redraw = true;
                }
                KeyCode::Char('f' | 'F' | '1') => {
                    fit_to_screen = !fit_to_screen;
                    needs_redraw = true;
                }
                _ => {}
            },
            _ => {}
        }
    }

    Ok(())
}

fn query_terminal_geometry() -> (u16, u16, u16, u16) {
    if let Ok(ws) = terminal::window_size()
        && ws.columns > 0
        && ws.rows > 0
    {
        return (ws.columns, ws.rows, ws.width, ws.height);
    }
    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    (cols.max(1), rows.max(2), 0, 0)
}

struct FrameContext<'a> {
    img: &'a RgbaImage,
    png_bytes: Option<&'a [u8]>,
    backend: ViewerBackend,
    title: &'a str,
    show_raw: bool,
    can_toggle_raw: bool,
    layout: FullscreenLayout,
    term_cols: u16,
    term_rows: u16,
    tmux: bool,
}

fn draw_frame(ctx: &FrameContext<'_>) -> Result<()> {
    let mut stdout = io::stdout().lock();

    if ctx.backend == ViewerBackend::Kitty {
        stdout.write_all(kitty_clear_sequence(ctx.tmux).as_bytes())?;
    }
    queue!(
        stdout,
        Clear(ClearType::All),
        cursor::MoveTo(ctx.layout.col, ctx.layout.row)
    )?;

    match ctx.backend {
        ViewerBackend::Kitty => {
            if let Some(bytes) = ctx.png_bytes {
                let seq = encode_kitty_png(
                    bytes,
                    KittyOptions {
                        cols: ctx.layout.kitty_cols,
                        rows: ctx.layout.kitty_rows,
                        tmux_passthrough: ctx.tmux,
                        trailing_newline: false,
                        no_cursor_move: true,
                        image_id: Some(1),
                    },
                );
                stdout.write_all(seq.as_bytes())?;
            }
        }
        ViewerBackend::Braille => {
            let text = render_braille(ctx.img, Some(u32::from(ctx.layout.disp_cols)));
            draw_centered_text(
                &mut stdout,
                &text,
                ctx.term_cols,
                ctx.term_rows.saturating_sub(1),
            )?;
        }
        ViewerBackend::Ascii => {
            let text = render_ascii(ctx.img, Some(u32::from(ctx.layout.disp_cols)));
            draw_centered_text(
                &mut stdout,
                &text,
                ctx.term_cols,
                ctx.term_rows.saturating_sub(1),
            )?;
        }
    }

    // Draw unobtrusive dim status bar along the bottom row.
    let mode_tag = if ctx.show_raw { "raw" } else { "dithered" };
    let scale_tag = if ctx.layout.effective_fit {
        "fit"
    } else {
        "1:1"
    };
    let raw_hint = if ctx.can_toggle_raw {
        "  [r] raw/dither"
    } else {
        ""
    };
    let bar = format!(
        " dith  ·  {}  ·  {}x{}  ·  {mode_tag}/{scale_tag}{raw_hint}  [f] fit/1:1  [q/Esc] quit",
        ctx.title,
        ctx.img.width(),
        ctx.img.height()
    );
    let truncated: String = bar.chars().take(usize::from(ctx.term_cols)).collect();

    queue!(
        stdout,
        cursor::MoveTo(0, ctx.term_rows.saturating_sub(1)),
        SetAttribute(Attribute::Dim),
    )?;
    stdout.write_all(truncated.as_bytes())?;
    queue!(stdout, SetAttribute(Attribute::Reset))?;
    stdout.flush()?;

    Ok(())
}

fn draw_centered_text(
    stdout: &mut impl Write,
    text: &str,
    term_cols: u16,
    avail_rows: u16,
) -> Result<()> {
    let lines: Vec<&str> = text.lines().take(usize::from(avail_rows)).collect();
    let max_line_chars = lines
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0)
        .min(usize::from(term_cols));
    let start_col = term_cols.saturating_sub(max_line_chars as u16) / 2;
    let start_row = avail_rows.saturating_sub(lines.len() as u16) / 2;

    for (i, line) in lines.iter().enumerate() {
        let clipped: String = line.chars().take(usize::from(term_cols)).collect();
        queue!(stdout, cursor::MoveTo(start_col, start_row + i as u16))?;
        stdout.write_all(clipped.as_bytes())?;
    }
    Ok(())
}

/// Content displayed in the fullscreen animated terminal player.
#[derive(Debug, Clone)]
pub struct FullscreenAnimInput<'a> {
    /// Dithered animation frames to play back.
    pub frames: &'a [AnimFrame],
    /// Optional raw source frames for live `[r]` before/after toggle during playback.
    pub raw_frames: Option<&'a [AnimFrame]>,
    /// Short caption displayed in the bottom status bar.
    pub title: String,
    /// Terminal renderer backend.
    pub backend: ViewerBackend,
}

/// Launch the interactive fullscreen animated terminal player (flicker-free synchronized output).
///
/// Keybindings:
/// - `Space`: pause / resume playback
/// - `Left` / `Right` (or `h` / `l`): step frame backward / forward
/// - `+` / `-` (or `]` / `[`): speed up / slow down playback
/// - `r`: toggle dithered vs. raw source frames live
/// - `f` / `1`: toggle fit-to-screen vs. 1:1 native pixel scale
/// - `q`, `Esc`, `Enter`, `Ctrl+C`: exit cleanly and restore scrollback.
///
/// # Errors
/// Returns an error if terminal I/O or PNG encoding fails.
pub fn run_fullscreen_animation_viewer(input: &FullscreenAnimInput<'_>) -> Result<()> {
    if input.frames.is_empty() {
        return Ok(());
    }

    let tmux = KittyOptions::should_use_tmux_passthrough();
    let is_kitty = input.backend == ViewerBackend::Kitty;

    // Pre-encode every frame's PNG payload once in parallel for silky-smooth real-time playback.
    let primary_pngs: Vec<Vec<u8>> = if is_kitty {
        input
            .frames
            .par_iter()
            .map(|f| encode_fast_png_bytes(&f.image))
            .collect::<Result<Vec<_>>>()?
    } else {
        Vec::new()
    };
    let raw_pngs: Option<Vec<Vec<u8>>> = if is_kitty {
        input
            .raw_frames
            .map(|rf| {
                rf.par_iter()
                    .map(|f| encode_fast_png_bytes(&f.image))
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
    } else {
        None
    };

    let _guard = FullscreenGuard::enter(tmux, is_kitty)?;
    let total_frames = input.frames.len();
    let can_toggle_raw = input.raw_frames.is_some_and(|rf| rf.len() == total_frames);

    let mut frame_idx = 0usize;
    let mut paused = false;
    let mut show_raw = false;
    let mut fit_to_screen = true;
    let mut speed_mult = 1.0f32;
    let mut needs_redraw = true;
    let mut needs_full_clear = true;
    let mut last_advance = Instant::now();
    let uniform_timing = input
        .frames
        .windows(2)
        .all(|pair| pair[0].delay_ms == pair[1].delay_ms);

    loop {
        let base_delay_ms = input.frames[frame_idx].delay_ms.max(16);
        let eff_delay = Duration::from_millis(
            ((base_delay_ms as f32) / speed_mult)
                .round()
                .clamp(16.0, 65_535.0) as u64,
        );

        if needs_redraw {
            let (term_cols, term_rows, term_px_w, term_px_h) = query_terminal_geometry();
            let active_img = if show_raw && let Some(rf) = input.raw_frames {
                &rf[frame_idx].image
            } else {
                &input.frames[frame_idx].image
            };
            let active_png = if show_raw && let Some(rp) = &raw_pngs {
                Some(rp[frame_idx].as_slice())
            } else {
                primary_pngs.get(frame_idx).map(Vec::as_slice)
            };

            let layout = compute_fullscreen_layout(
                active_img.width(),
                active_img.height(),
                term_cols,
                term_rows,
                term_px_w,
                term_px_h,
                fit_to_screen,
            );

            let rate = if uniform_timing {
                format!(
                    "{}fps",
                    (1000.0 / eff_delay.as_millis().max(1) as f32).round() as u32
                )
            } else {
                format!("{}ms", eff_delay.as_millis())
            };
            let state_icon = if paused { "paused" } else { "playing" };
            let title = format!(
                "{}  ·  frame {}/{} ({rate}, {state_icon})",
                input.title,
                frame_idx + 1,
                total_frames
            );

            draw_anim_frame(&AnimFrameContext {
                img: active_img,
                png_bytes: active_png,
                backend: input.backend,
                title: &title,
                show_raw,
                can_toggle_raw,
                layout,
                term_cols,
                term_rows,
                tmux,
                full_clear: needs_full_clear,
            })?;
            needs_redraw = false;
            needs_full_clear = false;
        }

        let poll_timeout = if paused {
            Duration::from_millis(100)
        } else {
            eff_delay.saturating_sub(last_advance.elapsed())
        };

        if event::poll(poll_timeout).context("polling terminal events")? {
            match event::read().context("reading terminal event")? {
                Event::Resize(_, _) => {
                    needs_redraw = true;
                    needs_full_clear = true;
                }
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Char('q' | 'Q') | KeyCode::Esc | KeyCode::Enter => break,
                    KeyCode::Char('c' | 'C') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        break;
                    }
                    KeyCode::Char(' ') => {
                        paused = !paused;
                        last_advance = Instant::now();
                        needs_redraw = true;
                    }
                    KeyCode::Right | KeyCode::Char('l' | '.') => {
                        paused = true;
                        frame_idx = (frame_idx + 1) % total_frames;
                        needs_redraw = true;
                    }
                    KeyCode::Left | KeyCode::Char('h' | ',') => {
                        paused = true;
                        frame_idx = if frame_idx == 0 {
                            total_frames - 1
                        } else {
                            frame_idx - 1
                        };
                        needs_redraw = true;
                    }
                    KeyCode::Char('+' | '=' | ']') => {
                        speed_mult = (speed_mult * 1.25).min(4.0);
                        needs_redraw = true;
                    }
                    KeyCode::Char('-' | '[') => {
                        speed_mult = (speed_mult / 1.25).max(0.25);
                        needs_redraw = true;
                    }
                    KeyCode::Char('r' | 'R') if can_toggle_raw => {
                        show_raw = !show_raw;
                        needs_redraw = true;
                    }
                    KeyCode::Char('f' | 'F' | '1') => {
                        fit_to_screen = !fit_to_screen;
                        needs_redraw = true;
                        needs_full_clear = true;
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        if !paused && last_advance.elapsed() >= eff_delay {
            frame_idx = (frame_idx + 1) % total_frames;
            last_advance = Instant::now();
            needs_redraw = true;
        }
    }

    Ok(())
}

struct AnimFrameContext<'a> {
    img: &'a RgbaImage,
    png_bytes: Option<&'a [u8]>,
    backend: ViewerBackend,
    title: &'a str,
    show_raw: bool,
    can_toggle_raw: bool,
    layout: FullscreenLayout,
    term_cols: u16,
    term_rows: u16,
    tmux: bool,
    full_clear: bool,
}

fn draw_anim_frame(ctx: &AnimFrameContext<'_>) -> Result<()> {
    let mut stdout = io::stdout().lock();

    // Begin synchronized output (DEC mode 2026) so frame replacement renders atomically with zero flicker.
    stdout.write_all(b"\x1b[?2026h")?;
    if ctx.backend != ViewerBackend::Kitty || ctx.full_clear {
        if ctx.backend == ViewerBackend::Kitty {
            stdout.write_all(kitty_clear_sequence(ctx.tmux).as_bytes())?;
        }
        queue!(stdout, Clear(ClearType::All))?;
    }
    queue!(stdout, cursor::MoveTo(ctx.layout.col, ctx.layout.row))?;

    match ctx.backend {
        ViewerBackend::Kitty => {
            if let Some(bytes) = ctx.png_bytes {
                let seq = encode_kitty_png(
                    bytes,
                    KittyOptions {
                        cols: ctx.layout.kitty_cols,
                        rows: ctx.layout.kitty_rows,
                        tmux_passthrough: ctx.tmux,
                        trailing_newline: false,
                        no_cursor_move: true,
                        image_id: Some(1),
                    },
                );
                stdout.write_all(seq.as_bytes())?;
            }
        }
        ViewerBackend::Braille => {
            let text = render_braille(ctx.img, Some(u32::from(ctx.layout.disp_cols)));
            draw_centered_text(
                &mut stdout,
                &text,
                ctx.term_cols,
                ctx.term_rows.saturating_sub(1),
            )?;
        }
        ViewerBackend::Ascii => {
            let text = render_ascii(ctx.img, Some(u32::from(ctx.layout.disp_cols)));
            draw_centered_text(
                &mut stdout,
                &text,
                ctx.term_cols,
                ctx.term_rows.saturating_sub(1),
            )?;
        }
    }

    let mode_tag = if ctx.show_raw { "raw" } else { "dithered" };
    let scale_tag = if ctx.layout.effective_fit {
        "fit"
    } else {
        "1:1"
    };
    let raw_hint = if ctx.can_toggle_raw { " [r] raw" } else { "" };
    let bar = format!(
        " dith  ·  {}  ·  {mode_tag}/{scale_tag}  [Space] play/pause  [←/→] step  [+/-] speed{raw_hint}  [f] fit  [q] quit",
        ctx.title
    );
    let truncated: String = bar.chars().take(usize::from(ctx.term_cols)).collect();

    queue!(
        stdout,
        cursor::MoveTo(0, ctx.term_rows.saturating_sub(1)),
        SetAttribute(Attribute::Dim),
    )?;
    stdout.write_all(truncated.as_bytes())?;
    queue!(stdout, SetAttribute(Attribute::Reset))?;
    // End synchronized output.
    stdout.write_all(b"\x1b[?2026l")?;
    stdout.flush()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fullscreen_layout_centers_and_constrains_limiting_axis() {
        // Wide image on square-ish pixel viewport -> width constrained.
        let wide = compute_fullscreen_layout(1000, 500, 100, 41, 1000, 820, true);
        assert_eq!(wide.kitty_cols, Some(100));
        assert_eq!(wide.kitty_rows, None);
        assert_eq!(wide.col, 0);
        assert!(wide.row > 0, "expected vertical centering: {wide:?}");

        // Tall image on wide viewport -> height constrained.
        let tall = compute_fullscreen_layout(400, 800, 120, 41, 1200, 820, true);
        assert_eq!(tall.kitty_cols, None);
        assert_eq!(tall.kitty_rows, Some(40));
        assert!(tall.col > 0, "expected horizontal centering: {tall:?}");
        assert_eq!(tall.row, 0);

        // 1:1 native mode on small image preserves native size without forced scaling.
        let native = compute_fullscreen_layout(200, 200, 100, 41, 1000, 820, false);
        assert_eq!(native.kitty_cols, None);
        assert_eq!(native.kitty_rows, None);
        assert!(!native.effective_fit);
        assert!(native.col > 0 && native.row > 0);
    }
}
