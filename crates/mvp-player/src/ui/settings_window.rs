//! The settings window: seven pages covering playback, output, subtitles, shell
//! integration, shortcuts and version information.

use egui::{Context, RichText, Ui};

use crate::app::PlayerApp;
use crate::icons::Icon;
use crate::settings::{AspectMode, EndAction};
use crate::state::SettingsTab;
use crate::state::{Overlay, Toast};
use crate::theme::{font, space, Tokens};
use crate::ui::surface;
use crate::ui::widgets;

/// Draw the settings window when it is open.
pub fn draw(app: &mut PlayerApp, ctx: &Context) {
    if app.ui.overlay != Overlay::Settings {
        return;
    }
    // Entering a page is what invalidates the expensive caches it uses — the
    // registry answers and the output-device list are I/O, not state, and
    // reading them on every frame is enough to make the window stutter.
    if app.ui.cached_settings_tab != Some(app.ui.settings_tab) {
        app.ui.cached_settings_tab = Some(app.ui.settings_tab);
        app.ui.assoc_cache = None;
        app.ui.audio_device_cache = None;
    }
    let tokens = app.theme.tokens.clone();
    // The window is large by design — seven pages, two columns — so what matters is
    // that it never asks for more room than the *client area* has. Clamping to the
    // monitor instead was the bug behind "in windowed mode I cannot see the close
    // button": a player window 900 pt tall on a 1440 pt screen was handed a 620 pt
    // sheet anchored to the centre of the *screen*, and the title bar — with the
    // close button in it — landed outside the player's own frame. The same geometry
    // is why the controls at the bottom of a page could not be reached.
    let room = (ctx.screen_rect().size() - egui::vec2(2.0 * space::LG, 2.0 * space::LG))
        .max(egui::vec2(320.0, 260.0));
    let mut size = crate::layout::Metrics::of(ctx).dialog_size([860.0, 620.0], [620.0, 440.0]);
    size[0] = size[0].min(room.x);
    size[1] = size[1].min(room.y);
    let min = [620.0f32.min(size[0]), 440.0f32.min(size[1])];
    // Where it opens the first time; after that egui keeps wherever the user
    // dragged it to, which is the point of dropping the anchor below.
    let default_pos = ctx.screen_rect().center() - egui::vec2(size[0], size[1]) * 0.5;
    let mut open = true;
    egui::Window::new(RichText::new("设置").size(font::H2).strong())
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        // Draggable inside the player. An *anchored* egui window ignores the
        // pointer by design, which is why this one could not be moved at all.
        .movable(true)
        .default_size(size)
        .max_size(size)
        .min_size(min)
        .default_pos(default_pos)
        .constrain_to(ctx.screen_rect())
        // An opaque sheet. The frame carries the fill, so the title strip is the same
        // colour as the body with nothing painted into a reserved slot after the fact
        // — see [`crate::ui::surface`] for what used to happen here and why it does not
        // any more.
        .frame(surface::sheet_shell(
            &tokens,
            surface::sheet_margin(),
            surface::SHEET_RADIUS,
        ))
        .show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                // ---- tab rail ------------------------------------------
                ui.vertical(|ui| {
                    ui.set_width(132.0);
                    ui.spacing_mut().item_spacing.y = space::XS;
                    for tab in SettingsTab::all() {
                        let selected = app.ui.settings_tab == *tab;
                        let (rect, response) = ui.allocate_exact_size(
                            egui::vec2(132.0, 34.0),
                            egui::Sense::click(),
                        );
                        // A rounded pill behind the current page, the way System
                        // Settings marks the page it is showing. No leading bar:
                        // the fill *is* the indicator, and two indicators for one
                        // state is one too many.
                        if selected {
                            ui.painter().rect_filled(
                                rect,
                                egui::CornerRadius::same(crate::theme::radius::MD as u8),
                                tokens.accent_soft,
                            );
                        } else if response.hovered() {
                            ui.painter().rect_filled(
                                rect,
                                egui::CornerRadius::same(crate::theme::radius::MD as u8),
                                tokens.hover,
                            );
                        }
                        ui.painter().text(
                            egui::pos2(rect.left() + space::MD, rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            tab.label(),
                            egui::FontId::proportional(font::BODY),
                            if selected { tokens.text } else { tokens.text_weak },
                        );
                        if response.clicked() {
                            app.ui.settings_tab = *tab;
                        }
                        if response.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                    }
                });

                ui.separator();

                // ---- page content --------------------------------------
                //
                // The page must state its own layout: a `Ui` inside
                // `horizontal_top` hands its *left-to-right* layout down to
                // everything drawn in it, so without this the rows march off
                // sideways in one long band instead of stacking.
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    // Mouse users scroll with the wheel. With egui's default a drag
                    // that starts on a slider is claimed by the scroll area as soon
                    // as it moves a few pixels sideways, which is the "the slider
                    // will not slide" report: the control is fine, it never sees
                    // the drag. (The scroll bar stays draggable.)
                    .scroll_source(egui::containers::scroll_area::ScrollSource {
                        drag: false,
                        ..Default::default()
                    })
                    .show(ui, |ui| {
                        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.spacing_mut().item_spacing.x = space::SM;
                            match app.ui.settings_tab {
                                SettingsTab::General => general_page(app, ui, &tokens),
                                SettingsTab::Video => video_page(app, ui, &tokens),
                                SettingsTab::Audio => audio_page(app, ui, &tokens),
                                SettingsTab::Subtitles => subtitle_page(app, ui, &tokens),
                                SettingsTab::Integration => integration_page(app, ui, &tokens),
                                SettingsTab::Shortcuts => shortcuts_page(ui, &tokens),
                                SettingsTab::About => about_page(app, ui, &tokens),
                            }
                        });
                    });
            });
        });
    if !open {
        app.ui.close_overlay();
    }
}

