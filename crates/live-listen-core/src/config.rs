//! User settings: defaults, validation and atomic persistence as JSON.
//!
//! Secrets never live here; they are stored in Windows Credential Manager (see `secrets`).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::hotkey::Binding;

/// Current settings schema version. Version 2 removed translation, conversation records,
/// summaries and voiceprints (see [`Settings::removed_secrets`]); version 3 replaced the
/// preconnect switch with [`Preconnect`] levels. Versions 4 through 6 update the
/// default spoken-note shortcut to right Alt, Ctrl+Left, and Ctrl+Right, respectively.
pub const SETTINGS_VERSION: u32 = 6;

/// Built-in speech provider identifier.
pub const DOUBAO_ID: &str = "doubao";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub version: u32,
    pub dictation: DictationSettings,
    pub captions: CaptionSettings,
    pub hotkeys: HotkeySettings,
    pub services: ServiceSettings,
    pub appearance: AppearanceSettings,
    pub general: GeneralSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            dictation: DictationSettings::default(),
            captions: CaptionSettings::default(),
            hotkeys: HotkeySettings::default(),
            services: ServiceSettings::default(),
            appearance: AppearanceSettings::default(),
            general: GeneralSettings::default(),
        }
    }
}

/// How recognized text reaches the target text box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WriteMode {
    /// Write only text the service has finalized; never deletes anything.
    #[default]
    Stable,
    /// Write interim text immediately and correct it in place.
    Live,
}

/// What happens to the key or click that ends continuous dictation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EndAction {
    /// The key or click only ends dictation.
    #[default]
    EndOnly,
    /// After the last text is written, the key or click is replayed.
    EndAndReplay,
}

/// How eagerly a standby speech connection is kept ready so the first words arrive sooner.
/// Each level trades connections to the speech service for first-word latency.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Preconnect {
    /// Connect only when dictation starts.
    #[default]
    Off,
    /// Keep a connection ready for a short while after each dictation or hold-key press.
    Balanced,
    /// Keep a connection ready from start-up and for a long while after each dictation.
    Aggressive,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DictationSettings {
    pub write_mode: WriteMode,
    pub end_action: EndAction,
    /// Show live text in the floating bar; otherwise show only the animation.
    pub show_text: bool,
    /// Show the floating bar inside the window being typed into when dictation starts.
    pub follow_window: bool,
    /// Font size shared by dictation text and the floating bar's status messages.
    pub font_size: f32,
    /// Input device name; empty means the system default microphone.
    pub microphone: String,
    pub preconnect_level: Preconnect,
    /// The on/off switch of version 2, read only to migrate it in [`Settings::sanitize`].
    /// Never written, so older builds see preconnect off instead of a value they reject.
    #[serde(rename = "preconnect", skip_serializing)]
    legacy_preconnect: Option<bool>,
}

impl Default for DictationSettings {
    fn default() -> Self {
        Self {
            write_mode: WriteMode::Stable,
            end_action: EndAction::EndOnly,
            show_text: true,
            follow_window: true,
            font_size: 16.0,
            microphone: String::new(),
            preconnect_level: Preconnect::Off,
            legacy_preconnect: None,
        }
    }
}

/// Audio source for captions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceKind {
    Microphone,
    /// What the computer is playing (WASAPI loopback of the default output).
    #[default]
    System,
    /// Microphone and computer audio mixed.
    Mixed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CaptionSettings {
    pub source: SourceKind,
    pub lines: u8,
    pub font_size: f32,
    pub click_through: bool,
    pub width_percent: u8,
}

