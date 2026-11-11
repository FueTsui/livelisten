//! Spoken-note cards on the engine thread.
//!
//! A card that cannot be written stays in memory, is shown and edited from there, and is
//! written again later; nothing is dropped on a failed save.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::Core;
use crate::cards::{CardBook, CardKind};
use crate::paths;

/// How often a failed card save is retried.
const CARD_RETRY: Duration = Duration::from_secs(5);

/// Error code for note cards that could not be written; the cards stay in memory.
const CARD_SAVE_FAILED: &str = "card_save_failed";
const CARD_MARKDOWN_FAILED: &str = "card_markdown_failed";

pub(super) struct Notes {
    cards: CardBook,
    cards_path: PathBuf,
    cards_dirty: bool,
    markdown_dir: PathBuf,
    markdown_dirty: bool,
    markdown_reported: bool,
    orphan_protection_pending: bool,
    retry_at: Instant,
    recovery_dir: PathBuf,
}

impl Notes {
    pub(super) fn open(config: &Path) -> Notes {
        let cards_path = config.join("cards.json");
        let (cards, source_loaded) = CardBook::load_with_status(&cards_path);
        Notes {
            cards,
            cards_path,
            cards_dirty: false,
            markdown_dir: config.to_path_buf(),
            markdown_dirty: source_loaded,
            markdown_reported: false,
            orphan_protection_pending: !source_loaded,
            retry_at: Instant::now(),
            recovery_dir: std::env::temp_dir().join("LiveListen-recovery"),
        }
    }

    /// An orderly exit writes the cards once more. If that fails, a uniquely named recovery
    /// copy is written; an error keeps the core callable so the user can free space and retry.
    pub(super) fn flush(&mut self) -> Result<(), &'static str> {
        if !self.cards_dirty {
            // The JSON is durable. A Markdown conflict must not trap the user in the app;
            // a new session rebuilds the projection and reports any remaining conflict.
            if let Err(e) = self.sync_markdown() {
                log::warn!("daily notes not synchronized before exit: {}", e.kind());
            }
            return Ok(());
        }
        self.cards_dirty = self.save_primary().is_err();
        if !self.cards_dirty {
            if let Err(e) = self.sync_markdown() {
                log::warn!("daily notes not synchronized before exit: {}", e.kind());
            }
            return Ok(());
        }
        let stamp = format!("{}-{}", paths::unix_ms(), std::process::id());
        let path = self.recovery_dir.join(format!("cards-{stamp}.json"));
        match self.cards.save(&path) {
            Ok(()) => {
                log::warn!("unsaved cards recovery copy in {}", self.recovery_dir.display());
                Ok(())
            }
            Err(e) => {
                log::error!("cards recovery failed: {}", e.kind());
                Err(CARD_SAVE_FAILED)
            }
        }
    }

    fn sync_markdown(&mut self) -> std::io::Result<()> {
        if !self.markdown_dirty {
            return Ok(());
        }
        let result = self.cards.sync_markdown(&self.markdown_dir);
        self.markdown_dirty = result.is_err();
        if result.is_ok() {
            self.markdown_reported = false;
        }
        result
    }

    fn save_primary(&mut self) -> std::io::Result<()> {
        if self.orphan_protection_pending {
            CardBook::protect_markdown_orphans(&self.markdown_dir)?;
            self.orphan_protection_pending = false;
        }
        self.cards.save(&self.cards_path)
    }
}

fn err(code: &str) -> String {
    code.to_string()
}

fn text_of<'a>(request: &'a Value, key: &str) -> &'a str {
    request[key].as_str().unwrap_or("")
}

fn id_of(request: &Value, key: &str) -> Result<u64, String> {
    request[key].as_u64().ok_or_else(|| err("bad_request"))
}

