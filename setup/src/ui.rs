//! The setup window (REL-07), in the same design as the other apps' installers: one plain
//! page per step (welcome, custom options, progress, result) in Windows 11 colours that
//! follow the system light or dark theme. "Install now" is the only choice by default;
//! "Custom options" sets the folder, shortcuts, start at login and starting the program.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui::{
    self, Align, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Layout, Margin, Rect, Response,
    RichText, Sense, Shape, Stroke, StrokeKind, TextStyle, Ui, pos2, vec2,
};

use crate::dialogs;
use crate::install::{Report, Stage};
use crate::manifest::Tasks;
use crate::text::tr;

const OK: i32 = 0;
const CANCELLED: i32 = 2;
const FAILED: i32 = 3;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Install,
    Uninstall,
}

/// What the user chose.
pub struct Choice {
    pub dir: PathBuf,
    pub tasks: Tasks,
    /// Start the program when done (silent progress windows; the interactive window starts
    /// it itself from the result page).
    pub launch: bool,
    pub delete_data: bool,
}

pub enum Done {
    Installed { previous: Option<String>, warnings: Vec<String> },
    Uninstalled { warnings: Vec<String> },
}

/// A failure shown on the result page: exit code and message.
pub type Failed = (i32, String);
pub type Work = Box<dyn FnOnce(Choice, &dyn Report) -> Result<Done, Failed> + Send>;
/// Checks a folder chosen for a new installation.
pub type CheckDir = Box<dyn Fn(&Path) -> Result<(), String>>;

/// Everything the window shows and checks; the product details come from its manifest.
pub struct View {
    pub role: Role,
    pub name: String,
    pub version: String,
    pub product_id: String,
    /// What the user data is called, e.g. "settings, note cards and saved keys".
    pub data_label: String,
    pub dir: PathBuf,
    /// Upgrades keep the current folder.
    pub dir_locked: bool,
    /// The version installed in `dir`.
    pub installed: Option<String>,
    /// A newer version is installed; continuing installs an older one.
    pub newer: Option<String>,
    pub tasks: Tasks,
    pub launch: bool,
    pub delete_data: bool,
    /// Progress only, closing by itself (`/SILENT`, `/UPDATE`).
    pub auto: bool,
    /// `/UPDATE`: the progress page says "updating".
    pub updating: bool,
    /// `/SUPPRESSMSGBOXES`: a failure closes the window instead of showing it.
    pub suppress: bool,
    /// The uninstaller of the installation in `dir`, offered on the welcome page.
    pub uninstaller: Option<PathBuf>,
    pub check_dir: CheckDir,
    pub running: Box<dyn Fn(&Path) -> bool>,
    pub launch_program: Box<dyn Fn(&Path)>,
    pub log_path: Option<PathBuf>,
}

pub struct Outcome {
    pub code: i32,
    /// The user chose to uninstall the existing version; open this after the window closed.
    pub open_uninstaller: Option<PathBuf>,
}

/// Shows the window until the user closes it. `Err` means it could not be shown at all.
pub fn run(view: View, work: Work) -> Result<Outcome, String> {
    let exit_code = Arc::new(AtomicI32::new(CANCELLED));
    let open_uninstaller = Arc::new(Mutex::new(None));
    let title = match view.role {
        Role::Install => format!("{} {}", view.name, tr("安装", "Setup")),
        Role::Uninstall => format!("{} {}", tr("卸载", "Uninstall"), view.name),
    };
    let icon = dialogs::icon_rgba(256);
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(&title)
        .with_inner_size([640.0, 460.0])
        .with_resizable(false)
        .with_maximize_button(false);
    if let Some((rgba, width, height)) = icon.clone() {
        viewport = viewport.with_icon(Arc::new(egui::IconData { rgba, width, height }));
    }
    let options = eframe::NativeOptions { viewport, centered: true, ..Default::default() };
    let app = App::new(view, work, icon, exit_code.clone(), open_uninstaller.clone());
    eframe::run_native(
        &title,
        options,
        Box::new(move |creation| {
            configure(&creation.egui_ctx);
            let mut app = app;
            if app.view.auto {
                app.start_auto(&creation.egui_ctx);
            }
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| e.to_string())?;
    let open = open_uninstaller.lock().unwrap_or_else(|e| e.into_inner()).take();
    Ok(Outcome { code: exit_code.load(Ordering::SeqCst), open_uninstaller: open })
}

// ------------------------------------------------------------------ theme

#[derive(Clone, Copy)]
struct Palette {
    window: Color32,
    panel: Color32,
    panel_alt: Color32,
    text: Color32,
    muted: Color32,
    border: Color32,
    strong_border: Color32,
    accent: Color32,
    accent_hover: Color32,
    accent_soft: Color32,
    on_accent: Color32,
    danger: Color32,
    warning_soft: Color32,
    success: Color32,
}

fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            window: Color32::from_rgb(32, 32, 32),
            panel: Color32::from_rgb(45, 45, 45),
            panel_alt: Color32::from_rgb(39, 39, 39),
            text: Color32::from_rgb(245, 245, 245),
            muted: Color32::from_rgb(180, 180, 180),
            border: Color32::from_rgb(58, 58, 58),
            strong_border: Color32::from_rgb(110, 110, 110),
            accent: Color32::from_rgb(96, 205, 255),
            accent_hover: Color32::from_rgb(86, 185, 230),
            accent_soft: Color32::from_rgb(36, 62, 77),
            on_accent: Color32::from_rgb(20, 20, 20),
            danger: Color32::from_rgb(255, 153, 164),
            warning_soft: Color32::from_rgb(67, 53, 25),
            success: Color32::from_rgb(108, 203, 95),
        }
    } else {
        Palette {
            window: Color32::WHITE,
            panel: Color32::WHITE,
            panel_alt: Color32::from_rgb(243, 243, 243),
            text: Color32::from_rgb(26, 26, 26),
            muted: Color32::from_rgb(96, 96, 96),
            border: Color32::from_rgb(229, 229, 229),
            strong_border: Color32::from_rgb(138, 138, 138),
            accent: Color32::from_rgb(0, 120, 212),
            accent_hover: Color32::from_rgb(26, 134, 217),
            accent_soft: Color32::from_rgb(230, 241, 251),
            on_accent: Color32::WHITE,
            danger: Color32::from_rgb(196, 43, 28),
            warning_soft: Color32::from_rgb(255, 244, 206),
            success: Color32::from_rgb(15, 123, 15),
        }
    }
}

