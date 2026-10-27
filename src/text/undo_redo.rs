// 對應 Kotlin `text/UndoRedoStack.kt`:以文字快照為單位的 undo/redo。
// 原 Android app 的編輯器改用頁級的 PageEditHistory,本模組未被 UI 使用;
// 一併移植(含單元測試)以免行為遺失。
#![allow(dead_code)]
use std::collections::VecDeque;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub text: String,
    pub sel_start: usize,
    pub sel_end: usize,
}

pub struct UndoRedoStack {
    current: Snapshot,
    undo_stack: VecDeque<Snapshot>,
    redo_stack: VecDeque<Snapshot>,
    max_depth: usize,
}

impl UndoRedoStack {
    pub fn new(initial_text: String, initial_selection: usize, max_depth: usize) -> Self {
        let len = initial_text.chars().count();
        UndoRedoStack {
            current: clamp(
                Snapshot {
                    text: initial_text,
                    sel_start: initial_selection,
                    sel_end: initial_selection,
                },
                len,
            ),
            undo_stack: VecDeque::new(),
            redo_stack: VecDeque::new(),
            max_depth,
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }
    pub fn current(&self) -> &Snapshot {
        &self.current
    }

    /// 記錄新狀態。回傳是否產生歷史節點(文字有變)。
    pub fn record(&mut self, text: String, sel_start: usize, sel_end: usize) -> bool {
        let len = text.chars().count();
        let next = clamp(Snapshot {
            text,
            sel_start,
            sel_end,
        }, len);
        // 僅選取改變 → 更新 current,不進歷史。
        if next.text == self.current.text {
            self.current = next;
            return false;
        }
        self.undo_stack.push_back(std::mem::replace(&mut self.current, next));
        if self.undo_stack.len() > self.max_depth {
            self.undo_stack.pop_front();
        }
        self.redo_stack.clear();
        true
    }

    pub fn undo(&mut self) -> Option<&Snapshot> {
        let prev = self.undo_stack.pop_back()?;
        self.redo_stack.push_back(std::mem::replace(&mut self.current, prev));
        Some(&self.current)
    }

    pub fn redo(&mut self) -> Option<&Snapshot> {
        let nxt = self.redo_stack.pop_back()?;
        self.undo_stack.push_back(std::mem::replace(&mut self.current, nxt));
        Some(&self.current)
    }

    /// 重新載入整檔時重置歷史。
    pub fn reset(&mut self, text: String, selection: usize) {
        let len = text.chars().count();
        self.current = clamp(
            Snapshot {
                text,
                sel_start: selection,
                sel_end: selection,
            },
            len,
        );
        self.undo_stack.clear();
        self.redo_stack.clear();
    }

    pub fn is_dirty_against(&self, baseline: &str) -> bool {
        self.current.text != baseline
    }
}

fn clamp(s: Snapshot, len: usize) -> Snapshot {
    let mut a = s.sel_start.min(len);
    let mut b = s.sel_end.min(len);
    if a > b {
        std::mem::swap(&mut a, &mut b);
    }
    Snapshot {
        text: s.text,
        sel_start: a,
        sel_end: b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(t: &str) -> UndoRedoStack {
        UndoRedoStack::new(t.into(), 0, 200)
    }

    #[test]
    fn selection_only_change_not_recorded() {
        let mut h = stack("abc");
        assert!(!h.record("abc".into(), 1, 2));
        assert!(!h.can_undo());
        assert_eq!(h.current().sel_end, 2);
    }

    #[test]
    fn text_change_pushes_history() {
        let mut h = stack("a");
        assert!(h.record("ab".into(), 2, 2));
        assert!(h.can_undo());
        let s = h.undo().cloned().unwrap();
        assert_eq!(s.text, "a");
        assert!(h.can_redo());
    }

    #[test]
    fn redo_replays_new_text() {
        let mut h = stack("before");
        h.record("after".into(), 5, 5);
        h.undo();
        assert_eq!(h.redo().unwrap().text, "after");
    }

    #[test]
    fn max_depth_drops_oldest() {
        let mut h = UndoRedoStack::new("v0".into(), 0, 3);
        for i in 0..10 {
            h.record(format!("v{}", i + 1), 2, 2);
        }
        let mut n = 0;
        while h.undo().is_some() {
            n += 1;
        }
        assert!(n <= 3, "was {n}");
    }

    #[test]
    fn dirty_against_baseline() {
        let mut h = stack("x");
        assert!(!h.is_dirty_against("x"));
        h.record("y".into(), 1, 1);
        assert!(h.is_dirty_against("x"));
        h.undo();
        assert!(!h.is_dirty_against("x"));
    }
}
