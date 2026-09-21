//! The audio enhancement panel: the ten-band equaliser and the effects after it.
//!
//! A floating window — not an [`Overlay`](crate::state::Overlay), and not a settings page.
//! Not an overlay for the same reason the picture panel is not one: what is being judged is
//! playing while the controls are used, and a scrim over the interface (or a keyboard trap)
//! would get in the way of adjusting it. Not a settings page because that is somewhere you
//! visit once, whereas an equaliser is what you sit in front of with the music on — and
//! because these parameters belong to the **output device**, not to the file, so the panel
//! is just as meaningful over a photograph as over a film.
//!
//! Three further things about this panel are deliberate.
//!
//! * The curve above the sliders is plotted from the *same* RBJ coefficients the filters
//!   are built from ([`mvp_core::dsp::Coeffs`]), so the picture and the sound cannot
//!   disagree — and it is drawn at the output device's own sample rate, because 16 kHz
//!   cannot exist at a 22 kHz rate and the picture should not claim it can.
//! * Every control writes into `settings.audio_enhance` and then publishes the whole set
//!   through [`PlayerApp::publish_audio_enhance`]. One path from interface to DSP, the
//!   same reasoning `picture.rs` is built on.
//! * When the switch is off, or there is no output device, the panel says so instead of
//!   offering controls that would do nothing.

use egui::{Color32, Context, RichText, Stroke, Ui, Vec2};

use crate::app::PlayerApp;
use crate::settings::{AudioEnhanceSettings, EqPreset};
use crate::theme::{font, space, Tokens};
use crate::ui::surface;
use crate::ui::widgets;

/// Width of the panel, in points.
///
/// The picture panel's width, and for the same reason: `slider_row` wants a label, a
/// readout and a slider, and a narrower panel squeezes every drag into the 72 pt floor.
const WIDTH: f32 = 356.0;

/// Vertical range of the curve, in dB either side of the centre line.
const CURVE_DB: f32 = 12.0;

/// How many points the curve is sampled from: enough that a narrow band does not look
/// like a triangle, few enough that the whole panel costs nothing to draw.
const CURVE_POINTS: usize = 160;

/// Frequency range the curve is drawn over.
const CURVE_MIN_HZ: f32 = 20.0;
const CURVE_MAX_HZ: f32 = 20_000.0;

/// The shelf corners, which must match `mvp_core::dsp::enhance`.
const BASS_CORNER_HZ: f32 = 120.0;
const CLARITY_CORNER_HZ: f32 = 6_000.0;

/// Draw the panel when it is open.
pub fn draw(app: &mut PlayerApp, ctx: &Context) {
    if !app.ui.audio_enhance_open {
        return;
    }
    let tokens = app.theme.tokens.clone();
    let mut open = true;

    let screen = ctx.screen_rect();
    // The panel is tall — four sections and eighteen rows — and a small or high-DPI screen
    // must not cut the last slider off, so it may take most of the height and scrolls when
    // the contents still do not fit.
    const RESERVED: f32 = 160.0;
    let room = (screen.height() - RESERVED).max(260.0);
    egui::Window::new(RichText::new("音效增强").size(font::H2).strong())
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .movable(true)
        .vscroll(true)
        .max_height(room)
        .default_size([WIDTH, 520.0_f32.min(room)])
        // Against the right edge, where the sidebar lives: an equaliser is set by ear, so it
        // may as well take the room a playlist would have used.
        .default_pos(egui::pos2(
            screen.right() - WIDTH - space::LG,
            screen.top() + 64.0,
        ))
        .constrain_to(screen)
        .frame(surface::sheet_shell(
            &tokens,
            surface::sheet_margin(),
            surface::SHEET_RADIUS,
        ))
        .show(ctx, |ui| {
            ui.set_width(WIDTH - 2.0 * space::MD);
            body(app, ui, &tokens);
        });

    if !open {
        app.ui.audio_enhance_open = false;
    }
}