fn general_page(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    widgets::section(ui, tokens, "启动与播放");
    let mut changed = false;
    changed |= widgets::switch_row(
        ui,
        tokens,
        "记住上次播放位置",
        &mut app.settings.remember_position,
        "重新打开同一个文件时从上次的位置继续",
    );

    let mut resume_min = app.settings.resume_min_seconds as f32;
    if widgets::slider_row(
        ui,
        tokens,
        "继续播放的最小进度",
        &mut resume_min,
        0.0..=120.0,
        |v| format!("{v:.0} 秒"),
    ) {
        app.settings.resume_min_seconds = resume_min as f64;
        changed = true;
    }
    changed |= widgets::switch_row(
        ui,
        tokens,
        "关闭时保存播放列表",
        &mut app.settings.restore_playlist,
        "下次启动时恢复播放列表与选项",
    );

    widgets::section(ui, tokens, "播放结束");
    changed |= widgets::combo_row(
        ui,
        tokens,
        "结束时",
        &mut app.settings.end_action,
        &[
            (EndAction::Playlist, "按播放列表继续"),
            (EndAction::Hold, "停留在最后一帧"),
            (EndAction::Close, "关闭播放器"),
        ],
    );

    widgets::section(ui, tokens, "操作");
    let mut seek_step = app.settings.seek_step as f32;
    if widgets::slider_row(
        ui,
        tokens,
        "快进/快退步长",
        &mut seek_step,
        1.0..=60.0,
        |v| format!("{v:.0} 秒"),
    ) {
        app.settings.seek_step = seek_step as f64;
        changed = true;
    }
    let mut seek_large = app.settings.seek_step_large as f32;
    if widgets::slider_row(
        ui,
        tokens,
        "大步进（Shift+方向键）",
        &mut seek_large,
        5.0..=300.0,
        |v| format!("{v:.0} 秒"),
    ) {
        app.settings.seek_step_large = seek_large as f64;
        changed = true;
    }
    changed |= widgets::switch_row(
        ui,
        tokens,
        "滚轮调节音量",
        &mut app.settings.wheel_controls_volume,
        "关闭后滚轮用于快进/快退",
    );
    changed |= widgets::switch_row(
        ui,
        tokens,
        "双击画面切换全屏",
        &mut app.settings.double_click_fullscreen,
        "",
    );

    widgets::section(ui, tokens, "窗口");
    changed |= widgets::switch_row(ui, tokens, "深色标题栏", &mut app.settings.dark_title_bar, "");
    let mut always_on_top = app.settings.always_on_top;
    if widgets::switch_row(ui, tokens, "窗口置顶", &mut always_on_top, "快捷键 A") {
        // Applied immediately: the window level is a message to the window
        // manager, so a switch that only edits a boolean looks broken.
        app.set_always_on_top(ui.ctx(), always_on_top);
        changed = true;
    }
    changed |= widgets::slider_row(
        ui,
        tokens,
        "全屏时自动隐藏控制栏（秒）",
        &mut app.settings.hide_controls_after,
        1.0..=15.0,
        |v| format!("{v:.0} 秒"),
    );

    if changed {
        app.store.mark_dirty();
    }
}

