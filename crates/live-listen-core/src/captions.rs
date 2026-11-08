//! Caption lines: splitting recognized text into sentences.

use std::collections::VecDeque;

use serde::Serialize;

const MAX_LINES: usize = 200;
/// Long runs without punctuation are cut so a line never grows without bound.
const MAX_SENTENCE: usize = 48;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LineSource {
    Captions,
    Dictation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    pub id: u64,
    pub text: String,
    #[serde(rename = "final")]
    pub finished: bool,
    pub source: LineSource,
    /// When the first words of the line arrived, in the caller's clock (milliseconds).
    pub start_ms: u64,
    /// When the line was last changed.
    pub end_ms: u64,
}

impl Line {
    fn new(text: String, finished: bool, source: LineSource, at_ms: u64) -> Line {
        Line { id: 0, text, finished, source, start_ms: at_ms, end_ms: at_ms }
    }
}

#[derive(Clone, Debug)]
struct Cursor {
    session: u64,
    source: LineSource,
    /// Characters of the session text already emitted as finished lines.
    done: usize,
    /// Open (unfinished) line, if any.
    open: Option<u64>,
    /// When the session text first grew past each length: `(length, at_ms)`, in order.
    /// A sentence starts at the time its first character was first heard.
    seen: Vec<(usize, u64)>,
}

impl Cursor {
    fn note_length(&mut self, len: usize, at_ms: u64) {
        let known = self.seen.last().map_or(0, |&(l, _)| l);
        if len > known || self.seen.is_empty() {
            self.seen.push((len, at_ms));
        }
    }

    /// Time the character at `index` first appeared, or `fallback` when unknown.
    fn first_seen(&self, index: usize, fallback: u64) -> u64 {
        let mut before = 0;
        for &(len, at) in &self.seen {
            if index >= before && index < len {
                return at;
            }
            before = len;
        }
        fallback
    }
}

#[derive(Clone, Debug, Default)]
pub struct Captions {
    lines: VecDeque<Line>,
    next_id: u64,
    cursors: Vec<Cursor>,
}

fn is_end(chars: &[char], i: usize) -> bool {
    match chars[i] {
        '。' | '！' | '？' | '!' | '?' | '；' | ';' | '…' => true,
        '.' => chars.get(i + 1).is_none_or(|c| c.is_whitespace()),
        _ => false,
    }
}

impl Captions {
    pub fn begin(&mut self, session: u64, source: LineSource) {
        if !self.cursors.iter().any(|c| c.session == session) {
            self.cursors.push(Cursor { session, source, done: 0, open: None, seen: Vec::new() });
        }
    }

    /// Applies the latest session text at `at_ms`; returns lines that just became finished.
    pub fn update(&mut self, session: u64, text: &str, stable: usize, at_ms: u64) -> Vec<Line> {
        let Some(index) = self.cursors.iter().position(|c| c.session == session) else {
            return Vec::new();
        };
        let chars: Vec<char> = text.chars().collect();
        let stable = stable.min(chars.len());
        let mut finished = Vec::new();
        let mut cursor = self.cursors[index].clone();
        cursor.note_length(chars.len(), at_ms);
        let mut start = cursor.done.min(stable);
        let mut i = start;
        while i < stable {
            if is_end(&chars, i) || i + 1 - start >= MAX_SENTENCE {
                let sentence: String = chars[start..=i].iter().collect::<String>().trim().to_string();
                if !sentence.is_empty() {
                    let began = cursor.first_seen(start, at_ms);
                    finished.push(self.finish_open(&mut cursor, sentence, began, at_ms));
                }
                start = i + 1;
            }
            i += 1;
        }
        cursor.done = start;
        let tail: String = chars[start.min(chars.len())..].iter().collect::<String>().trim().to_string();
        let began = cursor.first_seen(start, at_ms);
        self.set_open(&mut cursor, tail, began, at_ms);
        self.cursors[index] = cursor;
        finished
    }

    /// Ends a session; the remaining text becomes a finished line.
    pub fn end(&mut self, session: u64, final_text: Option<&str>, at_ms: u64) -> Vec<Line> {
        let mut finished = match final_text {
            Some(text) => self.update(session, text, text.chars().count(), at_ms),
            None => Vec::new(),
        };
        if let Some(index) = self.cursors.iter().position(|c| c.session == session) {
            let mut cursor = self.cursors.remove(index);
            if let Some(id) = cursor.open.take() {
                if let Some(line) = self.lines.iter_mut().find(|l| l.id == id) {
                    line.finished = true;
                    line.end_ms = line.end_ms.max(at_ms);
                    finished.push(line.clone());
                }
            }
        }
        finished
    }