/// The panel's contents, drawn inside the window's `Ui`.
fn body(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    let mut changed = false;
    let ready = app.audio_enhance_ready();
    let latency = app.audio_enhance_latency_ms();
    let sample_rate = app.audio_sample_rate();

    widgets::section(ui, tokens, "音效增强");
    changed |= widgets::switch_row(
        ui,
        tokens,
        "启用音效增强",
        &mut app.settings.audio_enhance.enabled,
        "关闭时音频处理链完全不参与播放",
    );
    if !ready {
        ui.label(
            RichText::new("当前没有可用的音频输出设备，音效不会生效。")
                .size(font::SMALL)
                .color(tokens.text_weak),
        );
    } else if app.settings.audio_enhance.enabled {
        ui.label(
            RichText::new(format!(
                "音效链引入约 {latency:.1} 毫秒延迟；所有滑块归零时自动旁路，音质与未启用时一致。"
            ))
            .size(font::SMALL)
            .color(tokens.text_weak),
        );
    }

    widgets::section(ui, tokens, "均衡器");
    changed |= widgets::row(ui, tokens, "预设", "选择预设会覆盖十段增益", 268.0, |ui| {
        let mut changed = false;
        let current = app.settings.audio_enhance.preset;
        egui::ComboBox::from_id_salt("mvp_audio_eq_preset")
            .selected_text(RichText::new(current.label()).size(font::SMALL))
            .width(258.0)
            .show_ui(ui, |ui| {
                for preset in EqPreset::all() {
                    let selected = current == *preset;
                    let label = RichText::new(preset.label()).size(font::SMALL);
                    if ui.selectable_label(selected, label).clicked() && !selected {
                        app.settings.audio_enhance.apply_preset(*preset);
                        changed = true;
                    }
                }
            });
        changed
    });

    curve(ui, tokens, &app.settings.audio_enhance, sample_rate);
    ui.label(
        RichText::new(format!(
            "响应曲线按 {:.0} Hz 输出采样率绘制；频段超出奈奎斯特频率时自动收窄。",
            sample_rate
        ))
        .size(font::SMALL)
        .color(tokens.text_weak),
    );

    // One call, and the preset label follows the gains inside it: a curve that stops
    // matching 温暖 stops claiming to be 温暖 (`EqPreset::of`).
    changed |= bands(ui, tokens, &mut app.settings.audio_enhance);

    // ---- bass and clarity -------------------------------------------------
    widgets::section(ui, tokens, "低音与清晰度");
    {
        let s = &mut app.settings.audio_enhance;
        changed |= widgets::slider_row(
            ui,
            tokens,
            "低音提升",
            &mut s.bass_db,
            0.0..=12.0,
            |v| format!("{v:+.1} dB"),
        );
        changed |= widgets::slider_row(ui, tokens, "低音谐波", &mut s.bass_harmonics, 0.0..=1.0, percent);
        changed |= widgets::slider_row(
            ui,
            tokens,
            "清晰度",
            &mut s.clarity_db,
            0.0..=12.0,
            |v| format!("{v:+.1} dB"),
        );
        changed |= widgets::slider_row(ui, tokens, "瞬态增强", &mut s.clarity_transient, 0.0..=1.0, percent);
    }
    ui.label(
        RichText::new("低音谐波为低音补上一层二次谐波，让小型扬声器也听得出低频的存在感。")
            .size(font::SMALL)
            .color(tokens.text_weak),
    );

    // ---- loudness and protection ------------------------------------------
    widgets::section(ui, tokens, "响度与保护");
    {
        let s = &mut app.settings.audio_enhance;
        changed |= widgets::slider_row(ui, tokens, "响度目标", &mut s.loudness_db, -60.0..=-10.0, |v| {
            if v <= -59.5 {
                "关闭".to_string()
            } else {
                format!("{v:.0} dBFS")
            }
        });
        changed |= widgets::slider_row(
            ui,
            tokens,
            "跟随速度",
            &mut s.loudness_speed,
            0.5..=10.0,
            |v| format!("{v:.1} 秒"),
        );
        changed |= widgets::slider_row(
            ui,
            tokens,
            "限制器上限",
            &mut s.limiter_ceiling_db,
            -6.0..=0.0,
            |v| format!("{v:+.1} dBFS"),
        );
    }
    ui.label(
        RichText::new("响度归一化让对白与音乐靠近同一音量；限制器排在其后，任何增益都不会让输出越过上限。")
            .size(font::SMALL)
            .color(tokens.text_weak),
    );

    // ---- spatial -----------------------------------------------------------
    widgets::section(ui, tokens, "空间感");
    {
        let s = &mut app.settings.audio_enhance;
        changed |= widgets::slider_row(ui, tokens, "立体声宽度", &mut s.width, 0.0..=2.0, percent);
        changed |= widgets::slider_row(ui, tokens, "空间残响", &mut s.room, 0.0..=1.0, percent);
    }
    ui.label(
        RichText::new(
            "立体声宽度调整左右声道的比例；空间残响给声音叠上会逐渐消失的反射尾音，数值越大尾音越响、\n\
             拖得越久（0 % 时这一级完全不介入，也不增加延迟）。只影响双声道输出的前两个声道。",
        )
        .size(font::SMALL)
        .color(tokens.text_weak),
    );

    // ---- reset -------------------------------------------------------------
    ui.add_space(space::MD);
    if widgets::secondary_button(ui, tokens, "恢复默认", 96.0) {
        app.settings.audio_enhance.reset();
        changed = true;
    }
    if app.settings.audio_enhance.is_neutral() && app.settings.audio_enhance.enabled {
        ui.label(
            RichText::new("所有参数均为中性值，音效链已自动旁路。")
                .size(font::SMALL)
                .color(tokens.text_weak),
        );
    }

    if changed {
        app.publish_audio_enhance();
        // The same gate every other page uses: one write per change, not one per frame.
        app.store.mark_dirty();
    }
}