fn video_page(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    let mut changed = false;
    widgets::section(ui, tokens, "解码");
    changed |= widgets::switch_row(
        ui,
        tokens,
        "启用硬件解码",
        &mut app.settings.hardware_decoding,
        "使用显卡（D3D11VA / DXVA2）解码，可显著降低 CPU 占用；\
         若出现花屏或绿屏请关闭。更改将在重新打开文件后生效。",
    );
    changed |= widgets::switch_row(
        ui,
        tokens,
        "HDR 色调映射",
        &mut app.settings.hdr_tone_map,
        "把 HDR10 / 杜比视界的 PQ、HLG 画面压到 SDR 显示范围：\
         203 尼特以下原样保留，高光柔和收敛，避免整幅画面发灰。\
         若显示器本身支持 HDR，建议关闭。",
    );

    widgets::section(ui, tokens, "画面");
    changed |= widgets::combo_row(
        ui,
        tokens,
        "默认画面比例",
        &mut app.settings.aspect,
        &[
            (AspectMode::Fit, "适应窗口"),
            (AspectMode::Crop, "裁剪填充"),
            (AspectMode::Stretch, "拉伸填充"),
            (AspectMode::Ratio16x9, "16:9"),
            (AspectMode::Ratio4x3, "4:3"),
        ],
    );
    changed |= widgets::switch_row(
        ui,
        tokens,
        "控制栏加暗色衬底",
        &mut app.settings.control_scrim,
        "在画面底部绘制渐变，让控制栏在明亮的画面上也清晰；\
         全屏时只在控制栏出现时绘制（窗口模式下控制栏本身已是实底）",
    );
    changed |= widgets::switch_row(
        ui,
        tokens,
        "缩放时显示鸟瞰图",
        &mut app.settings.minimap,
        "画面被放大到超出画布时，在画布右下角显示整幅画面的缩略图与取景框，\
         可拖动取景框移动画面（Ctrl + 滚轮缩放）。全屏且控制栏隐藏时不显示。",
    );

    widgets::section(ui, tokens, "截图");
    let dir = app.settings.snapshot_dir();
    changed |= widgets::row(
        ui,
        tokens,
        "保存目录",
        &dir.display().to_string(),
        190.0,
        |ui| {
            let mut changed = false;
            if ui.button(RichText::new("打开").size(font::SMALL)).clicked() {
                let _ = std::fs::create_dir_all(&dir);
                mvp_platform::shell::reveal_in_explorer(&dir);
            }
            if ui.button(RichText::new("选择…").size(font::SMALL)).clicked() {
                if let Some(picked) = rfd::FileDialog::new()
                    .set_title("选择截图保存目录")
                    .set_directory(&dir)
                    .pick_folder()
                {
                    app.settings.snapshot_dir = Some(picked);
                    changed = true;
                }
            }
            changed
        },
    );

    widgets::section(ui, tokens, "图片");
    changed |= widgets::slider_row(
        ui,
        tokens,
        "幻灯片间隔",
        &mut app.settings.slideshow_interval,
        1.0..=60.0,
        |v| format!("{v:.0} 秒"),
    );
    changed |= widgets::switch_row(
        ui,
        tokens,
        "播放动画图片（GIF / WebP / APNG）",
        &mut app.settings.animate_images,
        "",
    );

    if changed {
        app.store.mark_dirty();
    }
}

