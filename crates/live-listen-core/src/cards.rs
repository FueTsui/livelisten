//! Spoken notes and their desktop cards, stored in one JSON file.
//!
//! Cards and whether they show on the desktop survive a restart. A card follows the shared card
//! style in the settings unless it has a style of its own.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::SurfaceStyle;
use crate::paths::write_atomic;

mod markdown;

const MAX_TEXT: usize = 4000;
const MAX_RTF_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CardKind {
    #[default]
    Note,
    Todo,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Card {
    pub id: u64,
    pub kind: CardKind,
    pub text: String,
    /// Native editor formatting, paired with `text` for previews and plain-text consumers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rtf: Option<String>,
    /// When it was said, UTC ISO 8601; the UI shows it as the card's date.
    pub created: String,
    pub created_ms: u64,
    pub done: bool,
    pub archived: bool,
    /// Shown on the desktop (when cards are shown at all).
    pub visible: bool,
    /// Screen position in physical pixels; `None` lets the app place it.
    pub x: Option<i32>,
    pub y: Option<i32>,
    /// This card's own look; `None` uses `appearance.cards` from the settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<SurfaceStyle>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CardBook {
    pub version: u32,
    pub next_id: u64,
    /// Master switch for showing cards on the desktop.
    pub show_on_desktop: bool,
    pub cards: Vec<Card>,
}

impl Default for CardBook {
    fn default() -> Self {
        CardBook { version: 1, next_id: 0, show_on_desktop: true, cards: Vec::new() }
    }
}

/// Words that turn a spoken note into a to-do when it starts with them.
const TODO_PREFIXES: [&str; 12] =
    ["待办", "待辦", "提醒我", "提醒", "记得", "記得", "别忘了", "不要忘了", "todo", "to-do", "to do", "remind me to"];

fn trim_punct(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || "，,：:。.、;；-—".contains(c))
}

/// Decides between a note and a to-do; a leading "待办" or "记得" makes a to-do and is removed.
pub fn classify(text: &str) -> (CardKind, String) {
    let text = trim_punct(text);
    for prefix in TODO_PREFIXES {
        let head: String = text.chars().take(prefix.chars().count()).collect();
        if head.to_lowercase() == prefix {
            let rest = trim_punct(&text[head.len()..]);
            if !rest.is_empty() {
                return (CardKind::Todo, rest.to_string());
            }
        }
    }
    (CardKind::Note, text.to_string())
}

fn clip(text: &str) -> String {
    text.trim().chars().take(MAX_TEXT).collect()
}

/// Checks the bounded, text-only RTF envelope before passing it to the native editor.
/// This is not an RTF renderer: the UI must export `text` and `rtf` from one document.
fn valid_rtf(rtf: &str, text: &str) -> bool {
    if rtf.len() > MAX_RTF_BYTES || text.trim().chars().count() > MAX_TEXT || rtf.contains('\0') {
        return false;
    }
    let bytes = rtf.as_bytes();
    if !bytes.starts_with(b"{\\rtf1") || bytes.get(6).is_none_or(u8::is_ascii_digit) {
        return false;
    }
    let mut depth = 0_u32;
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'{' => {
                depth += 1;
                if depth > 128 {
                    return false;
                }
            }
            b'}' => {
                let Some(next) = depth.checked_sub(1) else { return false };
                depth = next;
                if depth == 0 && bytes[at + 1..].iter().any(|b| !b.is_ascii_whitespace()) {
                    return false;
                }
            }
            b'\\' => {
                at += 1;
                if at == bytes.len() {
                    return false;
                }
                let start = at;
                while at < bytes.len() && bytes[at].is_ascii_alphabetic() {
                    at += 1;
                }
                if at > start {
                    let word = &rtf[start..at];
                    // The card editor needs text, formatting and hyperlinks, never embedded
                    // files, OLE objects, pictures, binary data or external file tables.
                    if [
                        "object",
                        "objdata",
                        "objclass",
                        "objname",
                        "objalias",
                        "oleclsid",
                        "pict",
                        "shppict",
                        "nonshppict",
                        "listpicture",
                        "bin",
                        "filetbl",
                        "file",
                        "datastore",
                    ]
                    .iter()
                    .any(|blocked| word.eq_ignore_ascii_case(blocked))
                    {
                        return false;
                    }
                    continue;
                }
                // Escaped braces/backslashes and control symbols do not change group depth.
            }
            _ => {}
        }
        at += 1;
    }
    depth == 0
}

impl CardBook {
    /// Rebuilds the daily Markdown view without modifying unrelated or hand-edited files.
    pub fn sync_markdown(&self, config_dir: &Path) -> std::io::Result<()> {
        markdown::sync(self, config_dir)
    }

