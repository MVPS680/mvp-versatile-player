//! Small modal dialogs: open URL, keyboard reference and about.

use egui::{Context, RichText};

use crate::app::PlayerApp;
use crate::icons::Icon;
use crate::state::Overlay;
use crate::theme::{font, space};
use crate::ui::surface;
use crate::ui::widgets;

/// Draw whichever dialog is open.
pub fn draw(app: &mut PlayerApp, ctx: &Context) {
    match app.ui.overlay {
        Overlay::OpenUrl => url_dialog(app, ctx),
        Overlay::Shortcuts => shortcuts_dialog(app, ctx),
        Overlay::About => about_dialog(app, ctx),
        _ => {}
    }
}

/// The width for a dialog that is a fixed size by design.
///
/// A dialog is drawn in its own layer, on top of everything and clipped by
/// nothing, so a 460 pt dialog in a 400 pt window simply hangs off the side of
/// the screen — with its buttons, which is where the "确定" was supposed to be.
fn dialog_width(ctx: &Context, preferred: f32) -> f32 {
    preferred.min((ctx.screen_rect().width() - 2.0 * space::LG).max(200.0))
}

/// The frame every dialog is drawn in: an opaque sheet, one corner radius, one
/// shadow.
fn sheet_frame(tokens: &crate::theme::Tokens) -> egui::Frame {
    surface::sheet_shell(tokens, surface::sheet_margin(), surface::SHEET_RADIUS)
}

fn url_dialog(app: &mut PlayerApp, ctx: &Context) {
    let tokens = app.theme.tokens.clone();
    let mut open = true;
    let mut submit = false;
    let width = dialog_width(ctx, 460.0);
    egui::Window::new(RichText::new("打开网络串流").size(font::H3).strong())
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -40.0))
        .frame(sheet_frame(&tokens))
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.label(
                RichText::new("支持 http、https、rtsp、rtmp、mms、udp、rtp、srt 等协议")
                    .size(font::TINY)
                    .color(tokens.text_weak),
            );
            ui.add_space(space::SM);
            let response = ui.add(
                egui::TextEdit::singleline(&mut app.ui.url_input)
                    .hint_text("https://example.com/stream.m3u8")
                    .desired_width(f32::INFINITY),
            );
            if app.ui.url_focus {
                response.request_focus();
                app.ui.url_focus = false;
            }
            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                submit = true;
            }
            ui.add_space(space::MD);
            ui.horizontal(|ui| {
                // Accent for the action the dialog is for, grey for the way out.
                if widgets::primary_button(ui, &tokens, "播放", 72.0) {
                    submit = true;
                }
                if widgets::secondary_button(ui, &tokens, "取消", 72.0) {
                    app.ui.close_overlay();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(RichText::new("添加到播放列表").size(font::SMALL))
                        .clicked()
                    {
                        let url = app.ui.url_input.trim().to_string();
                        if !url.is_empty() {
                            app.playlist.add(mvp_core::playlist::PlaylistItem::from_url(url));
                            app.store.mark_dirty();
                            app.ui.close_overlay();
                        }
                    }
                });
            });
        });
    if submit {
        let url = app.ui.url_input.trim().to_string();
        if url.is_empty() {
            app.toast(crate::state::Toast::warning("请输入网络地址"));
        } else {
            app.open_url(url, true);
            app.ui.close_overlay();
        }
    }
    if !open {
        app.ui.close_overlay();
    }
}

fn shortcuts_dialog(app: &mut PlayerApp, ctx: &Context) {
    let tokens = app.theme.tokens.clone();
    let mut open = true;
    let size = crate::layout::Metrics::of(ctx).dialog_size([440.0, 520.0], [320.0, 320.0]);
    egui::Window::new(RichText::new("键盘快捷键").size(font::H3).strong())
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(size)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .frame(sheet_frame(&tokens))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::Vec2::splat(22.0), egui::Sense::hover());
                crate::icons::draw(ui.painter(), rect, Icon::Help, tokens.accent);
                ui.label(
                    RichText::new("按 Esc 关闭")
                        .size(font::TINY)
                        .color(tokens.text_muted),
                );
            });
            ui.add_space(space::XS);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (key, action) in crate::ui::settings_window::SHORTCUTS {
                        ui.horizontal(|ui| {
                            widgets::chip(ui, key, tokens.accent);
                            ui.label(
                                RichText::new(*action)
                                    .size(font::SMALL)
                                    .color(tokens.text_weak),
                            );
                        });
                    }
                });
        });
    if !open {
        app.ui.close_overlay();
    }
}

fn about_dialog(app: &mut PlayerApp, ctx: &Context) {
    let tokens = app.theme.tokens.clone();
    let mut open = true;
    let mut show_shortcuts = false;
    let width = dialog_width(ctx, 400.0);
    egui::Window::new(RichText::new("关于").size(font::H3).strong())
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .frame(sheet_frame(&tokens))
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.vertical_centered(|ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::Vec2::splat(56.0), egui::Sense::hover());
                ui.painter().circle_filled(
                    rect.center(),
                    28.0,
                    tokens.accent.gamma_multiply(0.18),
                );
                crate::icons::draw(
                    ui.painter(),
                    egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(26.0)),
                    Icon::Play,
                    tokens.accent,
                );
                ui.add_space(space::SM);
                ui.label(
                    RichText::new("MVP-Versatile-Player")
                        .size(font::H2)
                        .strong()
                        .color(tokens.text),
                );
                ui.label(
                    RichText::new(format!("版本 {}", env!("CARGO_PKG_VERSION")))
                        .size(font::SMALL)
                        .color(tokens.text_weak),
                );
            });
            ui.add_space(space::MD);
            widgets::key_value(ui, &tokens, "内核", &app.ui.ffmpeg_version.clone());
            widgets::key_value(ui, &tokens, "界面", "egui / eframe（OpenGL 后端）");
            widgets::key_value(
                ui,
                &tokens,
                "启动耗时",
                &format!("{:.0} 毫秒", app.ui.startup_ms),
            );
            ui.add_space(space::MD);
            ui.label(
                RichText::new(
                    "本程序以 GPL 许可证发布，内置的 FFmpeg 为 GPL 构建，\
                     包含 libx264 / libx265 / libaom 等组件。",
                )
                .size(font::TINY)
                .color(tokens.text_muted),
            );
            ui.add_space(space::SM);
            ui.horizontal(|ui| {
                if ui.button(RichText::new("快捷键").size(font::SMALL)).clicked() {
                    show_shortcuts = true;
                }
                if ui.button(RichText::new("项目主页").size(font::SMALL)).clicked() {
                    let _ = mvp_platform::shell::open_url(
                        "https://ffplayer.mvpclub.cc",
                    );
                }
            });
        });
    if show_shortcuts {
        app.ui.open_overlay(Overlay::Shortcuts);
        return;
    }
    if !open {
        app.ui.close_overlay();
    }
}