fn audio_page(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    let mut changed = false;
    widgets::section(ui, tokens, "输出设备");
    // Enumerating WASAPI endpoints is not free; do it once per visit.
    if app.ui.audio_device_cache.is_none() {
        app.ui.audio_device_cache = Some(crate::state::AudioDeviceCache {
            devices: mvp_core::audio::output_device_names(),
            default_name: mvp_core::audio::default_output_device_name()
                .unwrap_or_else(|| "系统默认设备".to_string()),
        });
    }
    let (devices, default_name) = {
        let cache = app
            .ui
            .audio_device_cache
            .as_ref()
            .expect("the cache was filled just above");
        (cache.devices.clone(), cache.default_name.clone())
    };

    let current = app
        .settings
        .audio_device
        .clone()
        .unwrap_or_else(|| format!("跟随系统（{default_name}）"));

    changed |= widgets::row(
        ui,
        tokens,
        "设备",
        "更改音频设备将在重新打开文件后生效",
        268.0,
        |ui| {
            let mut changed = false;
            egui::ComboBox::from_id_salt("mvp_audio_device")
                .selected_text(RichText::new(current).size(font::SMALL))
                .width(258.0)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(
                            app.settings.audio_device.is_none(),
                            RichText::new("跟随系统默认设备").size(font::SMALL),
                        )
                        .clicked()
                    {
                        app.settings.audio_device = None;
                        changed = true;
                    }
                    for device in &devices {
                        let selected = app
                            .settings
                            .audio_device
                            .as_deref()
                            .is_some_and(|d| d == device);
                        if ui
                            .selectable_label(
                                selected,
                                RichText::new(device).size(font::SMALL),
                            )
                            .clicked()
                        {
                            app.settings.audio_device = Some(device.clone());
                            changed = true;
                        }
                    }
                });
            changed
        },
    );

    widgets::section(ui, tokens, "音量");
    changed |= widgets::slider_row(
        ui,
        tokens,
        "音量",
        &mut app.settings.volume,
        0.0..=2.0,
        |v| format!("{:.0}%", v * 100.0),
    );
    changed |= widgets::switch_row(ui, tokens, "静音", &mut app.settings.muted, "");
    let mut audio_delay = app.settings.audio_delay as f32;
    if widgets::slider_row(
        ui,
        tokens,
        "音频延迟",
        &mut audio_delay,
        -5.0..=5.0,
        |v| format!("{v:+.2} 秒"),
    ) {
        app.settings.audio_delay = audio_delay as f64;
        changed = true;
    }
    ui.label(
        RichText::new("正值表示声音比画面晚，用于修正音画不同步")
            .size(font::TINY)
            .color(tokens.text_muted),
    );

    if changed {
        app.store.mark_dirty();
    }
}

fn subtitle_page(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    let mut changed = false;
    widgets::section(ui, tokens, "显示");
    changed |= widgets::switch_row(
        ui,
        tokens,
        "默认显示字幕",
        &mut app.settings.subtitles_enabled,
        "快捷键 V",
    );
    changed |= widgets::slider_row(
        ui,
        tokens,
        "字号",
        &mut app.settings.subtitle_size,
        10.0..=64.0,
        |v| format!("{v:.0} pt"),
    );
    changed |= widgets::slider_row(
        ui,
        tokens,
        "距底部距离",
        &mut app.settings.subtitle_margin,
        0.0..=200.0,
        |v| format!("{v:.0} pt"),
    );
    changed |= widgets::switch_row(
        ui,
        tokens,
        "文字描边/衬底",
        &mut app.settings.subtitle_outline,
        "在高亮画面上更容易看清",
    );

    changed |= widgets::color_row(
        ui,
        tokens,
        "文字颜色",
        "点常用色块即可，最右侧的色块可自定义任意颜色",
        &mut app.settings.subtitle_color,
        SUBTITLE_COLOURS,
    );

    let mut subtitle_delay = app.settings.subtitle_delay as f32;
    if widgets::slider_row(
        ui,
        tokens,
        "字幕延迟",
        &mut subtitle_delay,
        -10.0..=10.0,
        |v| format!("{v:+.2} 秒"),
    ) {
        app.settings.subtitle_delay = subtitle_delay as f64;
        changed = true;
    }

    widgets::section(ui, tokens, "加载");
    changed |= widgets::switch_row(
        ui,
        tokens,
        "自动加载同名字幕",
        &mut app.settings.autoload_sidecar_subtitles,
        "打开视频时自动查找同目录下的 .srt / .ass / .vtt 文件",
    );
    changed |= widgets::switch_row(
        ui,
        tokens,
        "外部字幕优先",
        &mut app.settings.prefer_external_subtitles,
        "",
    );

    if ui
        .button(RichText::new("立即加载字幕文件…").size(font::BODY))
        .clicked()
    {
        app.request_open_subtitle();
    }

    if changed {
        app.store.mark_dirty();
    }
}