    /// `began` is when the sentence was first heard; it starts a new line, while an open line
    /// keeps its own start.
    fn finish_open(&mut self, cursor: &mut Cursor, text: String, began: u64, at_ms: u64) -> Line {
        if let Some(id) = cursor.open.take() {
            if let Some(line) = self.lines.iter_mut().find(|l| l.id == id) {
                line.text = text;
                line.finished = true;
                line.end_ms = at_ms.max(line.start_ms);
                return line.clone();
            }
        }
        let mut line = Line::new(text, true, cursor.source, began.min(at_ms));
        line.end_ms = at_ms;
        self.push(line)
    }

    fn set_open(&mut self, cursor: &mut Cursor, text: String, began: u64, at_ms: u64) {
        match (cursor.open, text.is_empty()) {
            (Some(id), true) => {
                self.lines.retain(|l| l.id != id);
                cursor.open = None;
            }
            (Some(id), false) => {
                if let Some(line) = self.lines.iter_mut().find(|l| l.id == id) {
                    line.text = text;
                    line.end_ms = at_ms.max(line.start_ms);
                }
            }
            (None, false) => {
                let mut line = Line::new(text, false, cursor.source, began.min(at_ms));
                line.end_ms = at_ms;
                let line = self.push(line);
                cursor.open = Some(line.id);
            }
            (None, true) => {}
        }
    }

    fn push(&mut self, mut line: Line) -> Line {
        self.next_id += 1;
        line.id = self.next_id;
        self.lines.push_back(line.clone());
        while self.lines.len() > MAX_LINES {
            self.lines.pop_front();
        }
        line
    }

    /// The newest `count` lines, oldest first.
    pub fn recent(&self, count: usize) -> Vec<Line> {
        let skip = self.lines.len().saturating_sub(count);
        self.lines.iter().skip(skip).cloned().collect()
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.cursors.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_committed_text_into_sentences() {
        let mut c = Captions::default();
        c.begin(1, LineSource::Captions);
        assert!(c.update(1, "大家好", 0, 100).is_empty());
        assert_eq!(c.recent(5).len(), 1);
        let done = c.update(1, "大家好。今天", 4, 900);
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].text, "大家好。");
        let lines = c.recent(5);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].finished && !lines[1].finished);
        assert_eq!(lines[1].text, "今天");
        // The finished line keeps its id and start time when it was the open line.
        assert_eq!(lines[0].id, done[0].id);
        assert_eq!((done[0].start_ms, done[0].end_ms), (100, 900));
        assert_eq!(lines[1].start_ms, 900);
    }

    #[test]
    fn sentences_confirmed_together_keep_their_own_start() {
        let mut c = Captions::default();
        c.begin(1, LineSource::Captions);
        c.update(1, "我们开会。", 0, 2000);
        c.update(1, "我们开会。小王交报价。", 0, 6000);
        c.update(1, "我们开会。小王交报价。好的。", 0, 9000);
        let done = c.end(1, Some("我们开会。小王交报价。好的。"), 12000);
        let starts: Vec<u64> = done.iter().map(|l| l.start_ms).collect();
        assert_eq!(starts, vec![2000, 6000, 9000]);
        assert!(done.iter().all(|l| l.end_ms == 12000));
    }

    #[test]
    fn end_finishes_the_tail() {
        let mut c = Captions::default();
        c.begin(7, LineSource::Dictation);
        c.update(7, "Hello world. How are", 13, 0);
        let done = c.end(7, Some("Hello world. How are you"), 50);
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].text, "How are you");
        let lines = c.recent(2);
        assert!(lines.iter().all(|l| l.finished));
        let json = serde_json::to_value(&lines[1]).unwrap();
        assert_eq!(json["final"], true);
        assert!(json.get("translation").is_none() && json.get("speaker").is_none());
    }

    #[test]
    fn decimal_points_do_not_split_and_long_runs_do() {
        let mut c = Captions::default();
        c.begin(1, LineSource::Captions);
        let done = c.update(1, "价格是3.5元", 7, 0);
        assert!(done.is_empty());
        let long = "一".repeat(60);
        let done = c.update(1, &long, 60, 0);
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].text.chars().count(), MAX_SENTENCE);
    }
}