impl Default for CaptionSettings {
    fn default() -> Self {
        Self { source: SourceKind::System, lines: 2, font_size: 24.0, click_through: false, width_percent: 60 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HotkeySettings {
    /// Hold to talk, release to finish.
    pub hold: String,
    /// Start continuous dictation; any key or click ends it.
    pub toggle: String,
    /// Show or hide live captions.
    pub captions: String,
    /// Hold to record a spoken note, release to finish.
    pub note: String,
    /// Start or stop a spoken note like the tray entry.
    pub note_toggle: String,
    /// Create a blank desktop note without using the microphone.
    pub note_blank: String,
    /// Show or hide the cards on the desktop.
    pub cards: String,
}

impl Default for HotkeySettings {
    fn default() -> Self {
        Self {
            hold: "RCtrl".into(),
            toggle: "Ctrl+Alt+Space".into(),
            captions: "Ctrl+Alt+C".into(),
            note: "Ctrl+Right".into(),
            note_toggle: "Ctrl+Alt+N".into(),
            note_blank: "Ctrl+Alt+L".into(),
            cards: "Ctrl+Alt+K".into(),
        }
    }
}

impl HotkeySettings {
    /// Every shortcut field with its settings name.
    pub fn fields(&self) -> [(&'static str, &str); 7] {
        [
            ("hold", &self.hold),
            ("toggle", &self.toggle),
            ("captions", &self.captions),
            ("note", &self.note),
            ("cards", &self.cards),
            ("noteToggle", &self.note_toggle),
            ("noteBlank", &self.note_blank),
        ]
    }
}

/// A user-added official API or a compatible legacy stdio service.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CustomSpeechProvider {
    pub id: String,
    pub name: String,
    /// Official API family; empty/command retains existing stdio providers.
    pub provider_type: String,
    pub endpoint: String,
    pub model: String,
    pub app_id: String,
    pub resource_id: String,
    pub language: String,
    pub command: String,
    pub args: Vec<String>,
    /// Environment variable that receives the stored secret, if any.
    pub secret_env: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DoubaoSettings {
    /// Python interpreter; empty means search PATH.
    pub python: String,
    /// Folder that contains `doubao_asr`; empty means search next to the app.
    pub module_root: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ServiceSettings {
    /// Active speech provider id.
    pub speech: String,
    pub doubao: DoubaoSettings,
    pub custom_speech: Vec<CustomSpeechProvider>,
    /// Stop idle speech service processes after this many minutes.
    pub idle_release_minutes: u32,
}

impl Default for ServiceSettings {
    fn default() -> Self {
        Self {
            speech: DOUBAO_ID.into(),
            doubao: DoubaoSettings::default(),
            custom_speech: Vec::new(),
            idle_release_minutes: 5,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Shadow {
    None,
    #[default]
    Soft,
    Strong,
}

/// Visual style of one surface. Colors are `#AARRGGBB`; empty means the theme default.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SurfaceStyle {
    pub text: String,
    pub background: String,
    pub border: String,
    pub corner_radius: f32,
    pub shadow: Shadow,
    pub padding: f32,
    /// Desktop-card header layout; other surfaces retain but do not display these fields.
    pub card_header: String,
    /// Optional display date (YYYY-MM-DD), independent of the card's creation timestamp.
    pub card_header_date: String,
    /// Empty uses the localized card kind; otherwise a bounded single-line title.
    pub card_header_title: String,
}

impl Default for SurfaceStyle {
    fn default() -> Self {
        Self {
            text: String::new(),
            background: String::new(),
            border: String::new(),
            corner_radius: 8.0,
            shadow: Shadow::Soft,
            padding: 12.0,
            card_header: "title".into(),
            card_header_date: String::new(),
            card_header_title: String::new(),
        }
    }
}

impl SurfaceStyle {
    /// Dictation text starts without a painted background, outline, or shadow.
    pub fn bar_default() -> Self {
        Self { background: "#00000000".into(), border: "#00000000".into(), shadow: Shadow::None, ..Self::default() }
    }

    /// Clamps sizes and clears invalid colors; `name` prefixes the reported fields.
    pub fn sanitize(&mut self, name: &str) -> Vec<Issue> {
        let mut issues = Vec::new();
        self.corner_radius = finite_or(self.corner_radius, 8.0).clamp(0.0, 32.0);
        self.padding = finite_or(self.padding, 12.0).clamp(0.0, 48.0);
        if !matches!(
            self.card_header.as_str(),
            "none"
                | "yearMonthDay"
                | "monthDay"
                | "monthDayTime"
                | "monthDayWeekday"
                | "monthDayTitle"
                | "weekdayTitle"
                | "title"
        ) {
            issues.push(Issue { field: format!("{name}.cardHeader"), code: "invalid".into() });
            self.card_header = SurfaceStyle::default().card_header;
        }
        if !self.card_header_date.is_empty() && !valid_header_date(&self.card_header_date) {
            issues.push(Issue { field: format!("{name}.cardHeaderDate"), code: "invalid".into() });
            self.card_header_date.clear();
        }
        self.card_header_title = self
            .card_header_title
            .chars()
            .filter_map(|c| match c {
                '\r' | '\n' | '\t' => Some(' '),
                c if c.is_control() => None,
                c => Some(c),
            })
            .take(80)
            .collect();
        for (key, color) in
            [("text", &mut self.text), ("background", &mut self.background), ("border", &mut self.border)]
        {
            if !color.is_empty() && !is_argb(color) {
                issues.push(Issue { field: format!("{name}.{key}"), code: "invalid".into() });
                color.clear();
            }
        }
        issues
    }

    /// Default caption style: light text on a translucent dark panel.
    pub fn captions_default() -> Self {
        Self {
            text: "#FFFFFFFF".into(),
            background: "#B3202020".into(),
            border: "#00000000".into(),
            corner_radius: 8.0,
            shadow: Shadow::None,
            padding: 12.0,
            ..Self::default()
        }
    }
}

fn valid_header_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes.iter().enumerate().any(|(i, b)| i != 4 && i != 7 && !b.is_ascii_digit())
    {
        return false;
    }
    let year = value[..4].parse::<u32>().unwrap_or(0);
    let month = value[5..7].parse::<u32>().unwrap_or(0);
    let day = value[8..].parse::<u32>().unwrap_or(0);
    let last = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 400 == 0 || year % 4 == 0 && year % 100 != 0 => 29,
        2 => 28,
        _ => 0,
    };
    year > 0 && day > 0 && day <= last
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppearanceSettings {
    /// `system`, `light` or `dark`.
    pub theme: String,
    /// `system` or a BCP 47 tag (`zh-Hans`, `en`).
    pub language: String,
    /// User-selected dictation visualization: `orb` (default) or `ribbon`.
    pub voice_style: String,
    /// Uniform orb scale; caption font sizes are independent.
    pub voice_scale: f32,
    /// Uniform ribbon scale; 1.0 preserves the former default as the minimum.
    pub ribbon_scale: f32,
    /// Space between the dictation visualization and its text bubble.
    pub voice_text_gap: f32,
    /// Last surface and optional card selected in the appearance editor.
    pub style_surface: String,
    pub style_card: Option<u64>,
    #[serde(default = "SurfaceStyle::bar_default", deserialize_with = "deserialize_bar_style")]
    pub bar: SurfaceStyle,
    pub captions: SurfaceStyle,
    /// The style shared by all desktop cards; a card can have its own (see `cards::Card`).
    pub cards: SurfaceStyle,
}

/// Old partial overlay styles inherit the overlay default, without changing explicit shadows.
fn deserialize_bar_style<'de, D>(deserializer: D) -> Result<SurfaceStyle, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let mut value = serde_json::Value::deserialize(deserializer)?;
    if value.is_null() {
        return Ok(SurfaceStyle::bar_default());
    }
    if let Some(fields) = value.as_object_mut() {
        let empty_shadow = fields
            .get("shadow")
            .is_none_or(|shadow| shadow.is_null() || shadow.as_str().is_some_and(|text| text.trim().is_empty()));
        if empty_shadow {
            fields.insert("shadow".into(), serde_json::Value::String("none".into()));
        }
    }
    serde_json::from_value(value).map_err(serde::de::Error::custom)
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme: "system".into(),
            language: "system".into(),
            voice_style: "orb".into(),
            voice_scale: 1.5,
            ribbon_scale: 1.5,
            voice_text_gap: 8.0,
            style_surface: "bar".into(),
            style_card: None,
            bar: SurfaceStyle::bar_default(),
            captions: SurfaceStyle::captions_default(),
            cards: SurfaceStyle::default(),
        }
    }
}

/// Screen position in physical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GeneralSettings {
    pub start_with_windows: bool,
    /// Check release metadata once a day without downloading or installing updates.
    pub check_updates_daily: bool,
    pub bar_position: Option<Position>,
    /// Screen-space visual center for fixed-canvas scaling; old top-left remains compatible.
    pub bar_visual_center: Option<Position>,
    pub captions_position: Option<Position>,
    pub close_hint_shown: bool,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            start_with_windows: false,
            check_updates_daily: true,
            bar_position: None,
            bar_visual_center: None,
            captions_position: None,
            close_hint_shown: false,
        }
    }
}