fn integration_page(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    widgets::section(ui, tokens, "文件关联");
    ui.label(
        RichText::new(
            "关联注册在「当前用户」下，不需要管理员权限，也不会影响其他用户的设置。\
             注册后即可在资源管理器中右键选择「打开方式」，MVP 也会出现在\
             「默认应用」列表中。",
        )
        .size(font::SMALL)
        .color(tokens.text_weak),
    );
    ui.add_space(space::SM);

    let mut changed = false;
    changed |= widgets::switch_row(ui, tokens, "关联视频文件", &mut app.settings.file_kinds.video, "");
    changed |= widgets::switch_row(ui, tokens, "关联音频文件", &mut app.settings.file_kinds.audio, "");
    changed |= widgets::switch_row(ui, tokens, "关联图片文件", &mut app.settings.file_kinds.image, "");
    changed |= widgets::switch_row(
        ui,
        tokens,
        "关联播放列表",
        &mut app.settings.file_kinds.playlist,
        "",
    );
    changed |= widgets::switch_row(
        ui,
        tokens,
        "在右键菜单中添加「用 MVP 打开」",
        &mut app.settings.context_menu,
        "",
    );
    changed |= widgets::switch_row(
        ui,
        tokens,
        "同时尝试设置为默认播放器",
        &mut app.settings.set_as_default,
        "Windows 10/11 会保护用户的默认应用选择，可能仍需在系统设置中确认一次",
    );
    if changed {
        app.store.mark_dirty();
    }

    ui.add_space(space::MD);
    // The registry answers are read once per visit rather than once per frame:
    // four registry round-trips at 60 Hz is enough I/O to stutter the window.
    if app.ui.assoc_cache.is_none() {
        app.ui.assoc_cache = Some(crate::state::AssocCache {
            registered: mvp_platform::assoc::is_registered(),
            video: mvp_platform::assoc::current_handler_for(".mp4"),
            audio: mvp_platform::assoc::current_handler_for(".mp3"),
            image: mvp_platform::assoc::current_handler_for(".png"),
        });
    }
    let status = if app
        .ui
        .assoc_cache
        .as_ref()
        .is_some_and(|cache| cache.registered)
    {
        ("已注册文件关联", tokens.success)
    } else {
        ("尚未注册文件关联", tokens.text_weak)
    };
    ui.horizontal(|ui| {
        widgets::chip(ui, status.0, status.1);
        if !status.0.starts_with("已") {
            ui.label(
                RichText::new("点击右侧按钮即可完成注册")
                    .size(font::TINY)
                    .color(tokens.text_muted),
            );
        }
    });

    ui.add_space(space::SM);
    ui.horizontal(|ui| {
        if ui
            .add(
                egui::Button::new(RichText::new("注册文件关联").size(font::BODY))
                    .fill(tokens.accent),
            )
            .clicked()
        {
            match mvp_platform::assoc::register(
                &app.settings.file_kinds,
                app.settings.set_as_default,
                app.settings.context_menu,
            ) {
                Ok(report) => {
                    let count = report.extensions.len();
                    app.toast(Toast::success(format!(
                        "已注册 {count} 种文件类型"
                    )));
                    if report.requires_user_confirmation {
                        app.ui.error_banner = Some(
                            "Windows 保护默认应用设置：请在「默认应用」页面中把 MVP 设为默认播放器。"
                                .to_string(),
                        );
                    }
                }
                Err(err) => app.error(format!("注册失败: {err}")),
            }
            // The registry changed underneath the cache.
            app.ui.assoc_cache = None;
        }
        if ui
            .add(egui::Button::new(
                RichText::new("取消关联").size(font::BODY),
            ))
            .clicked()
        {
            match mvp_platform::assoc::unregister() {
                Ok(()) => app.toast(Toast::info("已取消文件关联")),
                Err(err) => app.error(format!("取消关联失败: {err}")),
            }
            app.ui.assoc_cache = None;
        }
        if ui
            .button(RichText::new("打开 Windows 默认应用设置").size(font::SMALL))
            .clicked()
        {
            if let Err(err) = mvp_platform::assoc::open_default_apps_settings() {
                app.error(format!("无法打开系统设置: {err}"));
            }
        }
    });

    widgets::section(ui, tokens, "当前关联");
    let cache = app.ui.assoc_cache.clone().unwrap_or_default();
    for (label, handler) in [
        ("视频 (.mp4)", cache.video),
        ("音频 (.mp3)", cache.audio),
        ("图片 (.png)", cache.image),
    ] {
        widgets::key_value(
            ui,
            tokens,
            label,
            handler.as_deref().unwrap_or("未关联"),
        );
    }

    widgets::section(ui, tokens, "程序信息");
    widgets::key_value(
        ui,
        tokens,
        "可执行文件",
        &mvp_platform::assoc::exe_path().display().to_string(),
    );
    widgets::key_value(
        ui,
        tokens,
        "设置目录",
        &crate::settings::Settings::config_dir().display().to_string(),
    );
    if ui
        .button(RichText::new("打开设置目录").size(font::SMALL))
        .clicked()
    {
        let dir = crate::settings::Settings::config_dir();
        let _ = std::fs::create_dir_all(&dir);
        mvp_platform::shell::reveal_in_explorer(&dir);
    }
}