fn colors(ui: &Ui) -> Palette {
    palette(ui.visuals().dark_mode)
}

fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

/// Segoe UI and Microsoft YaHei from the Windows font folder, light and dark styles.
fn configure(ctx: &egui::Context) {
    let fonts_dir =
        std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows")).join("Fonts");
    let mut fonts = FontDefinitions::default();
    let mut load = |key: &str, files: &[&str]| -> bool {
        for file in files {
            if let Ok(bytes) = std::fs::read(fonts_dir.join(file)) {
                fonts.font_data.insert(key.into(), Arc::new(FontData::from_owned(bytes)));
                return true;
            }
        }
        false
    };
    let regular: Vec<&str> = [
        load("ui", &["SegUIVar.ttf", "segoeui.ttf"]).then_some("ui"),
        load("cjk", &["msyh.ttc", "simhei.ttf", "simsun.ttc"]).then_some("cjk"),
    ]
    .into_iter()
    .flatten()
    .collect();
    let strong: Vec<&str> = [
        load("ui-bold", &["seguisb.ttf", "segoeuib.ttf"]).then_some("ui-bold"),
        load("cjk-bold", &["msyhbd.ttc", "msyh.ttc"]).then_some("cjk-bold"),
    ]
    .into_iter()
    .flatten()
    .collect();
    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    for (index, key) in regular.iter().enumerate() {
        proportional.insert(index, key.to_string());
    }
    let fallback = proportional.clone();
    let family = fonts.families.entry(FontFamily::Name("bold".into())).or_default();
    family.extend(strong.iter().map(|key| key.to_string()));
    family.extend(fallback);
    ctx.set_fonts(fonts);

    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        let p = palette(theme == egui::Theme::Dark);
        ctx.style_mut_of(theme, |style| {
            style.text_styles.insert(TextStyle::Heading, bold(22.0));
            style.text_styles.insert(TextStyle::Body, FontId::proportional(14.0));
            style.text_styles.insert(TextStyle::Button, FontId::proportional(14.0));
            style.text_styles.insert(TextStyle::Small, FontId::proportional(12.0));
            style.spacing.item_spacing = vec2(8.0, 8.0);
            style.spacing.interact_size = vec2(32.0, 32.0);
            style.spacing.button_padding = vec2(12.0, 6.0);
            let v = &mut style.visuals;
            v.override_text_color = Some(p.text);
            v.panel_fill = p.window;
            v.window_fill = p.panel;
            v.extreme_bg_color = p.panel;
            v.faint_bg_color = p.panel_alt;
            v.selection.bg_fill = p.accent_soft;
            v.selection.stroke = Stroke::new(1.5_f32, p.accent);
            v.text_cursor.stroke = Stroke::new(1.5_f32, p.accent);
            for visual in [
                &mut v.widgets.noninteractive,
                &mut v.widgets.inactive,
                &mut v.widgets.hovered,
                &mut v.widgets.active,
                &mut v.widgets.open,
            ] {
                visual.corner_radius = CornerRadius::same(4);
                visual.expansion = 0.0;
                visual.bg_stroke = Stroke::new(1.0_f32, p.border);
                visual.fg_stroke = Stroke::new(1.0_f32, p.text);
                visual.bg_fill = p.panel;
                visual.weak_bg_fill = p.panel;
            }
            v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, p.strong_border);
            v.widgets.active.bg_stroke = Stroke::new(1.0_f32, p.accent);
        });
    }
}

// ---------------------------------------------------------------- widgets

fn pointer_if_hovered(ui: &Ui, response: &Response) {
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
}