impl Core {
    pub(super) fn clock_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    /// Writes the cards; on failure they stay in memory and the user is told.
    fn save_cards(&mut self) -> Result<(), String> {
        let failed_before = self.notes.cards_dirty;
        let result = self.notes.save_primary();
        self.notes.cards_dirty = result.is_err();
        if let Err(e) = result {
            self.notes.retry_at = Instant::now() + CARD_RETRY;
            log::warn!("cards not saved: {}", e.kind());
            if !failed_before {
                self.emit_error("cards", CARD_SAVE_FAILED);
            }
            Err(err(CARD_SAVE_FAILED))
        } else {
            match self.notes.sync_markdown() {
                Ok(()) => Ok(()),
                Err(e) => {
                    self.notes.retry_at = Instant::now() + CARD_RETRY;
                    log::warn!("daily notes not synchronized: {}", e.kind());
                    if !self.notes.markdown_reported {
                        self.notes.markdown_reported = true;
                        self.emit_error("cards", CARD_MARKDOWN_FAILED);
                    }
                    Err(err(CARD_MARKDOWN_FAILED))
                }
            }
        }
    }

    fn emit_cards(&self) {
        self.emit_cards_with_editor(None);
    }

    fn emit_cards_with_editor(&self, edit_card: Option<u64>) {
        let mut event = json!({
            "event": "cards.changed",
            "cards": self.notes.cards.cards,
            "showOnDesktop": self.notes.cards.show_on_desktop,
            "markdownPending": self.notes.markdown_dirty,
            "markdownError": if self.notes.markdown_reported { CARD_MARKDOWN_FAILED } else { "" },
        });
        if let Some(id) = edit_card {
            event["editCard"] = json!(id);
        }
        self.emit(event);
    }

    pub(super) fn add_card(&mut self, kind: CardKind, text: &str, edit_on_desktop: bool) -> Result<u64, String> {
        let id = self.notes.cards.add(kind, text, paths::timestamp(), paths::unix_ms());
        self.notes.markdown_dirty = true;
        if edit_on_desktop {
            self.notes.cards.show_on_desktop = true;
        }
        let saved = self.save_cards();
        self.emit_cards_with_editor(edit_on_desktop.then_some(id));
        saved.map(|()| id)
    }

    /// A spoken note finished: keep it as a note or to-do card.
    pub(super) fn save_note(&mut self, text: &str) {
        match self.notes.cards.add_spoken(text, paths::timestamp(), paths::unix_ms()) {
            Some(id) => {
                self.notes.markdown_dirty = true;
                let kind = self.notes.cards.get(id).map(|c| c.kind).unwrap_or_default();
                // Cards the user hid stay hidden; the bar confirms that the note was saved.
                // "Saved" only when it is on disk; otherwise the error was shown.
                if self.save_cards().is_ok() {
                    log::info!("note saved as card {id} ({kind:?})");
                    self.emit(json!({"event": "note.saved", "id": id, "kind": kind}));
                }
                self.emit_cards();
            }
            None => self.notice("note_empty"),
        }
    }

    pub(super) fn set_cards_shown(&mut self, on: bool) {
        self.notes.cards.show_on_desktop = on;
        // Every card was hidden one by one: showing the desktop cards brings them back,
        // instead of a switch that appears to do nothing.
        if on && !self.notes.cards.cards.iter().any(|c| c.visible && !c.archived) {
            for card in self.notes.cards.cards.iter_mut().filter(|c| !c.archived) {
                card.visible = true;
            }
        }
        let _ = self.save_cards();
        self.emit_cards();
    }

    pub(super) fn cards_shown(&self) -> bool {
        self.notes.cards.show_on_desktop
    }

    /// Retries a failed card save.
    pub(super) fn notes_tick(&mut self) {
        if (self.notes.cards_dirty || self.notes.markdown_dirty) && Instant::now() >= self.notes.retry_at {
            let before = (self.notes.cards_dirty, self.notes.markdown_dirty, self.notes.markdown_reported);
            let _ = self.save_cards();
            if before != (self.notes.cards_dirty, self.notes.markdown_dirty, self.notes.markdown_reported) {
                self.emit_cards();
            }
        }
    }

    /// When the engine should try a failed card save again.
    pub(super) fn notes_deadline(&self) -> Option<Instant> {
        (self.notes.cards_dirty || self.notes.markdown_dirty).then_some(self.notes.retry_at)
    }