    pub(crate) fn protect_markdown_orphans(config_dir: &Path) -> std::io::Result<()> {
        markdown::protect_orphans(config_dir)
    }

    /// Loads the book; a damaged file is kept as `.bad` and an empty book is used.
    pub fn load(path: &Path) -> CardBook {
        Self::load_with_status(path).0
    }

    /// The status distinguishes a valid empty source from missing or corrupt data, so a
    /// startup projection cannot erase readable Markdown recovery copies in the latter case.
    pub(crate) fn load_with_status(path: &Path) -> (CardBook, bool) {
        match fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<CardBook>(&text) {
                Ok(mut book) => {
                    for card in &mut book.cards {
                        // Keep the plain text usable if a saved rich document is damaged.
                        if card.rtf.as_ref().is_some_and(|rtf| !valid_rtf(rtf, &card.text)) {
                            card.rtf = None;
                        }
                    }
                    (book, true)
                }
                Err(e) => {
                    log::warn!("cards file is invalid ({e}); starting empty");
                    let _ = fs::rename(path, path.with_extension("json.bad"));
                    (CardBook::default(), false)
                }
            },
            Err(_) => (CardBook::default(), false),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        write_atomic(path, serde_json::to_string_pretty(self)?.as_bytes())
    }

    pub fn add(&mut self, kind: CardKind, text: &str, created: String, created_ms: u64) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.cards.push(Card { id, kind, text: clip(text), created, created_ms, visible: true, ..Card::default() });
        id
    }

    /// Turns spoken text into a note or to-do card. Returns `None` for empty text.
    pub fn add_spoken(&mut self, text: &str, created: String, created_ms: u64) -> Option<u64> {
        let (kind, text) = classify(text);
        if text.is_empty() {
            return None;
        }
        Some(self.add(kind, &text, created, created_ms))
    }

    pub fn get(&self, id: u64) -> Option<&Card> {
        self.cards.iter().find(|c| c.id == id)
    }

    /// Applies `text`, `kind`, `done`, `archived`, `visible`, `x`, `y`, `createdMs` (the
    /// card's date, Unix milliseconds between 2000 and 2100) and `style` (an object, or `null`
    /// to follow the shared card style again) from `patch`. `rtf` requires its matching
    /// `text`; a plain-text edit clears old formatting. Nothing changes on an error.
    pub fn update(&mut self, id: u64, patch: &Value) -> Result<(), &'static str> {
        const YEAR_2000: u64 = 946_684_800_000;
        const YEAR_2100: u64 = 4_102_444_800_000;
        let rtf = match patch.get("rtf") {
            None => None,
            Some(Value::Null) => Some(None),
            Some(Value::String(rtf)) => {
                let text = patch["text"].as_str().ok_or("bad_rich_text")?;
                if !valid_rtf(rtf, text) {
                    return Err("bad_rich_text");
                }
                Some(Some(rtf.clone()))
            }
            Some(_) => return Err("bad_rich_text"),
        };
        let style = match patch.get("style") {
            None => None,
            Some(Value::Null) => Some(None),
            Some(value) => {
                let mut style: SurfaceStyle = serde_json::from_value(value.clone()).map_err(|_| "bad_style")?;
                // Invalid colors fall back to the theme, like the shared style.
                style.sanitize("style");
                Some(Some(style))
            }
        };
        let card = self.cards.iter_mut().find(|c| c.id == id).ok_or("unknown_card")?;
        // Validate every fallible field before applying any part of the patch.
        // A rejected edit must not silently change the in-memory book.
        let created_ms = patch
            .get("createdMs")
            .filter(|v| !v.is_null())
            .map(|v| v.as_u64().filter(|ms| (YEAR_2000..YEAR_2100).contains(ms)).ok_or("bad_date"))
            .transpose()?;
        let kind = patch["kind"]
            .as_str()
            .map(|kind| match kind {
                "note" => Ok(CardKind::Note),
                "todo" => Ok(CardKind::Todo),
                _ => Err("bad_kind"),
            })
            .transpose()?;
        if let Some(style) = style {
            card.style = style;
        }
        if let Some(ms) = created_ms {
            card.created_ms = ms;
            card.created = crate::paths::iso_from_ms(ms);
        }
        if let Some(text) = patch["text"].as_str() {
            card.text = clip(text);
            card.rtf = None;
        }
        if let Some(rtf) = rtf {
            card.rtf = rtf;
        }
        if let Some(kind) = kind {
            card.kind = kind;
        }
        if let Some(done) = patch["done"].as_bool() {
            card.done = done;
        }
        if let Some(archived) = patch["archived"].as_bool() {
            card.archived = archived;
            // Restoring a card brings it back to the desktop.
            card.visible = !archived;
        }
        if let Some(visible) = patch["visible"].as_bool() {
            card.visible = visible;
        }
        if let (Some(x), Some(y)) = (patch["x"].as_i64(), patch["y"].as_i64()) {
            card.x = i32::try_from(x).ok();
            card.y = i32::try_from(y).ok();
        }
        Ok(())
    }

    /// Makes every card follow the shared card style; returns how many had their own.
    pub fn clear_styles(&mut self) -> usize {
        self.cards.iter_mut().filter_map(|c| c.style.take()).count()
    }

    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.cards.len();
        self.cards.retain(|c| c.id != id);
        self.cards.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn invalid_composite_patch_does_not_change_any_card_field() {
        let mut book = CardBook::default();
        let id = book.add(CardKind::Note, "original", "t".into(), 0);
        let original = book.clone();
        assert_eq!(book.update(id, &json!({"style": {}, "createdMs": 1})), Err("bad_date"));
        assert_eq!(book, original);
        assert_eq!(
            book.update(id, &json!({"style": {}, "text": "new", "createdMs": 946_684_800_000_u64, "kind": "invalid"})),
            Err("bad_kind")
        );
        assert_eq!(book, original);
    }

    #[test]
    fn rich_text_round_trips_and_plain_text_edits_clear_formatting() {
        let mut book = CardBook::default();
        let id = book.add(CardKind::Note, "old", "t".into(), 0);
        let rtf = r"{\rtf1\ansi {\b hello} {\i world}}";
        book.update(id, &json!({"text": "hello world", "rtf": rtf})).unwrap();
        book.update(id, &json!({"x": 10, "y": 20, "done": true, "style": {"padding": 8}})).unwrap();
        assert_eq!(book.get(id).unwrap().rtf.as_deref(), Some(rtf));
        let dir = std::env::temp_dir().join(format!("ll-cards-rtf-{}", std::process::id()));
        let path = dir.join("cards.json");
        book.save(&path).unwrap();
        assert_eq!(CardBook::load(&path), book);

        // Formatting-only changes keep the plain-text mirror, and metadata keeps formatting.
        let changed = r"{\rtf1\ansi {\ul hello world}}";
        book.update(id, &json!({"text": "hello world", "rtf": changed})).unwrap();
        assert_eq!(book.get(id).unwrap().rtf.as_deref(), Some(changed));
        book.update(id, &json!({"text": "edited in a plain-text client"})).unwrap();
        assert_eq!(book.get(id).unwrap().rtf, None);
        book.update(id, &json!({"text": "hello world", "rtf": changed})).unwrap();
        book.update(id, &json!({"rtf": null})).unwrap();
        assert_eq!(book.get(id).unwrap().rtf, None);
        assert_eq!(book.get(id).unwrap().text, "hello world");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn old_cards_load_without_rich_text_and_damaged_formatting_keeps_plain_text() {
        let old = r#"{"version":1,"nextId":1,"cards":[{"id":1,"text":"old note","visible":true}]}"#;
        let book: CardBook = serde_json::from_str(old).unwrap();
        assert_eq!(book.cards[0].rtf, None);
        assert_eq!(book.cards[0].text, "old note");
        assert!(serde_json::to_value(&book).unwrap()["cards"][0].get("rtf").is_none());

        let dir = std::env::temp_dir().join(format!("ll-cards-rtf-recovery-{}", std::process::id()));
        let path = dir.join("cards.json");
        let mut damaged = book;
        damaged.cards[0].rtf = Some(r"{\rtf1\object bad}".into());
        damaged.save(&path).unwrap();
        let recovered = CardBook::load(&path);
        assert_eq!(recovered.cards[0].text, "old note");
        assert_eq!(recovered.cards[0].rtf, None);
        assert!(!path.with_extension("json.bad").exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn invalid_or_oversized_rich_text_rejects_the_entire_patch() {
        let mut book = CardBook::default();
        let id = book.add(CardKind::Note, "original", "t".into(), 0);
        let original = book.clone();
        for patch in [
            json!({"rtf": r"{\rtf1 missing plain text}", "done": true}),
            json!({"rtf": 42, "text": "new", "style": {"padding": 12}}),
            json!({"rtf": "not RTF", "text": "new", "done": true}),
            json!({"rtf": r"{\rtf1 incomplete", "text": "new", "done": true}),
            json!({"rtf": r"{\rtf1 valid} trailing content", "text": "new", "done": true}),
            json!({"rtf": format!("{{\\rtf1 {}}}", "x".repeat(MAX_RTF_BYTES)), "text": "new", "done": true}),
            json!({"rtf": r"{\rtf1 long text}", "text": "字".repeat(MAX_TEXT + 1), "done": true}),
        ] {
            assert_eq!(book.update(id, &patch), Err("bad_rich_text"));
            assert_eq!(book, original);
        }
        // Count Unicode characters, not UTF-8 bytes, at the plain-text limit.
        let text = "字".repeat(MAX_TEXT);
        book.update(id, &json!({"text": text, "rtf": r"{\rtf1\ansi text}"})).unwrap();
        assert_eq!(book.get(id).unwrap().text.chars().count(), MAX_TEXT);
    }

    #[test]
    fn rich_text_rejects_embedded_content_but_allows_literal_control_names_and_links() {
        for rtf in [
            r"{\rtf1 {\object\objdata 0102}}",
            r"{\rtf1 {\PICT 0102}}",
            r"{\rtf1\bin2 xx}",
            r"{\rtf1 {\*\filetbl {\file external}}}",
            "{\\rtf1 nul\0}",
        ] {
            assert!(!valid_rtf(rtf, "text"), "rejected embedded content");
        }
        assert!(valid_rtf(r"{\rtf1 escaped \\object \{braces\}}", "escaped \\object {braces}"));
        assert!(valid_rtf(
            r#"{\rtf1 {\field{\*\fldinst{HYPERLINK "https://example.com"}}{\fldrslt{example}}}}"#,
            "example"
        ));
    }

    #[test]
    fn spoken_text_becomes_a_note_or_todo() {
        assert_eq!(classify("记得，明天给妈妈打电话。"), (CardKind::Todo, "明天给妈妈打电话".into()));
        assert_eq!(classify("Remind me to call Sam"), (CardKind::Todo, "call Sam".into()));
        assert_eq!(classify("今天的灵感：做一个番茄钟。"), (CardKind::Note, "今天的灵感：做一个番茄钟".into()));
        assert_eq!(classify("待办"), (CardKind::Note, "待办".into()));
        let mut book = CardBook::default();
        assert_eq!(book.add_spoken("  。 ", "t".into(), 0), None);
        let id = book.add_spoken("待办：交报告", "t".into(), 5).unwrap();
        let card = book.get(id).unwrap();
        assert!(card.visible && card.kind == CardKind::Todo && card.text == "交报告");
    }

    #[test]
    fn updates_archive_and_persist() {
        let mut book = CardBook::default();
        let id = book.add(CardKind::Note, "a", "t".into(), 0);
        book.update(id, &json!({"done": true, "x": 10, "y": -20, "kind": "todo"})).unwrap();
        book.update(id, &json!({"archived": true})).unwrap();
        let card = book.get(id).unwrap();
        assert!(card.done && card.archived && !card.visible);
        assert_eq!((card.x, card.y, card.kind), (Some(10), Some(-20), CardKind::Todo));
        assert_eq!(book.update(id, &json!({"kind": "x"})), Err("bad_kind"));
        book.update(id, &json!({"createdMs": 1_762_423_200_000u64})).unwrap();
        assert_eq!(book.get(id).unwrap().created, "2025-11-06T10:00:00.000Z");
        assert_eq!(book.update(id, &json!({"createdMs": 5})), Err("bad_date"));
        assert_eq!(book.update(99, &json!({})), Err("unknown_card"));
        let dir = std::env::temp_dir().join(format!("ll-cards-{}", std::process::id()));
        let path = dir.join("cards.json");
        book.save(&path).unwrap();
        assert_eq!(CardBook::load(&path), book);
        fs::write(&path, "{bad").unwrap();
        assert_eq!(CardBook::load(&path), CardBook::default());
        assert!(book.remove(id) && !book.remove(id));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cards_can_have_their_own_style() {
        let mut book = CardBook::default();
        let a = book.add(CardKind::Note, "a", "t".into(), 0);
        let b = book.add(CardKind::Todo, "b", "t".into(), 0);
        let style = json!({"background": "#FF102030", "text": "nope", "cornerRadius": 99, "shadow": "strong"});
        book.update(a, &json!({"style": style})).unwrap();
        let own = book.get(a).unwrap().style.clone().unwrap();
        assert_eq!(own.background, "#FF102030");
        assert_eq!(own.text, "", "an invalid color follows the theme");
        assert_eq!((own.corner_radius, own.shadow), (32.0, crate::config::Shadow::Strong));
        assert_eq!(book.get(b).unwrap().style, None);
        assert_eq!(book.update(b, &json!({"style": 5, "done": true})), Err("bad_style"));
        assert!(!book.get(b).unwrap().done, "a rejected patch changes nothing");
        // Cards without a style of their own are written without the field.
        let text = serde_json::to_value(&book).unwrap();
        assert!(text["cards"][1].get("style").is_none());
        book.update(b, &json!({"style": {"padding": 4}})).unwrap();
        book.update(a, &json!({"style": null})).unwrap();
        assert_eq!(book.get(a).unwrap().style, None);
        assert_eq!(book.clear_styles(), 1);
        assert!(book.cards.iter().all(|c| c.style.is_none()));
    }
}