/// A problem found while validating settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Issue {
    pub field: String,
    pub code: String,
}

impl Settings {
    /// Clamps numbers, fills empty required values and reports invalid entries.
    pub fn sanitize(&mut self) -> Vec<Issue> {
        let mut issues = Vec::new();
        let defaults = HotkeySettings::default();
        let old_note = Binding::parse(&self.hotkeys.note).ok();
        let default_note = Binding::parse(&defaults.note).ok();
        let legacy_note = (self.version == 5 && old_note == Binding::parse("Ctrl+Left").ok())
            || (self.version < 5 && old_note == Binding::parse("RAlt").ok())
            || (self.version < 4 && old_note == Binding::parse("Ctrl+Alt+N").ok());
        if legacy_note
            && !self
                .hotkeys
                .fields()
                .iter()
                .any(|(field, value)| *field != "note" && Binding::parse(value).ok() == default_note)
        {
            self.hotkeys.note.clone_from(&defaults.note);
        }
        self.version = SETTINGS_VERSION;
        let mut seen: Vec<Binding> = Vec::new();
        for (field, value, fallback) in [
            ("hotkeys.hold", &mut self.hotkeys.hold, defaults.hold),
            ("hotkeys.toggle", &mut self.hotkeys.toggle, defaults.toggle),
            ("hotkeys.captions", &mut self.hotkeys.captions, defaults.captions),
            ("hotkeys.note", &mut self.hotkeys.note, defaults.note),
            ("hotkeys.cards", &mut self.hotkeys.cards, defaults.cards),
            // Defaults for new actions must never displace an existing binding.
            ("hotkeys.noteToggle", &mut self.hotkeys.note_toggle, defaults.note_toggle),
            ("hotkeys.noteBlank", &mut self.hotkeys.note_blank, defaults.note_blank),
        ] {
            if value.trim().is_empty() {
                value.clear(); // An empty binding disables the shortcut.
                continue;
            }
            let binding = Binding::parse(value)
                .ok()
                .filter(|b| {
                    seen.contains(b) || !b.is_modifier_only() || matches!(field, "hotkeys.hold" | "hotkeys.note")
                })
                .unwrap_or_else(|| {
                    issues.push(Issue { field: field.into(), code: "invalid".into() });
                    Binding::parse(&fallback).expect("valid default hotkey")
                });
            if seen.contains(&binding) {
                issues.push(Issue { field: field.into(), code: "duplicate".into() });
                value.clear();
            } else {
                *value = binding.to_string();
                seen.push(binding);
            }
        }
        // "On" in version 2 was the ten-minute window that `Aggressive` keeps.
        if self.dictation.legacy_preconnect.take() == Some(true) && self.dictation.preconnect_level == Preconnect::Off {
            self.dictation.preconnect_level = Preconnect::Aggressive;
        }
        self.dictation.font_size = finite_or(self.dictation.font_size, 16.0).clamp(12.0, 64.0);
        let c = &mut self.captions;
        c.lines = c.lines.clamp(1, 4);
        c.font_size = finite_or(c.font_size, 24.0).clamp(12.0, 64.0);
        c.width_percent = c.width_percent.clamp(30, 100);
        let s = &mut self.services;
        s.idle_release_minutes = s.idle_release_minutes.clamp(1, 120);
        if s.speech.trim().is_empty() {
            s.speech = DOUBAO_ID.into();
        }
        s.custom_speech.retain(|p| !p.id.trim().is_empty() && p.id != DOUBAO_ID);
        if s.speech != DOUBAO_ID && !s.custom_speech.iter().any(|p| p.id == s.speech) {
            issues.push(Issue { field: "services.speech".into(), code: "unknown".into() });
            s.speech = DOUBAO_ID.into();
        }
        let a = &mut self.appearance;
        a.voice_scale = finite_or(a.voice_scale, 1.5).clamp(1.0, 3.0);
        a.ribbon_scale = finite_or(a.ribbon_scale, 1.5).clamp(1.0, 3.0);
        a.voice_text_gap = finite_or(a.voice_text_gap, 8.0).clamp(0.0, 64.0);
        if !matches!(a.style_surface.as_str(), "bar" | "captions" | "cards") {
            a.style_surface = "bar".into();
        }
        if !matches!(a.voice_style.as_str(), "orb" | "ribbon") {
            issues.push(Issue { field: "appearance.voiceStyle".into(), code: "invalid".into() });
            a.voice_style = "orb".into();
        }
        if !matches!(a.theme.as_str(), "system" | "light" | "dark") {
            a.theme = "system".into();
        }
        if a.language.trim().is_empty() {
            a.language = "system".into();
        }
        for (name, style) in [("bar", &mut a.bar), ("captions", &mut a.captions), ("cards", &mut a.cards)] {
            issues.extend(style.sanitize(&format!("appearance.{name}")));
        }
        issues
    }