    pub(super) fn notes_call(&mut self, cmd: &str, request: &Value) -> Option<Result<Value, String>> {
        let result = match cmd {
            "cards.list" => Ok(json!({
                "cards": self.notes.cards.cards,
                "showOnDesktop": self.notes.cards.show_on_desktop,
                "unsaved": self.notes.cards_dirty,
                "markdownPending": self.notes.markdown_dirty,
                "markdownError": if self.notes.markdown_reported { CARD_MARKDOWN_FAILED } else { "" },
            })),
            "cards.add" => {
                let kind = if text_of(request, "kind") == "todo" { CardKind::Todo } else { CardKind::Note };
                let edit_on_desktop = request["editOnDesktop"].as_bool().unwrap_or(false);
                self.add_card(kind, text_of(request, "text"), edit_on_desktop).map(|id| json!({"id": id}))
            }
            "cards.update" => match id_of(request, "id") {
                Ok(card) => {
                    let patch = &request["patch"];
                    match self.notes.cards.update(card, patch) {
                        Ok(()) => {
                            if patch.as_object().is_some_and(|fields| {
                                fields.keys().any(|key| {
                                    matches!(key.as_str(), "text" | "rtf" | "kind" | "done" | "archived" | "createdMs")
                                })
                            }) {
                                self.notes.markdown_dirty = true;
                            }
                            let saved = self.save_cards();
                            // Moving a card is not news for the other windows.
                            let moved_only = patch.as_object().is_some_and(|m| m.keys().all(|k| k == "x" || k == "y"));
                            if !moved_only {
                                self.emit_cards();
                            }
                            saved.map(|()| json!({}))
                        }
                        Err(code) => Err(err(code)),
                    }
                }
                Err(e) => Err(e),
            },
            "cards.styles.clear" => {
                // Every card follows the shared card style from the settings again.
                let cleared = self.notes.cards.clear_styles();
                let saved = if cleared > 0 { self.save_cards() } else { Ok(()) };
                if cleared > 0 {
                    log::info!("{cleared} card styles cleared");
                    self.emit_cards();
                }
                saved.map(|()| json!({"cleared": cleared}))
            }
            "cards.delete" => match id_of(request, "id") {
                Ok(id)
                    if request.get("expected").is_some_and(|expected| {
                        self.notes.cards.get(id).is_some_and(|card| {
                            expected["text"].as_str() != Some(card.text.as_str())
                                || expected["rtf"].as_str() != card.rtf.as_deref()
                        })
                    }) =>
                {
                    Err(err("card_changed"))
                }
                Ok(card) if self.notes.cards.remove(card) => {
                    self.notes.markdown_dirty = true;
                    let saved = self.save_cards();
                    self.emit_cards();
                    saved.map(|()| json!({}))
                }
                Ok(_) => Err(err("unknown_card")),
                Err(e) => Err(e),
            },
            "cards.show" => {
                self.set_cards_shown(request["on"].as_bool().unwrap_or(true));
                Ok(json!({"on": self.notes.cards.show_on_desktop}))
            }
            _ => return None,
        };
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, mpsc};

