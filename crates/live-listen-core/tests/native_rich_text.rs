//! A synthetic document exported by the real WinUI RichEditBox via LiveListen.EditorChecks.

use live_listen_core::cards::{CardBook, CardKind};
use serde_json::{Value, json};

#[test]
fn native_rich_edit_output_survives_storage_and_daily_status_moves() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/native-note.json")).unwrap();
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("ll-native-note-{}-{stamp}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("cards.json");
    let mut book = CardBook::default();
    let id = book.add(CardKind::Todo, "fixture", "2026-11-08T02:00:00.000Z".into(), 1_794_103_200_000);
    book.update(id, &fixture).unwrap();
    book.save(&path).unwrap();
    book = CardBook::load(&path);
    let card = book.get(id).unwrap();
    assert_eq!(card.text, fixture["text"].as_str().unwrap());
    assert_eq!(card.rtf.as_deref(), fixture["rtf"].as_str());
    for (patch, folder) in
        [(json!({}), "进行中"), (json!({"done": true}), "已完成"), (json!({"archived": true}), "已归档")]
    {
        book.update(id, &patch).unwrap();
        book.sync_markdown(&dir).unwrap();
        let daily = std::fs::read_dir(dir.join("随手记").join(folder)).unwrap().next().unwrap().unwrap().path();
        let markdown = std::fs::read_to_string(daily).unwrap();
        assert!(markdown.contains("formatted") && markdown.contains("☐ todo one") && markdown.contains("– dash one"));
        assert_eq!(book.get(id).unwrap().rtf.as_deref(), fixture["rtf"].as_str());
    }
    assert_eq!(std::fs::read_dir(dir.join("随手记").join("进行中")).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(dir.join("随手记").join("已完成")).unwrap().count(), 0);
    std::fs::remove_dir_all(dir).unwrap();
}