/// A Windows 11 button; primary buttons use the accent colour.
fn button(ui: &mut Ui, label: &str, primary: bool, size: egui::Vec2) -> Response {
    let p = colors(ui);
    let fg = if primary { p.on_accent } else { p.text };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), FontId::proportional(14.0), fg);
    let size = vec2(size.x.max(galley.size().x + 32.0), size.y);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let hovered = response.hovered();
    let (fill, stroke) = if primary {
        let fill = if response.is_pointer_button_down_on() {
            p.accent.gamma_multiply(0.85)
        } else if hovered {
            p.accent_hover
        } else {
            p.accent
        };
        (fill, Stroke::NONE)
    } else {
        (
            if hovered { p.panel_alt } else { p.panel },
            Stroke::new(1.0_f32, if hovered { p.strong_border } else { p.border }),
        )
    };
    ui.painter().rect(rect, CornerRadius::same(4), fill, stroke, StrokeKind::Inside);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, fg);
    pointer_if_hovered(ui, &response);
    response
}

/// Buttons side by side, centred; returns the index of a clicked button.
fn button_row(ui: &mut Ui, buttons: &[(&str, bool)], size: egui::Vec2) -> Option<usize> {
    let gap = 12.0;
    let width = buttons.len() as f32 * size.x + (buttons.len() as f32 - 1.0) * gap;
    let mut clicked = None;
    ui.allocate_ui_with_layout(vec2(width, size.y), Layout::left_to_right(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (index, (label, primary)) in buttons.iter().enumerate() {
            if button(ui, label, *primary, size).clicked() {
                clicked = Some(index);
            }
        }
    });
    clicked
}

/// A plain muted text link for a secondary action.
fn text_link(ui: &mut Ui, label: &str) -> Response {
    let p = colors(ui);
    let galley = ui.painter().layout_no_wrap(label.to_owned(), FontId::proportional(12.5), Color32::PLACEHOLDER);
    let (rect, response) = ui.allocate_exact_size(galley.size() + vec2(0.0, 6.0), Sense::click());
    let color = if response.hovered() { p.accent_hover } else { p.muted };
    ui.painter().galley(pos2(rect.left(), rect.center().y - galley.size().y / 2.0), galley, color);
    pointer_if_hovered(ui, &response);
    response
}

/// A quiet accent link with a chevron in a circle, e.g. "⌄ 自定义选项".
fn options_link(ui: &mut Ui, label: &str, down: bool) -> Response {
    let p = colors(ui);
    let galley = ui.painter().layout_no_wrap(label.to_owned(), FontId::proportional(13.0), p.accent);
    let icon = 14.0;
    let size = vec2(icon + 6.0 + galley.size().x, galley.size().y.max(icon) + 8.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let color = if response.hovered() { p.accent_hover } else { p.accent };
    let center = pos2(rect.left() + icon / 2.0, rect.center().y);
    let stroke = Stroke::new(1.3_f32, color);
    ui.painter().circle_stroke(center, icon / 2.0 - 0.5, stroke);
    let sign = if down { 1.0 } else { -1.0 };
    ui.painter().add(Shape::line(
        vec![center + vec2(-3.0, -1.5 * sign), center + vec2(0.0, 1.5 * sign), center + vec2(3.0, -1.5 * sign)],
        stroke,
    ));
    ui.painter().galley(pos2(rect.left() + icon + 6.0, rect.center().y - galley.size().y / 2.0), galley, color);
    pointer_if_hovered(ui, &response);
    response
}

fn checkbox(ui: &mut Ui, checked: &mut bool, label: &str, size: f32) -> Response {
    let p = colors(ui);
    let galley = ui.painter().layout_no_wrap(label.to_owned(), FontId::proportional(size), p.text);
    let box_size = (size + 4.0).round();
    let row = vec2(box_size + 10.0 + galley.size().x, box_size.max(galley.size().y) + 8.0);
    let (rect, mut response) = ui.allocate_exact_size(row, Sense::click());
    if response.clicked() {
        *checked = !*checked;
        response.mark_changed();
    }
    let hovered = response.hovered();
    let square = Rect::from_center_size(pos2(rect.left() + box_size / 2.0, rect.center().y), vec2(box_size, box_size));
    if *checked {
        let fill = if hovered { p.accent_hover } else { p.accent };
        ui.painter().rect_filled(square, CornerRadius::same(4), fill);
        let at = |x: f32, y: f32| square.min + vec2(x, y) * (box_size / 18.0);
        ui.painter()
            .add(Shape::line(vec![at(4.5, 9.5), at(7.5, 12.5), at(13.5, 5.5)], Stroke::new(1.8_f32, p.on_accent)));
    } else {
        ui.painter().rect(
            square,
            CornerRadius::same(4),
            if hovered { p.panel_alt } else { p.panel },
            Stroke::new(1.0_f32, p.strong_border),
            StrokeKind::Inside,
        );
    }
    ui.painter().galley(pos2(square.right() + 10.0, rect.center().y - galley.size().y / 2.0), galley, p.text);
    pointer_if_hovered(ui, &response);
    response
}

/// A short warning in a soft pill, centred under the main button.
fn notice(ui: &mut Ui, text: &str) {
    let p = colors(ui);
    egui::Frame::new()
        .fill(p.warning_soft)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(12, 7))
        .show(ui, |ui| {
            ui.set_max_width(440.0);
            ui.add(egui::Label::new(RichText::new(text).size(12.5).color(p.text)).wrap());
        });
}