fn shortcuts_page(ui: &mut Ui, tokens: &Tokens) {
    widgets::section(ui, tokens, "播放控制");
    for (key, action) in SHORTCUTS {
        ui.horizontal(|ui| {
            widgets::chip(ui, key, tokens.accent);
            ui.label(
                RichText::new(*action)
                    .size(font::SMALL)
                    .color(tokens.text_weak),
            );
        });
    }
}

/// Every shortcut, shared with the help window.
pub const SHORTCUTS: &[(&str, &str)] = &[
    ("空格 / K", "播放 / 暂停"),
    ("← / →", "快退 / 快进 5 秒"),
    ("Shift + ← / →", "快退 / 快进 30 秒"),
    (", / .", "上一帧 / 下一帧（暂停时逐帧查看）"),
    ("↑ / ↓", "音量 +5% / -5%"),
    ("M", "静音"),
    ("N / P", "下一项 / 上一项"),
    ("S", "截图"),
    ("F / Alt+Enter", "全屏"),
    ("Esc", "退出全屏或关闭弹窗"),
    ("[ / ]", "降低 / 提高播放速度"),
    ("\\", "恢复正常速度"),
    ("C", "切换循环模式"),
    ("H", "切换随机播放"),
    ("B", "设置 A–B 循环"),
    ("V", "字幕开关"),
    ("G", "加载字幕文件"),
    ("Z", "切换画面比例"),
    ("R", "旋转 90°"),
    ("A", "窗口置顶"),
    ("T / Ctrl+L", "显示 / 隐藏侧边栏"),
    ("Ctrl+O", "打开文件"),
    ("Ctrl+Shift+O", "打开文件夹"),
    ("Ctrl+U", "打开网络串流"),
    ("Ctrl+,", "设置"),
    ("Ctrl+W", "退出"),
    ("F1", "快捷键帮助"),
    ("0 / 1", "图片：适应窗口 / 100%"),
    ("+ / -", "放大 / 缩小画面（视频与图片）"),
    (
        "Ctrl + 滚轮",
        "缩放视频 / 图片画面，以指针为中心；右下角出现鸟瞰图，可拖动它定位",
    ),
    (
        "鼠标滚轮",
        "在画面上滚动：调节音量，每格 5%（设置中可改为快进 / 快退 5 秒）",
    ),
    ("鼠标拖动画面", "放大后平移画面（图片始终可平移）"),
];