    /// The schema version of a settings file, or `None` when it is missing or unreadable.
    pub fn stored_version(path: &Path) -> Option<u32> {
        let text = fs::read_to_string(path).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        Some(value.get("version").and_then(serde_json::Value::as_u64).unwrap_or(0) as u32)
    }

    /// Secrets of removed features that settings of `version` may have left in Windows
    /// Credential Manager: the translation and summary API keys (before version 2).
    pub fn removed_secrets(version: u32) -> &'static [&'static str] {
        if version < 2 { &["translation", "summary"] } else { &[] }
    }

    /// Loads settings; a damaged file is kept as `settings.json.bad` and defaults are used.
    pub fn load(path: &Path) -> Settings {
        let mut settings = match fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<Settings>(&text) {
                Ok(settings) => settings,
                Err(error) => {
                    log::warn!("settings file is invalid ({error}); using defaults");
                    let _ = fs::rename(path, path.with_extension("json.bad"));
                    Settings::default()
                }
            },
            Err(_) => Settings::default(),
        };
        settings.sanitize();
        settings
    }

    /// Writes to a temporary file first, then replaces the old file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let temp: PathBuf = path.with_extension("json.tmp");
        {
            let mut file = fs::File::create(&temp)?;
            file.write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
            file.sync_all()?;
        }
        fs::rename(&temp, path)
    }
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