fn progress_bar(ui: &mut Ui, fraction: f32, width: f32) {
    let p = colors(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 4.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(2), p.border);
    let mut filled = rect;
    filled.set_width(rect.width() * fraction.clamp(0.0, 1.0));
    if filled.width() > 0.5 {
        ui.painter().rect_filled(filled, CornerRadius::same(2), p.accent);
    }
}

#[derive(Clone, Copy)]
enum Badge {
    Success,
    Error,
}

fn badge(ui: &mut Ui, kind: Badge) {
    let p = colors(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(72.0, 72.0), Sense::hover());
    let center = rect.center();
    let color = match kind {
        Badge::Success => p.success,
        Badge::Error => p.danger,
    };
    ui.painter().circle_filled(center, 32.0, color);
    let stroke = Stroke::new(3.5_f32, if ui.visuals().dark_mode { p.window } else { Color32::WHITE });
    match kind {
        Badge::Success => {
            ui.painter().add(Shape::line(
                vec![center + vec2(-12.0, 0.5), center + vec2(-4.0, 8.5), center + vec2(13.0, -9.0)],
                stroke,
            ));
        }
        Badge::Error => {
            ui.painter().line_segment([center + vec2(0.0, -14.0), center + vec2(0.0, 4.0)], stroke);
            ui.painter().circle_filled(center + vec2(0.0, 13.0), 2.5, stroke.color);
        }
    }
}

/// The product name, centred, with its version tag centred below it.
fn title_with_tag(ui: &mut Ui, title: &str, tag: &str) {
    let p = colors(ui);
    let name = ui.painter().layout_no_wrap(title.to_owned(), bold(26.0), p.text);
    let label = ui.painter().layout_no_wrap(tag.to_owned(), FontId::proportional(12.5), p.muted);
    let tag_size = label.size() + vec2(14.0, 6.0);
    let gap = 6.0;
    let size = vec2(name.size().x.max(tag_size.x), name.size().y + gap + tag_size.y);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().galley(pos2(rect.center().x - name.size().x / 2.0, rect.top()), name.clone(), p.text);
    let tag_rect =
        Rect::from_min_size(pos2(rect.center().x - tag_size.x / 2.0, rect.top() + name.size().y + gap), tag_size);
    ui.painter().rect_filled(tag_rect, CornerRadius::same(5), p.panel_alt);
    ui.painter().galley(tag_rect.center() - label.size() / 2.0, label, p.muted);
}

fn heading(ui: &mut Ui, text: &str) {
    let p = colors(ui);
    ui.label(RichText::new(text).font(bold(22.0)).color(p.text));
}

fn caption(ui: &mut Ui, text: &str, size: f32) {
    let p = colors(ui);
    ui.add(egui::Label::new(RichText::new(text).size(size).color(p.muted)).wrap());
}

fn window_handle() -> isize {
    // SAFETY: GetActiveWindow only reads the calling thread's active window.
    unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow() }.0 as isize
}

// -------------------------------------------------------------------- app

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Welcome,
    /// Install location and options ("自定义选项").
    Options,
    Working,
    Finished,
    Failed,
}

#[derive(Default)]
struct Shared {
    fraction: f32,
    status: String,
    finished: Option<Result<Done, Failed>>,
}

/// Turns install and uninstall steps into the progress bar.
struct Progress {
    role: Role,
    name: String,
    shared: Arc<Mutex<Shared>>,
    ctx: egui::Context,
}

impl Report for Progress {
    fn progress(&self, stage: Stage, done: u64, total: u64) {
        let part = done as f32 / total.max(1) as f32;
        let (fraction, status) = match (self.role, stage) {
            (Role::Install, Stage::Extracting) => {
                (0.02 + 0.83 * part, tr("正在解压文件", "Extracting files").to_string())
            }
            (_, Stage::Closing) => (
                if self.role == Role::Install { 0.86 } else { 0.05 },
                format!("{}{}", tr("正在关闭", "Closing "), self.name),
            ),
            (Role::Install, Stage::Replacing) => (0.9, tr("正在替换程序文件", "Replacing program files").to_string()),
            (Role::Uninstall, Stage::Replacing) => {
                (0.1 + 0.8 * part, tr("正在删除程序文件", "Removing program files").to_string())
            }
            (Role::Install, Stage::Finishing) => (0.96, tr("正在登记安装信息", "Registering").to_string()),
            (Role::Uninstall, _) => (0.95, tr("正在清理", "Cleaning up").to_string()),
        };
        if let Ok(mut shared) = self.shared.lock() {
            shared.fraction = fraction;
            shared.status = status;
        }
        self.ctx.request_repaint();
    }

    fn cancelled(&self) -> bool {
        false
    }

    /// The user already agreed to close the program before the work started.
    fn force_close(&self, _: &[String]) -> bool {
        true
    }
}

struct App {
    view: View,
    work: Option<Work>,
    exit_code: Arc<AtomicI32>,
    open_uninstaller: Arc<Mutex<Option<PathBuf>>>,
    icon_rgba: Option<(Vec<u8>, u32, u32)>,
    icon: Option<egui::TextureHandle>,
    page: Page,
    shared: Arc<Mutex<Shared>>,
    dir_text: String,
    dir_error: Option<String>,
    tasks: Tasks,
    delete_data: bool,
    close_approved: bool,
    notice: Option<String>,
    launch_now: bool,
    done: Option<Done>,
    failure: Option<Failed>,
}