    use super::super::{Msg, Options};
    use super::*;
    use crate::engine::EventSink;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("live-listen-notes-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// An engine core without hooks, audio or the engine thread.
    fn core(dir: &Path) -> (Core, Arc<Mutex<Vec<Value>>>, mpsc::Receiver<Msg>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink_events = Arc::clone(&events);
        let sink: EventSink =
            Arc::new(move |e: &str| sink_events.lock().unwrap().push(serde_json::from_str(e).unwrap()));
        let (tx, rx) = mpsc::channel();
        let options = Options { config_dir: Some(dir.to_path_buf()), hooks: false, ..Options::default() };
        let mut core = Core::new(options, sink, tx);
        core.notes.recovery_dir = dir.join("recovery");
        (core, events, rx)
    }

    fn errors(events: &Mutex<Vec<Value>>, code: &str) -> usize {
        events.lock().unwrap().iter().filter(|e| e["event"] == "error" && e["code"] == code).count()
    }

    #[test]
    fn blank_note_shortcut_shows_and_edits_a_persisted_empty_note() {
        let dir = temp_dir("blank-shortcut");
        let (mut core, events, _rx) = core(&dir);
        core.set_cards_shown(false);
        core.on_signal(crate::hotkey::Signal::NoteBlank);

        assert!(core.dictation.is_none(), "blank cards do not start recognition");
        let book = CardBook::load(&dir.join("cards.json"));
        assert!(book.show_on_desktop);
        assert_eq!(book.cards.len(), 1);
        let card = &book.cards[0];
        assert!(card.visible && card.text.is_empty());
        assert_eq!(card.kind, CardKind::Note);
        let events = events.lock().unwrap();
        let change = events.iter().rfind(|e| e["event"] == "cards.changed").unwrap();
        assert_eq!(change["editCard"], card.id);
        assert_eq!(change["showOnDesktop"], true);
        drop(core);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn delete_can_require_the_same_text_and_formatting() {
        let dir = temp_dir("conditional-delete");
        let (mut core, _, _rx) = core(&dir);
        let added = core.notes_call("cards.add", &json!({"text": "原文"})).unwrap().unwrap();
        let id = added["id"].as_u64().unwrap();
        core.notes_call("cards.update", &json!({"id": id, "patch": {"text": "另一处的修改"}})).unwrap().unwrap();
        let stale = json!({"id": id, "expected": {"text": "原文", "rtf": null}});
        assert_eq!(core.notes_call("cards.delete", &stale).unwrap(), Err("card_changed".to_string()));
        assert_eq!(CardBook::load(&dir.join("cards.json")).cards[0].text, "另一处的修改");
        let current = json!({"id": id, "expected": {"text": "另一处的修改", "rtf": null}});
        core.notes_call("cards.delete", &current).unwrap().unwrap();
        assert!(CardBook::load(&dir.join("cards.json")).cards.is_empty());
        drop(core);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn markdown_failures_preserve_json_report_and_retry_without_another_edit() {
        let dir = temp_dir("markdown-retry");
        std::fs::write(dir.join("随手记"), "blocked").unwrap();
        let (mut core, events, _rx) = core(&dir);
        let result = core.notes_call("cards.add", &json!({"text": "持久化原文"})).unwrap();
        assert_eq!(result, Err(CARD_MARKDOWN_FAILED.to_string()));
        assert_eq!(CardBook::load(&dir.join("cards.json")).cards[0].text, "持久化原文");
        assert!(!core.notes.cards_dirty && core.notes.markdown_dirty);
        assert_eq!(errors(&events, CARD_MARKDOWN_FAILED), 1);
        assert_eq!(core.notes_call("cards.list", &json!({})).unwrap().unwrap()["markdownPending"], true);
        core.notes.flush().unwrap(); // A durable JSON card permits shutdown despite an export conflict.
        assert!(core.notes.markdown_dirty);
        std::fs::remove_file(dir.join("随手记")).unwrap();
        core.notes.retry_at = Instant::now();
        core.notes_tick();
        assert!(!core.notes.markdown_dirty && core.notes_deadline().is_none());
        assert_eq!(std::fs::read_dir(dir.join("随手记").join("进行中")).unwrap().count(), 1);
        drop(core);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn startup_rebuilds_markdown_and_metadata_updates_do_not_touch_it() {
        let dir = temp_dir("markdown-startup");
        let mut book = CardBook::default();
        let id = book.add(CardKind::Note, "已有卡片", paths::timestamp(), paths::unix_ms());
        book.save(&dir.join("cards.json")).unwrap();
        let (mut core, _, _rx) = core(&dir);
        core.notes_tick();
        assert!(!core.notes.markdown_dirty);
        let daily = std::fs::read_dir(dir.join("随手记").join("进行中")).unwrap().next().unwrap().unwrap().path();
        std::fs::write(&daily, "手动修改，元数据变更不应碰此文件").unwrap();
        core.notes_call("cards.update", &json!({"id": id, "patch": {"x": 10, "y": 20, "style": {"padding": 12}}}))
            .unwrap()
            .unwrap();
        assert!(!core.notes.markdown_dirty);
        assert_eq!(std::fs::read_to_string(&daily).unwrap(), "手动修改，元数据变更不应碰此文件");
        assert_eq!(
            core.notes_call("cards.update", &json!({"id": id, "patch": {"text": "新内容"}})).unwrap(),
            Err(CARD_MARKDOWN_FAILED.to_string())
        );
        assert_eq!(CardBook::load(&dir.join("cards.json")).cards[0].text, "新内容");
        drop(core);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_or_corrupt_primary_data_does_not_erase_markdown_on_startup() {
        for corrupt in [false, true] {
            let dir = temp_dir(if corrupt { "markdown-corrupt" } else { "markdown-missing" });
            let mut book = CardBook::default();
            book.add(CardKind::Note, "保留可读的恢复材料", paths::timestamp(), paths::unix_ms());
            book.sync_markdown(&dir).unwrap();
            let daily = std::fs::read_dir(dir.join("随手记").join("进行中")).unwrap().next().unwrap().unwrap().path();
            let before = std::fs::read(&daily).unwrap();
            if corrupt {
                std::fs::write(dir.join("cards.json"), "{bad").unwrap();
            }
            let (mut core, _, _rx) = core(&dir);
            core.notes_tick();
            core.notes.flush().unwrap();
            assert!(!core.notes.markdown_dirty);
            assert_eq!(std::fs::read(&daily).unwrap(), before);
            let added = core.notes_call("cards.add", &json!({"text": "丢失主文件后新建"})).unwrap();
            assert_eq!(added, Err(CARD_MARKDOWN_FAILED.to_string()));
            assert_eq!(std::fs::read(&daily).unwrap(), before);
            let id = core.notes.cards.cards[0].id;
            core.notes_call("cards.update", &json!({"id": id, "patch": {"createdMs": paths::unix_ms() + 86_400_000}}))
                .unwrap()
                .unwrap();
            assert_eq!(std::fs::read(&daily).unwrap(), before);
            drop(core);
            let (mut restarted, _, _rx) = self::core(&dir);
            restarted.notes_tick();
            assert_eq!(std::fs::read(&daily).unwrap(), before, "protection survives a new valid JSON and restart");
            drop(restarted);
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn failed_cards_retry_without_another_edit_and_recover_on_exit() {
        let dir = temp_dir("cards-retry");
        std::fs::create_dir_all(dir.join("cards.json")).unwrap();
        let (mut core, _, _rx) = core(&dir);
        core.save_note("待办 保留这条");
        assert!(core.notes.cards_dirty);
        assert!(core.notes_deadline().is_some());
        std::fs::remove_dir(dir.join("cards.json")).unwrap();
        core.notes.retry_at = Instant::now();
        core.notes_tick();
        assert!(!core.notes.cards_dirty);
        assert_eq!(CardBook::load(&dir.join("cards.json")).cards.len(), 1);
        std::fs::remove_file(dir.join("cards.json")).unwrap();
        std::fs::create_dir(dir.join("cards.json")).unwrap();
        core.save_note("第二条");
        core.notes.flush().unwrap();
        let paths: Vec<_> = std::fs::read_dir(dir.join("recovery")).unwrap().map(|p| p.unwrap().path()).collect();
        assert_eq!(paths.len(), 1);
        assert_eq!(CardBook::load(&paths[0]).cards.len(), 2);
        drop(core);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cards_that_cannot_be_saved_are_not_reported_saved() {
        let dir = temp_dir("cards-fail");
        std::fs::create_dir_all(dir.join("cards.json")).unwrap();
        let (mut core, events, _rx) = core(&dir);
        core.save_note("待办 明天给张三回电话");
        let all = events.lock().unwrap().clone();
        assert!(!all.iter().any(|e| e["event"] == "note.saved"));
        assert_eq!(errors(&events, CARD_SAVE_FAILED), 1);
        let added = core.notes_call("cards.add", &json!({"text": "x"})).unwrap();
        assert_eq!(added, Err(CARD_SAVE_FAILED.to_string()));
        assert_eq!(core.notes.cards.cards.len(), 2, "kept in memory");
        drop(core);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn card_styles_are_set_per_card_and_cleared_for_all() {
        let dir = temp_dir("cards-style");
        let (mut core, events, _rx) = core(&dir);
        let a = core.notes_call("cards.add", &json!({"text": "甲"})).unwrap().unwrap()["id"].as_u64().unwrap();
        let b = core.notes_call("cards.add", &json!({"text": "乙"})).unwrap().unwrap()["id"].as_u64().unwrap();
        let style = json!({"background": "#FFFFF4B0", "cornerRadius": 2, "shadow": "none"});
        core.notes_call("cards.update", &json!({"id": a, "patch": {"style": style}})).unwrap().unwrap();
        core.notes_call("cards.update", &json!({"id": b, "patch": {"style": {"padding": 20}}})).unwrap().unwrap();
        let last = events.lock().unwrap().iter().rev().find(|e| e["event"] == "cards.changed").cloned().unwrap();
        assert_eq!(last["cards"][0]["style"]["background"], "#FFFFF4B0", "other windows see the new look");
        let saved = CardBook::load(&dir.join("cards.json"));
        assert_eq!(saved.cards[0].style.as_ref().unwrap().corner_radius, 2.0);
        // Back to the shared style for one card, then for all.
        core.notes_call("cards.update", &json!({"id": a, "patch": {"style": null}})).unwrap().unwrap();
        assert!(core.notes.cards.get(a).unwrap().style.is_none());
        let cleared = core.notes_call("cards.styles.clear", &json!({})).unwrap().unwrap();
        assert_eq!(cleared["cleared"], 1);
        assert!(CardBook::load(&dir.join("cards.json")).cards.iter().all(|c| c.style.is_none()));
        let bad = core.notes_call("cards.update", &json!({"id": a, "patch": {"style": "red"}})).unwrap();
        assert_eq!(bad, Err("bad_style".to_string()));
        drop(core);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn shutdown_preserves_cards_when_both_destinations_fail() {
        let dir = temp_dir("exit-cards");
        std::fs::create_dir(dir.join("cards.json")).unwrap();
        std::fs::write(dir.join("recovery"), b"blocked").unwrap();
        let (mut core, _, _rx) = core(&dir);
        core.save_note("不能丢失的便签");
        assert_eq!(core.call(&json!({"cmd": "shutdown"}))["error"], CARD_SAVE_FAILED);
        assert!(!core.exit_prepared);
        assert_eq!(core.call(&json!({"cmd": "cards.list"}))["cards"][0]["text"], "不能丢失的便签");
        std::fs::remove_file(dir.join("recovery")).unwrap();
        assert_eq!(core.call(&json!({"cmd": "shutdown"}))["ok"], true);
        let path = std::fs::read_dir(dir.join("recovery")).unwrap().next().unwrap().unwrap().path();
        assert_eq!(CardBook::load(&path).cards[0].text, "不能丢失的便签");
        core.shutdown();
        assert_eq!(std::fs::read_dir(dir.join("recovery")).unwrap().count(), 1);
        drop(core);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn shutdown_discards_queued_mutations() {
        let dir = temp_dir("exit-queue");
        let (mut core, _, rx) = core(&dir);
        let id = core.notes_call("cards.add", &json!({"text": "原话"})).unwrap().unwrap()["id"].as_u64().unwrap();
        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        core.tx.send(Msg::Call(json!({"cmd": "shutdown"}), shutdown_tx)).unwrap();
        let (edit_tx, edit_rx) = mpsc::channel();
        let late = json!({"cmd": "cards.update", "id": id, "patch": {"text": "too late"}});
        core.tx.send(Msg::Call(late, edit_tx)).unwrap();
        let tx = core.tx.clone();
        super::super::run(core, rx);
        assert_eq!(shutdown_rx.recv().unwrap()["ok"], true);
        assert!(edit_rx.recv().is_err(), "the command after shutdown was never executed");
        assert!(tx.send(Msg::Quit).is_err(), "the loop no longer receives messages");
        assert_eq!(CardBook::load(&dir.join("cards.json")).cards[0].text, "原话");
        let _ = std::fs::remove_dir_all(dir);
    }
}