/// `0.0..=1.0` written as a percentage.
fn percent(value: f32) -> String {
    format!("{:.0} %", value * 100.0)
}

/// The ten band sliders. `true` when any of them moved.
///
/// `set_band` rather than a write to the array: the preset label has to follow the gains, and
/// keeping that in one setter is what stops the two from drifting apart.
fn bands(ui: &mut Ui, tokens: &Tokens, settings: &mut AudioEnhanceSettings) -> bool {
    let mut changed = false;
    for index in 0..mvp_core::dsp::EQ_BANDS {
        let freq = mvp_core::dsp::EQ_FREQS.get(index).copied().unwrap_or(1_000.0);
        let label = band_label(freq);
        let mut gain = settings.bands.get(index).copied().unwrap_or(0.0);
        let moved = widgets::slider_row(ui, tokens, &label, &mut gain, -12.0..=12.0, |v| {
            format!("{v:+.1} dB")
        });
        if moved {
            settings.set_band(index, gain);
            changed = true;
        }
    }
    changed
}

/// `31 Hz`, `1 kHz`, `16 kHz`.
fn band_label(freq: f32) -> String {
    if freq >= 1_000.0 {
        format!("{:.0} kHz", freq / 1_000.0)
    } else {
        format!("{freq:.0} Hz")
    }
}

/// Draw the response of the coefficients the DSP will actually build.
fn curve(ui: &mut Ui, tokens: &Tokens, settings: &AudioEnhanceSettings, sample_rate: f32) {
    let width = ui.available_width().max(160.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 96.0), egui::Sense::hover());
    let painter = ui.painter();

    let faint = |colour: Color32, alpha: u8| {
        Color32::from_rgba_unmultiplied(colour.r(), colour.g(), colour.b(), alpha)
    };
    // The grid: 0 dB in the middle, −6 and −12 / +6 and +12 either side of it.
    for step in -2..=2 {
        let db = step as f32 * (CURVE_DB / 2.0);
        let y = y_of(rect, db);
        let stroke = if step == 0 {
            Stroke::new(1.0_f32, faint(tokens.text, 90))
        } else {
            Stroke::new(1.0_f32, faint(tokens.text_weak, 45))
        };
        painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], stroke);
    }
    for freq in mvp_core::dsp::EQ_FREQS {
        let x = x_of(rect, freq);
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            Stroke::new(1.0_f32, faint(tokens.text_weak, 30)),
        );
    }

    let mut points = Vec::with_capacity(CURVE_POINTS);
    for index in 0..CURVE_POINTS {
        let t = index as f32 / (CURVE_POINTS - 1) as f32;
        let freq = CURVE_MIN_HZ * (CURVE_MAX_HZ / CURVE_MIN_HZ).powf(t);
        let db = response_db(settings, sample_rate, freq);
        points.push(egui::pos2(
            rect.left() + t * rect.width(),
            y_of(rect, db).clamp(rect.top(), rect.bottom()),
        ));
    }
    painter.add(egui::Shape::line(points, Stroke::new(1.6_f32, tokens.text)));
}

/// Total response of the whole equaliser at `freq`, in dB.
///
/// Cascaded filters multiply, so their decibels add — which is why a curve that looks
/// wrong can only mean a coefficient that *is* wrong.
fn response_db(settings: &AudioEnhanceSettings, sample_rate: f32, freq: f32) -> f32 {
    let mut db = 0.0;
    for (index, gain) in settings.bands.iter().enumerate() {
        if gain.abs() < 1e-4 {
            continue;
        }
        let centre = mvp_core::dsp::EQ_FREQS.get(index).copied().unwrap_or(1_000.0);
        let coeffs = mvp_core::dsp::Coeffs::peaking(
            sample_rate,
            centre.min(sample_rate * 0.45),
            settings.bandwidth,
            *gain,
        );
        db += 20.0 * coeffs.magnitude(sample_rate, freq).max(1e-9).log10();
    }
    db += shelf_db(BASS_CORNER_HZ, settings.bass_db, sample_rate, freq);
    db += shelf_db(CLARITY_CORNER_HZ, settings.clarity_db, sample_rate, freq);
    db
}