impl App {
    fn new(
        view: View,
        work: Work,
        icon_rgba: Option<(Vec<u8>, u32, u32)>,
        exit_code: Arc<AtomicI32>,
        open_uninstaller: Arc<Mutex<Option<PathBuf>>>,
    ) -> App {
        App {
            dir_text: view.dir.display().to_string(),
            tasks: view.tasks,
            delete_data: view.delete_data,
            launch_now: view.launch,
            view,
            work: Some(work),
            exit_code,
            open_uninstaller,
            icon_rgba,
            icon: None,
            page: Page::Welcome,
            shared: Arc::default(),
            dir_error: None,
            close_approved: false,
            notice: None,
            done: None,
            failure: None,
        }
    }

    fn name(&self) -> &str {
        &self.view.name
    }

    fn running_notice(&self) -> String {
        format!(
            "{}{}",
            self.name(),
            tr(
                " 正在运行。继续将先请它正常退出，必要时再结束它。",
                " is running. Continuing asks it to exit first and ends it if needed."
            )
        )
    }

    fn spawn_work(&mut self, ctx: &egui::Context, choice: Choice) {
        let Some(work) = self.work.take() else { return };
        let progress = Progress {
            role: self.view.role,
            name: self.view.name.clone(),
            shared: self.shared.clone(),
            ctx: ctx.clone(),
        };
        self.page = Page::Working;
        std::thread::spawn(move || {
            let result = work(choice, &progress);
            if let Ok(mut shared) = progress.shared.lock() {
                shared.finished = Some(result);
            }
            progress.ctx.request_repaint();
        });
    }

    /// `/SILENT` and `/UPDATE`: start at once with the prepared choices.
    fn start_auto(&mut self, ctx: &egui::Context) {
        let choice = Choice {
            dir: self.view.dir.clone(),
            tasks: self.tasks,
            launch: self.view.launch,
            delete_data: self.delete_data,
        };
        self.spawn_work(ctx, choice);
    }

