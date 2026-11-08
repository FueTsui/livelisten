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

pub(super) struct Notes {
    cards: CardBook,
    cards_path: PathBuf,
    cards_dirty: bool,
    retry_at: Instant,
    recovery_dir: PathBuf,
}

impl Notes {
    pub(super) fn open(config: &Path) -> Notes {
        let cards_path = config.join("cards.json");
        Notes {
            cards: CardBook::load(&cards_path),
            cards_path,
            cards_dirty: false,
            retry_at: Instant::now(),
            recovery_dir: std::env::temp_dir().join("LiveListen-recovery"),
        }
    }

    /// An orderly exit writes the cards once more. If that fails, a uniquely named recovery
    /// copy is written; an error keeps the core callable so the user can free space and retry.
    pub(super) fn flush(&mut self) -> Result<(), &'static str> {
        if !self.cards_dirty {
            return Ok(());
        }
        self.cards_dirty = self.cards.save(&self.cards_path).is_err();
        if !self.cards_dirty {
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
        let result = self.notes.cards.save(&self.notes.cards_path);
        self.notes.cards_dirty = result.is_err();
        if let Err(e) = result {
            self.notes.retry_at = Instant::now() + CARD_RETRY;
            log::warn!("cards not saved: {}", e.kind());
            if !failed_before {
                self.emit_error("cards", CARD_SAVE_FAILED);
            }
            Err(err(CARD_SAVE_FAILED))
        } else {
            Ok(())
        }
    }

    fn emit_cards(&self) {
        self.emit(json!({
            "event": "cards.changed",
            "cards": self.notes.cards.cards,
            "showOnDesktop": self.notes.cards.show_on_desktop,
        }));
    }

    /// A spoken note finished: keep it as a note or to-do card.
    pub(super) fn save_note(&mut self, text: &str) {
        match self.notes.cards.add_spoken(text, paths::timestamp(), paths::unix_ms()) {
            Some(id) => {
                let kind = self.notes.cards.get(id).map(|c| c.kind).unwrap_or_default();
                if !self.notes.cards.show_on_desktop {
                    self.notes.cards.show_on_desktop = true;
                }
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
        let _ = self.save_cards();
        self.emit_cards();
    }

    pub(super) fn cards_shown(&self) -> bool {
        self.notes.cards.show_on_desktop
    }

    /// Retries a failed card save.
    pub(super) fn notes_tick(&mut self) {
        if self.notes.cards_dirty && Instant::now() >= self.notes.retry_at {
            let _ = self.save_cards();
        }
    }

    /// When the engine should try a failed card save again.
    pub(super) fn notes_deadline(&self) -> Option<Instant> {
        self.notes.cards_dirty.then_some(self.notes.retry_at)
    }

    pub(super) fn notes_call(&mut self, cmd: &str, request: &Value) -> Option<Result<Value, String>> {
        let result = match cmd {
            "cards.list" => Ok(json!({
                "cards": self.notes.cards.cards,
                "showOnDesktop": self.notes.cards.show_on_desktop,
                "unsaved": self.notes.cards_dirty,
            })),
            "cards.add" => {
                let kind = if text_of(request, "kind") == "todo" { CardKind::Todo } else { CardKind::Note };
                let card = self.notes.cards.add(kind, text_of(request, "text"), paths::timestamp(), paths::unix_ms());
                let saved = self.save_cards();
                self.emit_cards();
                saved.map(|()| json!({"id": card}))
            }
            "cards.update" => match id_of(request, "id") {
                Ok(card) => {
                    let patch = &request["patch"];
                    match self.notes.cards.update(card, patch) {
                        Ok(()) => {
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
                Ok(card) if self.notes.cards.remove(card) => {
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