/// One shelf's contribution, in dB.
fn shelf_db(corner: f32, gain_db: f32, sample_rate: f32, freq: f32) -> f32 {
    if gain_db.abs() < 1e-4 {
        return 0.0;
    }
    let coeffs = if corner < 1_000.0 {
        mvp_core::dsp::Coeffs::low_shelf(sample_rate, corner, gain_db, 1.0)
    } else {
        mvp_core::dsp::Coeffs::high_shelf(sample_rate, corner, gain_db, 1.0)
    };
    20.0 * coeffs.magnitude(sample_rate, freq).max(1e-9).log10()
}

/// `db` as a y inside `rect`.
fn y_of(rect: egui::Rect, db: f32) -> f32 {
    rect.center().y - (db / CURVE_DB).clamp(-1.0, 1.0) * rect.height() * 0.45
}

/// `freq` as an x inside `rect`, logarithmically: an octave has to be the same width
/// everywhere, which is the only way a ten-band octave equaliser reads correctly.
fn x_of(rect: egui::Rect, freq: f32) -> f32 {
    let t = (freq.max(CURVE_MIN_HZ) / CURVE_MIN_HZ).log10() / (CURVE_MAX_HZ / CURVE_MIN_HZ).log10();
    rect.left() + t.clamp(0.0, 1.0) * rect.width()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three functions below are pure, which is the point: the curve and its axes are the
    /// part of this page a user cannot check by eye, so they are checked here instead.

    #[test]
    fn the_curve_is_flat_when_nothing_is_set() {
        let flat = AudioEnhanceSettings::default();
        for freq in [20.0, 120.0, 1_000.0, 6_000.0, 20_000.0] {
            assert!(
                response_db(&flat, 48_000.0, freq).abs() < 1e-3,
                "{freq} Hz drew {} dB with every control neutral",
                response_db(&flat, 48_000.0, freq)
            );
        }
    }

    #[test]
    fn the_curve_peaks_where_a_band_was_moved() {
        let mut moved = AudioEnhanceSettings::default();
        moved.set_band(5, 6.0); // the 1 kHz band
        let at_centre = response_db(&moved, 48_000.0, 1_000.0);
        assert!(
            (at_centre - 6.0).abs() < 0.4,
            "a +6 dB band drew {at_centre:.2} dB at its centre"
        );
        assert!(
            response_db(&moved, 48_000.0, 60.0).abs() < 1.0,
            "and must leave two octaves below alone"
        );
    }

    #[test]
    fn the_frequency_axis_is_logarithmic() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(300.0, 96.0));
        // The geometric mid-point of the range (632 Hz) belongs in the middle of the box; on a
        // linear axis it would sit at the far right.
        let middle = x_of(rect, (CURVE_MIN_HZ * CURVE_MAX_HZ).sqrt());
        assert!((middle - 150.0).abs() < 4.0, "the mid frequency drew at {middle}");
        assert!(x_of(rect, CURVE_MIN_HZ).abs() < 0.01);
        assert!((x_of(rect, CURVE_MAX_HZ) - 300.0).abs() < 0.01);
    }

    #[test]
    fn the_vertical_axis_puts_zero_db_in_the_middle() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(300.0, 100.0));
        assert!((y_of(rect, 0.0) - 50.0).abs() < 0.01);
        assert!(y_of(rect, CURVE_DB) < 50.0, "a boost is drawn above the middle");
        assert!(y_of(rect, -CURVE_DB) > 50.0, "a cut is drawn below it");
        // And a wild value cannot draw outside the box.
        assert!(y_of(rect, 200.0) >= rect.top());
        assert!(y_of(rect, -200.0) <= rect.bottom());
    }

    #[test]
    fn the_band_labels_read_the_way_a_graphic_equaliser_should() {
        assert_eq!(band_label(31.0), "31 Hz");
        assert_eq!(band_label(250.0), "250 Hz");
        assert_eq!(band_label(1_000.0), "1 kHz");
        assert_eq!(band_label(16_000.0), "16 kHz");
    }

    #[test]
    fn a_percentage_readout_rounds_to_whole_numbers() {
        assert_eq!(percent(0.0), "0 %");
        assert_eq!(percent(0.5), "50 %");
        assert_eq!(percent(1.0), "100 %");
    }
}
