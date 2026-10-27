// 對應 Kotlin `text/PageEditHistory.kt`:頁級編輯歷史(節點=單頁新舊內容)。
use std::collections::VecDeque;

#[derive(Clone, Debug)]
pub struct Edit {
    pub page_index: usize,
    pub old_text: String,
    pub new_text: String,
    pub sel_start: usize,
    pub sel_end: usize,
}

pub struct PageEditHistory {
    undo_stack: VecDeque<Edit>,
    redo_stack: VecDeque<Edit>,
    max_depth: usize,
}

impl PageEditHistory {
    pub fn new(max_depth: usize) -> Self {
        PageEditHistory {
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

    /// 內容沒變(僅游標移動)不進歷史。回傳是否產生節點。
    pub fn record(&mut self, edit: Edit) -> bool {
        if edit.old_text == edit.new_text {
            return false;
        }
        self.undo_stack.push_back(edit);
        if self.undo_stack.len() > self.max_depth {
            self.undo_stack.pop_front();
        }
        self.redo_stack.clear();
        true
    }

    /// 回傳要還原成 old_text 的那筆(呼叫端把 pages[idx] 換成 old_text)。
    pub fn undo(&mut self) -> Option<Edit> {
        let e = self.undo_stack.pop_back()?;
        self.redo_stack.push_back(e.clone());
        Some(e)
    }

    /// 回傳要重放成 new_text 的那筆。
    pub fn redo(&mut self) -> Option<Edit> {
        let e = self.redo_stack.pop_back()?;
        self.undo_stack.push_back(e.clone());
        Some(e)
    }

    pub fn reset(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(idx: usize, old: &str, new: &str) -> Edit {
        Edit {
            page_index: idx,
            old_text: old.into(),
            new_text: new.into(),
            sel_start: new.chars().count(),
            sel_end: new.chars().count(),
        }
    }

    #[test]
    fn record_then_undo_returns_old_text() {
        let mut h = PageEditHistory::new(60);
        assert!(h.record(edit(2, "old", "new")));
        let e = h.undo().unwrap();
        assert_eq!(e.page_index, 2);
        assert_eq!(e.old_text, "old");
    }

    #[test]
    fn no_op_edit_not_recorded() {
        let mut h = PageEditHistory::new(60);
        assert!(!h.record(edit(0, "same", "same")));
        assert!(!h.can_undo());
    }

    #[test]
    fn cross_save_undo_survives() {
        let mut h = PageEditHistory::new(60);
        h.record(edit(0, "a", "ab"));
        h.record(edit(1, "x", "xy"));
        assert_eq!(h.undo().unwrap().page_index, 1);
        assert_eq!(h.undo().unwrap().page_index, 0);
    }

    #[test]
    fn redo_restores_new_text() {
        let mut h = PageEditHistory::new(60);
        h.record(edit(0, "before", "after"));
        h.undo();
        assert_eq!(h.redo().unwrap().new_text, "after");
    }

    #[test]
    fn max_depth_caps() {
        let mut h = PageEditHistory::new(3);
        for i in 0..10 {
            h.record(edit(0, &format!("v{i}"), &format!("v{}", i + 1)));
        }
        let mut n = 0;
        while h.undo().is_some() {
            n += 1;
        }
        assert!(n <= 3, "was {n}");
    }
}