fn about_page(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    ui.vertical_centered(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(64.0), egui::Sense::hover());
        ui.painter().circle_filled(
            rect.center(),
            32.0,
            tokens.accent.gamma_multiply(0.18),
        );
        crate::icons::draw(
            ui.painter(),
            egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(30.0)),
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
        ui.add_space(space::XS);
        ui.label(
            RichText::new("多功能音视频与图片播放器 · 基于 FFmpeg 与 egui")
                .size(font::SMALL)
                .color(tokens.text_weak),
        );
    });

    widgets::section(ui, tokens, "FFmpeg");
    widgets::key_value(ui, tokens, "版本", &app.ui.ffmpeg_version.clone());
    widgets::key_value(
        ui,
        tokens,
        "许可证",
        if mvp_core::ffmpeg_is_gpl() {
            "GPL（含 x264 / x265 等 GPL 组件）"
        } else {
            "LGPL"
        },
    );
    ui.collapsing(RichText::new("编译配置").size(font::SMALL), |ui| {
        ui.label(
            RichText::new(&app.ui.ffmpeg_config)
                .size(font::TINY)
                .monospace()
                .color(tokens.text_muted),
        );
    });

    widgets::section(ui, tokens, "运行环境");
    widgets::key_value(ui, tokens, "启动耗时", &format!("{:.0} 毫秒", app.ui.startup_ms));
    // Read once per visit, like the audio page: enumerating devices is I/O.
    if app.ui.audio_device_cache.is_none() {
        app.ui.audio_device_cache = Some(crate::state::AudioDeviceCache {
            devices: mvp_core::audio::output_device_names(),
            default_name: mvp_core::audio::default_output_device_name()
                .unwrap_or_else(|| "无".to_string()),
        });
    }
    widgets::key_value(
        ui,
        tokens,
        "音频设备",
        app.ui
            .audio_device_cache
            .as_ref()
            .map(|cache| cache.default_name.as_str())
            .unwrap_or("无"),
    );
    widgets::key_value(
        ui,
        tokens,
        "最后截图",
        &app.ui
            .last_snapshot
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "—".to_string()),
    );

    ui.add_space(space::MD);
    ui.horizontal(|ui| {
        if ui.button(RichText::new("项目主页").size(font::SMALL)).clicked() {
            let _ = mvp_platform::shell::open_url(
                "https://github.com/mvp-versatile-player/mvp-versatile-player",
            );
        }
        if ui.button(RichText::new("查看快捷键").size(font::SMALL)).clicked() {
            app.ui.open_overlay(Overlay::Shortcuts);
        }
    });
}

/// The colours subtitles are actually read in — white, off-white, yellow, cyan —
/// plus the outlined black that every player offers. The chip the user picks is
/// copied straight into `subtitle_color`, so a value that is not in this list
/// (set by hand in `settings.json`, or through the custom swatch) still shows and
/// still round-trips.
pub const SUBTITLE_COLOURS: &[(&str, [u8; 3])] = &[
    ("白色", [255, 255, 255]),
    ("奶白", [255, 246, 214]),
    ("黄色", [255, 214, 10]),
    ("橙色", [255, 170, 90]),
    ("青色", [128, 235, 235]),
    ("绿色", [140, 235, 140]),
    ("粉色", [255, 150, 190]),
    ("灰色", [190, 190, 190]),
    ("黑色", [12, 12, 12]),
];

#[cfg(test)]
mod tests {
    use super::SHORTCUTS;
    use std::collections::HashSet;

    /// The table is the user-facing contract for the keyboard. A duplicated key
    /// would mean two commands fighting over one press, and an empty one would
    /// document a shortcut that does not exist.
    #[test]
    fn every_shortcut_is_documented_once() {
        let mut seen: HashSet<&str> = HashSet::new();
        for (key, action) in SHORTCUTS {
            assert!(!key.trim().is_empty(), "a shortcut has no key: {action}");
            assert!(!action.trim().is_empty(), "a shortcut has no description");
            assert!(seen.insert(key), "duplicated shortcut key: {key}");
        }
    }

