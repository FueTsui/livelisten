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

const MAX_TEXT: usize = 4000;

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

impl CardBook {
    /// Loads the book; a damaged file is kept as `.bad` and an empty book is used.
    pub fn load(path: &Path) -> CardBook {
        match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                log::warn!("cards file is invalid ({e}); starting empty");
                let _ = fs::rename(path, path.with_extension("json.bad"));
                CardBook::default()
            }),
            Err(_) => CardBook::default(),
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
    /// to follow the shared card style again) from `patch`. Nothing changes on an error.
    pub fn update(&mut self, id: u64, patch: &Value) -> Result<(), &'static str> {
        const YEAR_2000: u64 = 946_684_800_000;
        const YEAR_2100: u64 = 4_102_444_800_000;
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
