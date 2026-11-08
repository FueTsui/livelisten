//! Planning what to type into the target text box. Pure logic; the engine executes the ops
//! and confirms each one that reached the target ([`Writer::typed`], [`Writer::deleted`]),
//! so `written` never runs ahead of the text box.

use crate::config::WriteMode;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteOp {
    Type(String),
    /// Delete this many characters before the caret.
    Backspace(usize),
}

/// Tracks what this session has typed into the target and plans the next edits.
#[derive(Clone, Debug)]
pub struct Writer {
    mode: WriteMode,
    written: String,
    frozen: bool,
}

fn common_prefix_chars(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

impl Writer {
    pub fn new(mode: WriteMode) -> Self {
        Writer { mode, written: String::new(), frozen: false }
    }

    pub fn written(&self) -> &str {
        &self.written
    }

    fn may_delete(&self) -> bool {
        self.mode == WriteMode::Live && !self.frozen
    }

    /// Plans edits for the latest provider text. `stable` counts finalized leading characters.
    pub fn update(&self, text: &str, stable: usize) -> Vec<WriteOp> {
        let desired: String = if self.may_delete() { text.to_string() } else { text.chars().take(stable).collect() };
        self.plan(&desired)
    }

    /// Plans the last edits once the final text is known.
    pub fn finish(&self, final_text: &str) -> Vec<WriteOp> {
        self.plan(final_text)
    }

    /// `text` reached the target at the caret.
    pub fn typed(&mut self, text: &str) {
        self.written.push_str(text);
    }

    /// `count` characters before the caret were deleted.
    pub fn deleted(&mut self, count: usize) {
        let keep = self.written.chars().count().saturating_sub(count);
        self.written = self.written.chars().take(keep).collect();
    }

    /// Confirms ops that all reached the target.
    pub fn applied(&mut self, ops: &[WriteOp]) {
        for op in ops {
            match op {
                WriteOp::Type(text) => self.typed(text),
                WriteOp::Backspace(count) => self.deleted(*count),
            }
        }
    }

    /// Stops in-place corrections, e.g. after the target lost focus and the caret may have moved.
    pub fn freeze(&mut self) {
        self.frozen = true;
    }

    /// Allows corrections again once the caret is known to be where the text ended.
    pub fn thaw(&mut self) {
        self.frozen = false;
    }

    /// The part of `text` that has not reached the target.
    pub fn unwritten(&self, text: &str) -> String {
        match text.strip_prefix(self.written.as_str()) {
            Some(rest) => rest.to_string(),
            None => text.chars().skip(self.written.chars().count()).collect(),
        }
    }

    fn plan(&self, desired: &str) -> Vec<WriteOp> {
        let written_chars = self.written.chars().count();
        let common = common_prefix_chars(&self.written, desired);
        let mut ops = Vec::new();
        if common < written_chars {
            if !self.may_delete() {
                // Never delete: only append what goes beyond the text already written.
                let extra: String = desired.chars().skip(written_chars).collect();
                if !extra.is_empty() {
                    ops.push(WriteOp::Type(extra));
                }
                return ops;
            }
            ops.push(WriteOp::Backspace(written_chars - common));
        }
        let tail: String = desired.chars().skip(common).collect();
        if !tail.is_empty() {
            ops.push(WriteOp::Type(tail));
        }
        ops
    }
}

/// Removes control and bidirectional-override characters from service text before it is
/// typed or shown; line breaks and tabs become spaces.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\n' | '\r' | '\t' => Some(' '),
            c if c.is_control() => None,
            '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' => None,
            c => Some(c),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plans and confirms that every op reached the target.
    fn update(w: &mut Writer, text: &str, stable: usize) -> Vec<WriteOp> {
        let ops = w.update(text, stable);
        w.applied(&ops);
        ops
    }

    fn finish(w: &mut Writer, text: &str) -> Vec<WriteOp> {
        let ops = w.finish(text);
        w.applied(&ops);
        ops
    }

    #[test]
    fn stable_mode_appends_finalized_text_only() {
        let mut w = Writer::new(WriteMode::Stable);
        assert!(update(&mut w, "今天", 0).is_empty());
        assert_eq!(update(&mut w, "今天天气不错，", 7), vec![WriteOp::Type("今天天气不错，".into())]);
        assert!(update(&mut w, "今天天气不错，我们", 7).is_empty());
        assert_eq!(finish(&mut w, "今天天气不错，我们走吧。"), vec![WriteOp::Type("我们走吧。".into())]);
        assert_eq!(w.written(), "今天天气不错，我们走吧。");
    }

    #[test]
    fn stable_mode_never_deletes_on_mismatch() {
        let mut w = Writer::new(WriteMode::Stable);
        update(&mut w, "abc", 3);
        assert_eq!(finish(&mut w, "abd!"), vec![WriteOp::Type("!".into())]);
    }

    #[test]
    fn live_mode_corrects_in_place() {
        let mut w = Writer::new(WriteMode::Live);
        assert_eq!(update(&mut w, "今天天汽", 0), vec![WriteOp::Type("今天天汽".into())]);
        assert_eq!(update(&mut w, "今天天气好", 0), vec![WriteOp::Backspace(1), WriteOp::Type("气好".into())]);
        assert_eq!(update(&mut w, "今天", 2), vec![WriteOp::Backspace(3)]);
        assert_eq!(finish(&mut w, "今天好。"), vec![WriteOp::Type("好。".into())]);
    }

    #[test]
    fn frozen_live_mode_stops_deleting() {
        let mut w = Writer::new(WriteMode::Live);
        update(&mut w, "hello wrld", 0);
        w.freeze();
        assert!(update(&mut w, "hello world", 6).is_empty());
        assert_eq!(finish(&mut w, "hello world."), vec![WriteOp::Type("d.".into())]);
    }

    #[test]
    fn thawed_live_mode_corrects_again() {
        let mut w = Writer::new(WriteMode::Live);
        update(&mut w, "hello wrld", 0);
        w.freeze();
        w.thaw();
        assert_eq!(update(&mut w, "hello world", 0), vec![WriteOp::Backspace(3), WriteOp::Type("orld".into())]);
    }

    #[test]
    fn unwritten_text_is_what_is_left() {
        let mut w = Writer::new(WriteMode::Stable);
        update(&mut w, "一二三", 2);
        assert_eq!(w.unwritten("一二三四"), "三四");
    }

    #[test]
    fn planned_text_counts_only_once_confirmed() {
        let mut w = Writer::new(WriteMode::Live);
        // Typing was rejected: nothing counts as written, so all of it is left to copy.
        assert_eq!(w.update("你好世界", 0), vec![WriteOp::Type("你好世界".into())]);
        assert_eq!(w.unwritten("你好世界"), "你好世界");
        w.typed("你好世界");
        // A long correction done in two steps resumes where the first step stopped.
        assert_eq!(w.update("你好", 0), vec![WriteOp::Backspace(2)]);
        w.deleted(1);
        assert_eq!(w.written(), "你好世");
        assert_eq!(w.update("你好", 0), vec![WriteOp::Backspace(1)]);
        w.deleted(1);
        assert!(w.update("你好", 0).is_empty());
    }

    #[test]
    fn sanitize_strips_controls() {
        assert_eq!(sanitize("a\u{7}b\nc\u{202E}d"), "ab cd");
    }
}