    /// Frame stepping is the one command that was documented as something it
    /// did not do; it must stay visible in the reference.
    #[test]
    fn frame_stepping_is_listed() {
        assert!(
            SHORTCUTS.iter().any(|(key, _)| key.contains(',')),
            "the frame-step keys must be documented"
        );
    }

    /// The subtitle palette has to be worth showing.
    ///
    /// Before it existed the page offered nothing but egui's custom swatch — a
    /// 20 pt square with no colours to choose from — which is why the setting read
    /// as having no palette at all. An empty or duplicated list would quietly put
    /// the page back in that state.
    #[test]
    fn the_subtitle_palette_is_real() {
        use std::collections::HashSet;

        assert!(
            super::SUBTITLE_COLOURS.len() >= 6,
            "a palette needs a few colours to pick from"
        );
        assert!(
            super::SUBTITLE_COLOURS
                .iter()
                .any(|(_, rgb)| *rgb == [255, 255, 255]),
            "white has to be on the palette: it is the default subtitle colour"
        );
        let mut seen: HashSet<[u8; 3]> = HashSet::new();
        for (name, rgb) in super::SUBTITLE_COLOURS {
            assert!(!name.trim().is_empty(), "a chip has no name: {rgb:?}");
            assert!(seen.insert(*rgb), "duplicated colour on the palette: {name}");
        }
    }

    /// The page has to scroll inside the sheet instead of being cut off by it.
    ///
    /// The bug this guards: a `ScrollArea` inside an `egui::Window` whose height is
    /// capped with `max_size` still believes it has the *screen's* height to play
    /// with — the cap applies to the window's own rect, not to the content `Ui` —
    /// so it lays the page out at its full height and never becomes scrollable.
    /// The sheet then slices the page off at its bottom edge, and everything below
    /// that line is unreachable: a page's action buttons, and any switch that
    /// happens to sit near the edge.
    ///
    /// The layout below mirrors the real one in [`draw`] (tab rail, separator, the
    /// same `ScrollArea` configuration); if that changes, this test has to change
    /// with it.
    #[test]
    fn the_page_scrolls_instead_of_being_clipped_by_the_sheet() {
        use std::cell::Cell;
        use std::rc::Rc;

        /// The height the settings window is allowed on a 780 pt screen.
        const SHEET: f32 = 620.0;
        /// A page taller than the sheet, which is the whole point.
        const ROWS: usize = 40;

        let ctx = egui::Context::default();
        let viewport = Rc::new(Cell::new(0.0f32));
        let content = Rc::new(Cell::new(0.0f32));

        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1280.0, 780.0),
        ));

        let _ = ctx.run(input, |ctx| {
            egui::Window::new("设置")
                .default_size([860.0, SHEET])
                .max_size([860.0, SHEET])
                .min_size([620.0, 440.0])
                .show(ctx, |ui| {
                    ui.horizontal_top(|ui| {
                        ui.vertical(|ui| {
                            ui.set_width(132.0);
                            for _ in 0..7 {
                                ui.allocate_exact_size(
                                    egui::vec2(132.0, 34.0),
                                    egui::Sense::click(),
                                );
                            }
                        });
                        ui.separator();
                        let out = egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .scroll_source(egui::containers::scroll_area::ScrollSource {
                                drag: false,
                                ..Default::default()
                            })
                            .show(ui, |ui| {
                                ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                                    ui.set_min_width(ui.available_width());
                                    for _ in 0..ROWS {
                                        ui.allocate_space(egui::vec2(ui.available_width(), 26.0));
                                    }
                                });
                            });
                        viewport.set(out.inner_rect.height());
                        content.set(out.content_size.y);
                    });
                });
        });

        assert!(viewport.get() > 0.0, "the page must have been laid out");
        assert!(
            viewport.get() <= SHEET,
            "the page viewport is {} pt tall inside a {SHEET} pt sheet: it was handed \
             the screen's height, so it can never scroll and the sheet clips it",
            viewport.get()
        );
        assert!(
            content.get() > viewport.get(),
            "the test page must overflow its viewport ({} vs {})",
            content.get(),
            viewport.get()
        );
    }
}