    fn close(&self, ctx: &egui::Context, code: i32) {
        self.exit_code.store(code, Ordering::SeqCst);
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// The main action: install (or uninstall).
    fn proceed(&mut self, ctx: &egui::Context) {
        let dir = PathBuf::from(self.dir_text.trim());
        if self.view.role == Role::Install && !self.view.dir_locked {
            if let Err(error) = (self.view.check_dir)(&dir) {
                // The location is edited under the custom options.
                self.dir_error = Some(error);
                self.page = Page::Options;
                return;
            }
        }
        self.dir_error = None;
        if (self.view.running)(&dir) && !self.close_approved {
            self.close_approved = true;
            self.notice = Some(self.running_notice());
            return;
        }
        let choice = Choice { dir, tasks: self.tasks, launch: false, delete_data: self.delete_data };
        self.spawn_work(ctx, choice);
    }

    /// Leave the custom options, keeping the choices (the folder must be valid).
    fn collapse_options(&mut self) {
        if !self.view.dir_locked {
            if let Err(error) = (self.view.check_dir)(Path::new(self.dir_text.trim())) {
                self.dir_error = Some(error);
                return;
            }
        }
        self.dir_error = None;
        self.page = Page::Welcome;
    }

    fn finish(&mut self, ctx: &egui::Context) {
        if self.view.role == Role::Install && self.launch_now {
            (self.view.launch_program)(Path::new(self.dir_text.trim()));
        }
        self.close(ctx, OK);
    }

    fn close_failed(&self, ctx: &egui::Context) {
        self.close(ctx, self.failure.as_ref().map_or(FAILED, |f| f.0));
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let finished = self.shared.lock().ok().and_then(|mut shared| shared.finished.take());
        let Some(result) = finished else { return };
        match result {
            Ok(done) => {
                self.exit_code.store(OK, Ordering::SeqCst);
                if self.view.auto {
                    self.close(ctx, OK);
                }
                self.done = Some(done);
                self.page = Page::Finished;
            }
            Err(failure) => {
                self.exit_code.store(failure.0, Ordering::SeqCst);
                if self.view.auto && self.view.suppress {
                    self.close(ctx, failure.0);
                }
                self.failure = Some(failure);
                self.page = Page::Failed;
            }
        }
    }

    /// The program icon with a soft shadow under its rounded tile.
    fn logo(&mut self, ui: &mut Ui, size: f32) {
        let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
        if self.icon.is_none() {
            self.icon = self.icon_rgba.as_ref().map(|(rgba, width, height)| {
                ui.ctx().load_texture(
                    "icon",
                    egui::ColorImage::from_rgba_unmultiplied([*width as usize, *height as usize], rgba),
                    egui::TextureOptions::LINEAR,
                )
            });
        }
        let tile = rect.shrink(size * 0.03);
        let shadow = egui::Shadow {
            offset: [0, 6],
            blur: 24,
            spread: 0,
            color: Color32::from_black_alpha(if ui.visuals().dark_mode { 110 } else { 34 }),
        };
        ui.painter().add(shadow.as_shape(tile, CornerRadius::same((size * 0.22) as u8)));
        if let Some(icon) = &self.icon {
            ui.painter().image(icon.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
    }

    fn installed_here(&self) -> Option<&str> {
        self.view.installed.as_deref()
    }

    fn install_label(&self) -> &'static str {
        match (&self.notice, &self.view.newer, self.installed_here()) {
            (Some(_), _, _) => tr("关闭并安装", "Close and install"),
            (_, Some(_), _) => tr("仍然安装", "Install anyway"),
            (_, _, Some(v)) if v == self.view.version => tr("重新安装", "Reinstall"),
            (_, _, Some(_)) => tr("立即升级", "Upgrade now"),
            _ => tr("立即安装", "Install now"),
        }
    }

    /// One quiet line under the main button: what will happen, or a warning.
    fn install_note(&self, ui: &mut Ui) {
        let (version, label) = (&self.view.version, &self.view.data_label);
        if let Some(text) = &self.notice {
            notice(ui, text);
        } else if let Some(newer) = &self.view.newer {
            notice(
                ui,
                &format!(
                    "{}{newer}{}{version}{}",
                    tr("已安装较新的 v", "A newer v"),
                    tr("，继续将替换为较旧的 v", " is installed; continuing replaces it with the older v"),
                    tr("。", ".")
                ),
            );
        } else if let Some(installed) = self.installed_here() {
            let text = if installed == version {
                format!(
                    "{}{label}{}",
                    tr("将修复当前安装，", "Repairs this installation; "),
                    tr("都会保留。", " are kept.")
                )
            } else {
                format!(
                    "{}{installed}{}{label}{}",
                    tr("将从 v", "Upgrades from v"),
                    tr(" 升级，", "; "),
                    tr("都会保留。", " are kept.")
                )
            };
            caption(ui, &text, 12.5);
        }
    }

    /// When the folder already has the program, its own uninstaller is offered at the
    /// right end of the footer row.
    fn uninstall_link(&mut self, ui: &mut Ui, ctx: &egui::Context, row: Rect) {
        let (Some(version), Some(uninstaller)) = (self.view.installed.clone(), self.view.uninstaller.clone()) else {
            return;
        };
        let area = Rect::from_x_y_ranges(ui.max_rect().x_range(), row.y_range());
        let clicked = ui
            .scope_builder(egui::UiBuilder::new().max_rect(area).layout(Layout::right_to_left(Align::Center)), |ui| {
                text_link(ui, &format!("{}{version}", tr("卸载 v", "Uninstall v")))
                    .on_hover_text(tr(
                        "打开已安装版本的卸载程序，可以选择是否同时删除数据。",
                        "Opens the installed version's uninstaller, which asks whether to delete the data too.",
                    ))
                    .clicked()
            })
            .inner;
        if clicked {
            if let Ok(mut slot) = self.open_uninstaller.lock() {
                *slot = Some(uninstaller);
            }
            self.close(ctx, CANCELLED);
        }
    }

    fn welcome_page(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let p = colors(ui);
        ui.add_space(64.0);
        self.logo(ui, 112.0);
        ui.add_space(22.0);
        match self.view.role {
            Role::Install => {
                let name = self.view.name.clone();
                title_with_tag(ui, &name, &self.view.version);
                ui.add_space(22.0);
                if button(ui, self.install_label(), true, vec2(180.0, 40.0)).clicked() {
                    self.proceed(ctx);
                }
                ui.add_space(16.0);
                self.install_note(ui);
            }
            Role::Uninstall => {
                title_with_tag(ui, &format!("{}{}", tr("卸载 ", "Uninstall "), self.name()), &self.view.version);
                ui.add_space(4.0);
                ui.add(egui::Label::new(RichText::new(&self.dir_text).size(12.5).color(p.muted)).truncate());
                ui.add_space(18.0);
                let label = format!("{}{}", tr("同时删除", "Also delete "), self.view.data_label);
                checkbox(ui, &mut self.delete_data, &label, 12.5).on_hover_text(tr(
                    "删除后无法恢复。不勾选则保留，重新安装后可继续使用。",
                    "This cannot be undone. Left unticked, they stay and work again after reinstalling.",
                ));
                ui.add_space(12.0);
                let primary = if self.notice.is_some() {
                    tr("关闭并卸载", "Close and uninstall")
                } else {
                    tr("卸载", "Uninstall")
                };
                match button_row(ui, &[(tr("取消", "Cancel"), false), (primary, true)], vec2(140.0, 40.0)) {
                    Some(0) => self.close(ctx, CANCELLED),
                    Some(_) => self.proceed(ctx),
                    None => {}
                }
                if let Some(text) = self.notice.clone() {
                    ui.add_space(14.0);
                    notice(ui, &text);
                }
            }
        }
    }

    fn options_page(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let p = colors(ui);
        ui.add_space(40.0);
        heading(ui, tr("自定义选项", "Custom options"));
        ui.add_space(22.0);
        ui.allocate_ui_with_layout(vec2(440.0, 200.0), Layout::top_down(Align::Min), |ui| {
            ui.label(RichText::new(tr("安装位置", "Install location")).size(12.0).color(p.muted));
            ui.add_space(2.0);
            if self.view.dir_locked {
                egui::Frame::new()
                    .fill(p.panel_alt)
                    .stroke(Stroke::new(1.0_f32, p.border))
                    .corner_radius(CornerRadius::same(4))
                    .inner_margin(Margin::symmetric(10, 7))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.add(egui::Label::new(RichText::new(&self.dir_text).color(p.text)).truncate());
                    });
                caption(ui, tr("升级时沿用当前安装位置。", "Upgrades keep the current location."), 12.0);
            } else {
                ui.horizontal(|ui| {
                    let edit = egui::TextEdit::singleline(&mut self.dir_text)
                        .desired_width(ui.available_width() - 96.0 - 8.0)
                        .margin(Margin::symmetric(8, 7))
                        .font(FontId::proportional(14.0));
                    if ui.add(edit).changed() {
                        self.dir_error = None;
                    }
                    if button(ui, tr("浏览…", "Browse…"), false, vec2(96.0, 32.0)).clicked() {
                        let title = format!(
                            "{}{}{}",
                            tr("选择", "Choose where to install "),
                            self.view.name,
                            tr("的安装位置", "")
                        );
                        if let Some(folder) = dialogs::browse_folder(window_handle(), &title) {
                            let id = &self.view.product_id;
                            let named =
                                folder.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(id));
                            let chosen = if named { folder } else { folder.join(id) };
                            self.dir_text = chosen.display().to_string();
                            self.dir_error = None;
                        }
                    }
                });
            }
            if let Some(error) = &self.dir_error {
                ui.add(egui::Label::new(RichText::new(error).size(12.5).color(p.danger)).wrap());
            }
            ui.add_space(12.0);
            let launch_label =
                format!("{}{}", tr("安装完成后运行 ", "Start "), self.view.name) + tr("", " when finished");
            egui::Grid::new("options").num_columns(2).spacing(vec2(24.0, 0.0)).show(ui, |ui| {
                checkbox(ui, &mut self.tasks.start_menu, tr("创建开始菜单快捷方式", "Start menu shortcut"), 13.0);
                checkbox(ui, &mut self.tasks.desktop, tr("创建桌面快捷方式", "Desktop shortcut"), 13.0);
                ui.end_row();
                checkbox(ui, &mut self.tasks.startup, tr("开机自动启动", "Start when I sign in"), 13.0);
                checkbox(ui, &mut self.launch_now, &launch_label, 13.0);
                ui.end_row();
            });
        });
        ui.add_space(22.0);
        if button(ui, self.install_label(), true, vec2(180.0, 40.0)).clicked() {
            self.proceed(ctx);
        }
        if self.notice.is_some() || self.view.newer.is_some() {
            ui.add_space(12.0);
            self.install_note(ui);
        }
    }

    fn working_page(&mut self, ui: &mut Ui) {
        let (fraction, status) =
            self.shared.lock().map(|shared| (shared.fraction, shared.status.clone())).unwrap_or_default();
        let verb = match self.view.role {
            Role::Install if self.view.updating => tr("正在更新 ", "Updating "),
            Role::Install if self.installed_here().is_some() => tr("正在升级 ", "Upgrading "),
            Role::Install => tr("正在安装 ", "Installing "),
            Role::Uninstall => tr("正在卸载 ", "Uninstalling "),
        };
        ui.add_space(64.0);
        self.logo(ui, 112.0);
        ui.add_space(26.0);
        heading(ui, &format!("{verb}{}", self.name()));
        ui.add_space(4.0);
        let status = if status.is_empty() { tr("正在准备", "Preparing").to_string() } else { status };
        caption(ui, &format!("{status} · {:.0}%", fraction * 100.0), 13.0);
        ui.add_space(26.0);
        progress_bar(ui, fraction, 360.0);
    }

    fn result_lines(&self) -> Option<(Badge, String, Vec<String>)> {
        let (name, version, label) = (self.name(), &self.view.version, &self.view.data_label);
        let note = |warning: &String| format!("{}{warning}", tr("注意：", "Note: "));
        match (&self.done, &self.failure) {
            (Some(Done::Installed { previous, warnings }), _) => {
                let first = match previous {
                    Some(old) if old != version => format!(
                        "{name}{}{old}{}{version}{}{label}{}",
                        tr(" 已从 v", " was upgraded from v"),
                        tr(" 升级到 v", " to v"),
                        tr("，", "; "),
                        tr("均已保留。", " were kept.")
                    ),
                    Some(_) => format!(
                        "{name} v{version}{}{label}{}",
                        tr(" 已重新安装，", " was reinstalled; "),
                        tr("均已保留。", " were kept.")
                    ),
                    None => format!(
                        "{name} v{version}{}{}{}",
                        tr(" 已安装到 ", " was installed in "),
                        self.dir_text,
                        tr("。", ".")
                    ),
                };
                let title = if previous.is_some() {
                    tr("升级完成", "Upgrade complete")
                } else {
                    tr("安装完成", "Installation complete")
                };
                let mut lines = vec![first];
                lines.extend(warnings.iter().map(note));
                Some((Badge::Success, title.to_string(), lines))
            }
            (Some(Done::Uninstalled { warnings }), _) => {
                let first = if self.delete_data {
                    format!(
                        "{name}{}{label}{}",
                        tr(" 及其", " and its "),
                        tr("已从此电脑删除。", " were removed from this PC.")
                    )
                } else {
                    crate::text::sentence(&format!(
                        "{label}{}",
                        tr("已保留，重新安装后可继续使用。", " were kept and work again after reinstalling.")
                    ))
                };
                let mut lines = vec![first];
                lines.extend(warnings.iter().map(note));
                Some((Badge::Success, format!("{name}{}", tr(" 已卸载", " was uninstalled")), lines))
            }
            (None, Some((_, message))) => {
                let mut lines = vec![message.clone()];
                if let Some(path) = &self.view.log_path {
                    lines.push(format!("{}{}", tr("日志：", "Log: "), path.display()));
                }
                let title = match self.view.role {
                    Role::Install => tr("安装未完成", "Setup did not finish"),
                    Role::Uninstall => tr("卸载未完成", "Uninstalling did not finish"),
                };
                Some((Badge::Error, title.to_string(), lines))
            }
            (None, None) => None,
        }
    }

    fn result_page(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let p = colors(ui);
        let Some((kind, title, lines)) = self.result_lines() else { return };
        ui.add_space(56.0);
        badge(ui, kind);
        ui.add_space(20.0);
        heading(ui, &title);
        ui.add_space(6.0);
        ui.allocate_ui_with_layout(vec2(540.0, 72.0), Layout::top_down(Align::Center), |ui| {
            for (index, line) in lines.iter().enumerate() {
                let size = if index == 0 { 14.0 } else { 12.0 };
                ui.add(egui::Label::new(RichText::new(line).size(size).color(p.muted)).wrap());
            }
        });
        ui.add_space(16.0);
        if self.done.is_some() {
            if self.view.role == Role::Install {
                let label = format!("{}{}", tr("立即运行 ", "Start "), self.name()) + tr("", " now");
                checkbox(ui, &mut self.launch_now, &label, 12.5);
                ui.add_space(8.0);
            }
            if button(ui, tr("完成", "Finish"), true, vec2(180.0, 40.0)).clicked() {
                self.finish(ctx);
            }
        } else if button(ui, tr("关闭", "Close"), true, vec2(180.0, 40.0)).clicked() {
            self.close_failed(ctx);
        }
    }

    /// Only the interactive install pages have footer links.
    fn has_footer(&self) -> bool {
        !self.view.auto && self.view.role == Role::Install && matches!(self.page, Page::Welcome | Page::Options)
    }

    /// The centred "custom options" link, and the uninstall link at the right end of the row.
    fn footer(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let expand = self.page == Page::Welcome;
        let label = if expand { tr("自定义选项", "Custom options") } else { tr("收起选项", "Fewer options") };
        let link = ui.vertical_centered(|ui| options_link(ui, label, expand)).inner;
        if expand {
            self.uninstall_link(ui, ctx, link.rect);
        }
        if link.clicked() {
            if expand {
                self.page = Page::Options;
            } else {
                self.collapse_options();
            }
        }
    }

    /// Enter presses the main button and Esc cancels, as in a Windows dialog.
    fn keyboard(&mut self, ctx: &egui::Context) {
        let (enter, escape) =
            ctx.input(|input| (input.key_pressed(egui::Key::Enter), input.key_pressed(egui::Key::Escape)));
        match self.page {
            Page::Welcome | Page::Options if enter && !self.view.auto => self.proceed(ctx),
            Page::Welcome if escape => self.close(ctx, CANCELLED),
            Page::Options if escape => self.collapse_options(),
            Page::Finished if enter => self.finish(ctx),
            Page::Failed if enter || escape => self.close_failed(ctx),
            _ => {}
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.page == Page::Working {
            self.poll(ctx);
        }
        self.keyboard(ctx);
        if ctx.input(|input| input.viewport().close_requested()) {
            match self.page {
                // Files are being replaced; closing now would leave a half-finished state.
                Page::Working => ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose),
                Page::Welcome | Page::Options => self.exit_code.store(CANCELLED, Ordering::SeqCst),
                Page::Finished => self.exit_code.store(OK, Ordering::SeqCst),
                Page::Failed => self.exit_code.store(self.failure.as_ref().map_or(FAILED, |f| f.0), Ordering::SeqCst),
            }
        }
        let p = palette(ctx.style().visuals.dark_mode);
        if self.has_footer() {
            egui::TopBottomPanel::bottom("footer")
                .exact_height(64.0)
                .show_separator_line(false)
                .frame(egui::Frame::new().fill(p.window).inner_margin(Margin {
                    left: 22,
                    right: 22,
                    top: 0,
                    bottom: 22,
                }))
                .show(ctx, |ui| self.footer(ui, ctx));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.window).inner_margin(Margin::symmetric(32, 0)))
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| match self.page {
                    Page::Welcome => self.welcome_page(ui, ctx),
                    Page::Options => self.options_page(ui, ctx),
                    Page::Working => self.working_page(ui),
                    Page::Finished | Page::Failed => self.result_page(ui, ctx),
                });
            });
    }
}