/// Accepts `#AARRGGBB`.
pub fn is_argb(value: &str) -> bool {
    value.len() == 9 && value.starts_with('#') && value[1..].chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_and_are_valid() {
        let mut settings = Settings::default();
        assert!(settings.sanitize().is_empty());
        let text = serde_json::to_string(&settings).unwrap();
        assert_eq!(serde_json::from_str::<Settings>(&text).unwrap(), settings);
    }

    #[test]
    fn official_provider_fields_and_visual_center_round_trip() {
        let raw = r#"{"services":{"customSpeech":[{"id":"old","command":"legacy.exe"},{"id":"api","providerType":"qwen","model":"qwen3-asr-flash-realtime","endpoint":"wss://dashscope.aliyuncs.com/api-ws/v1/realtime"}]},"general":{"barPosition":{"x":10,"y":20}},"appearance":{"voiceScale":0.75}}"#;
        let mut settings: Settings = serde_json::from_str(raw).unwrap();
        assert!(settings.general.bar_visual_center.is_none());
        assert!(settings.services.custom_speech[0].provider_type.is_empty());
        assert_eq!(settings.services.custom_speech[0].command, "legacy.exe");
        settings.general.bar_visual_center = Some(Position { x: 222, y: 333 });
        settings.sanitize();
        assert_eq!(settings.appearance.voice_scale, 1.0);
        assert_eq!(settings.appearance.voice_text_gap, 8.0);
        assert_eq!(settings.general.bar_position, Some(Position { x: 10, y: 20 }));
        let encoded = serde_json::to_string(&settings).unwrap();
        assert!(encoded.contains("barVisualCenter"));
        assert!(encoded.contains("voiceTextGap"));
        assert!(!encoded.contains("apiKey"));
        assert_eq!(serde_json::from_str::<Settings>(&encoded).unwrap(), settings);
    }

    #[test]
    fn missing_fields_use_defaults() {
        let settings: Settings = serde_json::from_str(r#"{"dictation":{"writeMode":"live"}}"#).unwrap();
        assert_eq!(settings.dictation.write_mode, WriteMode::Live);
        assert_eq!(settings.hotkeys, HotkeySettings::default());
    }

    #[test]
    fn note_hotkey_migration_preserves_custom_disabled_and_conflicting_bindings() {
        assert_eq!(HotkeySettings::default().note, "Ctrl+Right");
        for (raw, expected) in [
            (r#"{"version":3,"hotkeys":{"note":"Ctrl+Alt+N"}}"#, "Ctrl+Right"),
            (r#"{"version":3,"hotkeys":{"note":"ctrl + alt + n"}}"#, "Ctrl+Right"),
            (r#"{"hotkeys":{"note":"Ctrl+Alt+N"}}"#, "Ctrl+Right"),
            (r#"{"version":3,"hotkeys":{"note":"Ctrl+Alt+M"}}"#, "Ctrl+Alt+M"),
            (r#"{"version":3,"hotkeys":{"note":""}}"#, ""),
            (r#"{"version":3,"hotkeys":{"note":"Ctrl+Alt+N","hold":"RAlt"}}"#, "Ctrl+Right"),
            (r#"{"version":4,"hotkeys":{"note":"rightAlt"}}"#, "Ctrl+Right"),
            (r#"{"version":4,"hotkeys":{"note":"RAlt","hold":"Ctrl+Right"}}"#, "RAlt"),
            (r#"{"version":5,"hotkeys":{"note":"RAlt"}}"#, "RAlt"),
            (r#"{"version":5,"hotkeys":{"note":"ctrl + left"}}"#, "Ctrl+Right"),
            (r#"{"version":5,"hotkeys":{"note":"Ctrl+Left","hold":"Ctrl+Right"}}"#, "Ctrl+Left"),
            (r#"{"version":4,"hotkeys":{"note":"Ctrl+Left"}}"#, "Ctrl+Left"),
            (r#"{"version":6,"hotkeys":{"note":"Ctrl+Left"}}"#, "Ctrl+Left"),
            (r#"{"version":5,"hotkeys":{"note":""}}"#, ""),
            (r#"{"version":4,"hotkeys":{"note":"Ctrl+Alt+N"}}"#, "Ctrl+Alt+N"),
        ] {
            let mut settings: Settings = serde_json::from_str(raw).unwrap();
            let issues = settings.sanitize();
            assert_eq!(settings.version, SETTINGS_VERSION);
            if expected == "Ctrl+Alt+N" {
                assert!(settings.hotkeys.note_toggle.is_empty());
                assert_eq!(issues, [Issue { field: "hotkeys.noteToggle".into(), code: "duplicate".into() }]);
            } else {
                assert_eq!(settings.hotkeys.note_toggle, "Ctrl+Alt+N");
                assert!(issues.is_empty(), "{raw}");
            }
            assert_eq!(settings.hotkeys.note_blank, "Ctrl+Alt+L");
            assert_eq!(settings.hotkeys.note, expected, "{raw}");
            let mut restored: Settings = serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
            assert!(restored.sanitize().is_empty());
            assert_eq!(restored, settings, "migration is idempotent");
        }
    }

    #[test]
    fn backspace_shortcut_survives_save_reload_and_sanitize() {
        let dir = std::env::temp_dir().join(format!("ll-config-backspace-{}", std::process::id()));
        let path = dir.join("settings.json");
        let mut settings = Settings::default();
        settings.hotkeys.cards = "Backspace".into();
        settings.hotkeys.captions.clear();
        assert!(settings.sanitize().is_empty());
        settings.save(&path).unwrap();
        let mut reloaded = Settings::load(&path);
        assert_eq!(reloaded.hotkeys.cards, "Backspace");
        assert_eq!(reloaded.hotkeys.note, "Ctrl+Right");
        assert!(reloaded.hotkeys.captions.is_empty());
        assert!(reloaded.sanitize().is_empty());
        reloaded.save(&path).unwrap();
        assert_eq!(Settings::load(&path), settings);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn card_header_defaults_and_all_modes_round_trip() {
        let legacy: SurfaceStyle = serde_json::from_str(r#"{"padding":8}"#).unwrap();
        assert_eq!(legacy.card_header, "title");
        assert!(legacy.card_header_date.is_empty());
        assert!(legacy.card_header_title.is_empty());
        for mode in [
            "none",
            "yearMonthDay",
            "monthDay",
            "monthDayTime",
            "monthDayWeekday",
            "monthDayTitle",
            "weekdayTitle",
            "title",
        ] {
            let mut settings = Settings::default();
            settings.appearance.cards.card_header = mode.into();
            settings.appearance.cards.card_header_date = "2028-02-29".into();
            settings.appearance.cards.card_header_title = "读书 📖".into();
            assert!(settings.sanitize().is_empty());
            let saved = serde_json::to_string(&settings).unwrap();
            let restored: Settings = serde_json::from_str(&saved).unwrap();
            assert_eq!(restored, settings);
            assert_eq!(restored.appearance.bar, SurfaceStyle::bar_default());
            assert_eq!(restored.appearance.captions, SurfaceStyle::captions_default());
        }
    }

    #[test]
    fn card_header_rejects_invalid_dates_and_bounds_unicode_titles() {
        for date in [
            "2026-02-29",
            "1900-02-29",
            "2026-04-31",
            "2026-00-01",
            "2026-11-00",
            "0000-01-01",
            "2026-1-01",
            "not-a-date",
        ] {
            let mut style = SurfaceStyle { card_header_date: date.into(), ..SurfaceStyle::default() };
            assert!(style.sanitize("cards").iter().any(|i| i.field == "cards.cardHeaderDate"), "{date}");
            assert!(style.card_header_date.is_empty());
        }
        for date in ["0001-01-01", "2000-02-29", "2028-02-29", "9999-12-31"] {
            assert!(valid_header_date(date), "{date}");
        }
        let mut style = SurfaceStyle {
            card_header: "invalid".into(),
            card_header_title: format!("A\nB\t{}\0", "🦀".repeat(90)),
            ..SurfaceStyle::default()
        };
        assert!(style.sanitize("cards").iter().any(|i| i.field == "cards.cardHeader"));
        assert_eq!(style.card_header, "title");
        assert_eq!(style.card_header_title.chars().count(), 80);
        assert!(style.card_header_title.starts_with("A B "));
        assert!(style.card_header_title.chars().all(|c| !c.is_control()));
    }

    #[test]
    fn card_header_override_survives_reload_without_changing_note_dates() {
        use crate::cards::{CardBook, CardKind};
        let mut book = CardBook::default();
        let id = book.add(CardKind::Todo, "买牛奶", "2026-11-08T12:30:00Z".into(), 1_783_600_000_000);
        let original = book.get(id).unwrap().clone();
        book.update(
            id,
            &serde_json::json!({"style": {
                "cardHeader": "weekdayTitle", "cardHeaderDate": "2028-02-29", "cardHeaderTitle": "采购"
            }}),
        )
        .unwrap();
        let dir = std::env::temp_dir().join(format!("ll-config-card-header-{}", std::process::id()));
        let path = dir.join("cards.json");
        book.save(&path).unwrap();
        let mut restored = CardBook::load(&path);
        let card = restored.get(id).unwrap();
        assert_eq!(card.text, original.text);
        assert_eq!((card.created.as_str(), card.created_ms), (original.created.as_str(), original.created_ms));
        assert_eq!(card.style.as_ref().unwrap().card_header_title, "采购");
        assert_eq!(card.style.as_ref().unwrap().card_header_date, "2028-02-29");
        restored.update(id, &serde_json::json!({"style": null})).unwrap();
        assert!(restored.get(id).unwrap().style.is_none(), "use shared removes all own header fields");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dictation_shadow_defaults_do_not_replace_explicit_surface_styles() {
        for raw in [
            r#"{}"#,
            r#"{"appearance":{}}"#,
            r#"{"appearance":{"bar":null}}"#,
            r#"{"appearance":{"bar":{}}}"#,
            r#"{"appearance":{"bar":{"padding":20}}}"#,
            r#"{"appearance":{"bar":{"shadow":null}}}"#,
            r#"{"appearance":{"bar":{"shadow":""}}}"#,
            r#"{"appearance":{"bar":{"shadow":"  "}}}"#,
        ] {
            let mut settings: Settings = serde_json::from_str(raw).unwrap();
            assert_eq!(settings.appearance.bar.shadow, Shadow::None, "{raw}");
            assert_eq!(settings.appearance.captions.shadow, Shadow::None);
            assert_eq!(settings.appearance.cards.shadow, Shadow::Soft);
            assert!(settings.sanitize().is_empty());
            let encoded = serde_json::to_string(&settings).unwrap();
            assert_eq!(serde_json::from_str::<Settings>(&encoded).unwrap(), settings);
        }
        for (shadow, expected) in [("none", Shadow::None), ("soft", Shadow::Soft), ("strong", Shadow::Strong)] {
            let raw = serde_json::json!({"appearance": {
                "bar": {"shadow": shadow}, "captions": {"shadow": "soft"}, "cards": {"shadow": "strong"}
            }});
            let settings: Settings = serde_json::from_value(raw).unwrap();
            assert_eq!(settings.appearance.bar.shadow, expected);
            assert_eq!(settings.appearance.captions.shadow, Shadow::Soft);
            assert_eq!(settings.appearance.cards.shadow, Shadow::Strong);
        }
        // Other surface defaults retain their previous behavior when only a partial style exists.
        let partial: Settings = serde_json::from_str(r#"{"appearance":{"captions":{},"cards":{}}}"#).unwrap();
        assert_eq!(partial.appearance.captions.shadow, Shadow::Soft);
        assert_eq!(partial.appearance.cards.shadow, Shadow::Soft);
    }

    #[test]
    fn appearance_editor_selection_round_trips_without_creating_card_styles() {
        let legacy: Settings = serde_json::from_str(r#"{"appearance":{}}"#).unwrap();
        assert_eq!(legacy.appearance.style_surface, "bar");
        assert_eq!(legacy.appearance.style_card, None);
        let mut settings: Settings =
            serde_json::from_str(r#"{"appearance":{"styleSurface":"cards","styleCard":41}}"#).unwrap();
        assert!(settings.sanitize().is_empty());
        assert_eq!(settings.appearance.style_surface, "cards");
        assert_eq!(settings.appearance.style_card, Some(41));
        assert_eq!(settings.appearance.cards, SurfaceStyle::default());
        let saved = serde_json::to_string(&settings).unwrap();
        assert_eq!(serde_json::from_str::<Settings>(&saved).unwrap(), settings);
        settings.appearance.style_surface = "unknown".into();
        settings.sanitize();
        assert_eq!(settings.appearance.style_surface, "bar");
    }

    #[test]
    fn visual_text_gap_has_independent_finite_bounds() {
        let mut settings = Settings::default();
        for (gap, expected) in [(f32::NAN, 8.0), (f32::INFINITY, 8.0), (-5.0, 0.0), (100.0, 64.0), (24.5, 24.5)] {
            settings.appearance.voice_text_gap = gap;
            settings.sanitize();
            assert_eq!(settings.appearance.voice_text_gap, expected);
            assert_eq!(settings.appearance.voice_scale, 1.5);
            assert_eq!(settings.dictation.font_size, 16.0);
        }
    }

    #[test]
    fn overlay_font_and_ribbon_scale_preserve_existing_preferences() {
        let old = r##"{"dictation":{"showText":false,"writeMode":"live"},
            "appearance":{"voiceScale":1.3,"bar":{"text":"#FF112233"}},
            "captions":{"fontSize":32},"general":{"barPosition":{"x":120,"y":240}}}"##;
        let mut settings: Settings = serde_json::from_str(old).unwrap();
        assert_eq!(settings.dictation.font_size, 16.0);
        assert_eq!(settings.appearance.ribbon_scale, 1.5);
        settings.dictation.font_size = 41.0;
        settings.appearance.ribbon_scale = 2.35;
        assert!(settings.sanitize().is_empty());
        let restored: Settings = serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored, settings);
        assert_eq!(restored.appearance.voice_scale, 1.3);
        assert_eq!(restored.appearance.bar.text, "#FF112233");
        assert_eq!(restored.captions.font_size, 32.0);
        assert_eq!(restored.dictation.write_mode, WriteMode::Live);
        assert!(!restored.dictation.show_text);
        assert_eq!(restored.general.bar_position, Some(Position { x: 120, y: 240 }));
    }

    #[test]
    fn overlay_font_and_ribbon_scale_have_independent_finite_bounds() {
        let mut settings = Settings::default();
        for (font, expected_font, ribbon, expected_ribbon) in [
            (f32::NAN, 16.0, f32::NAN, 1.5),
            (f32::INFINITY, 16.0, f32::INFINITY, 1.5),
            (0.0, 12.0, 0.0, 1.0),
            (100.0, 64.0, 10.0, 3.0),
        ] {
            settings.dictation.font_size = font;
            settings.appearance.ribbon_scale = ribbon;
            settings.sanitize();
            assert_eq!(settings.dictation.font_size, expected_font);
            assert_eq!(settings.appearance.ribbon_scale, expected_ribbon);
            assert_eq!(settings.captions.font_size, 24.0);
            assert_eq!(settings.appearance.voice_scale, 1.5);
        }
    }

    #[test]
    fn voice_style_migrates_without_changing_existing_preferences() {
        let old = r##"{"appearance":{"theme":"dark","bar":{"background":"#80333333","padding":20}},
            "dictation":{"showText":false,"writeMode":"live"},"general":{"barPosition":{"x":40,"y":60}}}"##;
        let mut settings: Settings = serde_json::from_str(old).unwrap();
        assert_eq!(settings.appearance.voice_style, "orb");
        assert_eq!(settings.appearance.voice_scale, 1.5);
        settings.appearance.voice_style = "ribbon".into();
        settings.appearance.voice_scale = 1.4;
        assert!(settings.sanitize().is_empty());
        let saved = serde_json::to_string(&settings).unwrap();
        let restored: Settings = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored, settings);
        assert_eq!(restored.appearance.voice_scale, 1.4);
        assert_eq!(restored.appearance.bar.background, "#80333333");
        assert_eq!(restored.appearance.bar.padding, 20.0);
        assert_eq!(restored.dictation.write_mode, WriteMode::Live);
        assert!(!restored.dictation.show_text);
        assert_eq!(restored.general.bar_position, Some(Position { x: 40, y: 60 }));
        settings.appearance.voice_style = "automatic".into();
        assert!(settings.sanitize().iter().any(|i| i.field == "appearance.voiceStyle"));
        assert_eq!(settings.appearance.voice_style, "orb");
        settings.appearance.voice_scale = f32::NAN;
        settings.sanitize();
        assert_eq!(settings.appearance.voice_scale, 1.5);
        settings.appearance.voice_scale = 10.0;
        settings.sanitize();
        assert_eq!(settings.appearance.voice_scale, 3.0);
        settings.appearance.voice_scale = 0.1;
        settings.sanitize();
        assert_eq!(settings.appearance.voice_scale, 1.0);
    }

    #[test]
    fn sanitize_fixes_bad_values() {
        let mut settings = Settings::default();
        settings.hotkeys.toggle = "Ctrl+Nope".into();
        settings.hotkeys.captions = "rctrl".into();
        settings.captions.lines = 9;
        settings.appearance.bar.text = "red".into();
        settings.services.speech = "missing".into();
        let issues = settings.sanitize();
        let fields: Vec<_> = issues.iter().map(|i| (i.field.as_str(), i.code.as_str())).collect();
        assert!(fields.contains(&("hotkeys.toggle", "invalid")));
        assert!(fields.contains(&("hotkeys.captions", "duplicate")));
        assert!(fields.contains(&("appearance.bar.text", "invalid")));
        assert!(fields.contains(&("services.speech", "unknown")));
        assert_eq!(settings.hotkeys.toggle, "Ctrl+Alt+Space");
        assert_eq!(settings.hotkeys.captions, "");
        assert_eq!(settings.captions.lines, 4);
        assert_eq!(settings.services.speech, DOUBAO_ID);
    }

    #[test]
    fn removed_features_are_dropped_from_old_settings() {
        let old = r#"{"version":1,"captions":{"translate":true,"targetLanguage":"ja","lines":3},
            "hotkeys":{"record":"Ctrl+Alt+R","note":"Ctrl+Alt+M"},
            "services":{"translation":{"baseUrl":"https://x","model":"m"},"summary":{"baseUrl":"https://y","model":"n"}},
            "general":{"recordRetentionDays":30,"speakerVoice":true,"startWithWindows":true}}"#;
        let mut settings: Settings = serde_json::from_str(old).unwrap();
        assert!(settings.sanitize().is_empty());
        assert_eq!((settings.captions.lines, settings.hotkeys.note.as_str()), (3, "Ctrl+Alt+M"));
        assert!(settings.general.start_with_windows);
        let saved = serde_json::to_string(&settings).unwrap();
        for gone in
            ["translate", "targetLanguage", "record", "translation", "summary", "recordRetentionDays", "speakerVoice"]
        {
            assert!(!saved.contains(&format!("\"{gone}\"")), "{gone} is still written");
        }
        assert_eq!(settings.version, SETTINGS_VERSION);
        assert_eq!(Settings::removed_secrets(1), ["translation", "summary"]);
        assert!(Settings::removed_secrets(SETTINGS_VERSION).is_empty());
    }

    #[test]
    fn preconnect_switch_of_version_2_becomes_a_level() {
        for (old, level) in [
            (r#"{"version":2,"dictation":{"preconnect":true}}"#, Preconnect::Aggressive),
            (r#"{"version":2,"dictation":{"preconnect":false}}"#, Preconnect::Off),
            (r#"{"version":2,"dictation":{}}"#, Preconnect::Off),
            (r#"{"version":3,"dictation":{"preconnectLevel":"balanced"}}"#, Preconnect::Balanced),
            // An older build that saved its switch over a level keeps a chosen level.
            (r#"{"version":3,"dictation":{"preconnect":true,"preconnectLevel":"balanced"}}"#, Preconnect::Balanced),
        ] {
            let mut settings: Settings = serde_json::from_str(old).unwrap();
            assert!(settings.sanitize().is_empty());
            assert_eq!(settings.dictation.preconnect_level, level, "{old}");
            let saved = serde_json::to_value(&settings).unwrap();
            assert!(saved["dictation"].get("preconnect").is_none(), "the old switch is never written");
            let name = serde_json::to_value(level).unwrap();
            assert_eq!(saved["dictation"]["preconnectLevel"], name);
        }
        assert!(serde_json::from_str::<Settings>(r#"{"dictation":{"preconnectLevel":"always"}}"#).is_err());
    }

    #[test]
    fn save_and_load_preserve_values_and_keep_bad_files() {
        let dir = std::env::temp_dir().join(format!("ll-config-{}", std::process::id()));
        let path = dir.join("settings.json");
        assert_eq!(Settings::stored_version(&path), None);
        let mut settings = Settings::default();
        settings.captions.lines = 3;
        settings.save(&path).unwrap();
        assert_eq!(Settings::stored_version(&path), Some(SETTINGS_VERSION));
        assert_eq!(Settings::load(&path), settings);
        fs::write(&path, "{not json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        assert!(path.with_extension("json.bad").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
