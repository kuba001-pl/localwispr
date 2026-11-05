//! CommonMark code-fence tracking shared by recognition hints and insertion.

/// Tracks CommonMark fenced code blocks line by line. Shared by insertion
/// (is the caret inside code?) and recognition hints (which context is code?)
/// so both agree on what a fence is.
#[derive(Default)]
pub(crate) struct Fence(Option<(char, usize)>);

impl Fence {
    pub(crate) fn is_open(&self) -> bool {
        self.0.is_some()
    }

    /// Feeds one line (newline excluded). Returns whether it opened or closed
    /// a fence: at most three spaces of indentation, three or more matching
    /// markers, no backtick in a backtick fence's info string, and nothing but
    /// whitespace after a closing fence.
    pub(crate) fn line(&mut self, line: &str) -> bool {
        let trimmed = line.trim_start_matches(' ');
        let indentation = line.len() - trimmed.len();
        let marker = trimmed.chars().next().unwrap_or(' ');
        let run = trimmed.chars().take_while(|c| *c == marker).count();
        if indentation > 3 || !matches!(marker, '`' | '~') || run < 3 {
            return false;
        }
        let rest = &trimmed[run..];
        match self.0 {
            Some((open, length)) => {
                let closes = marker == open && run >= length && rest.trim().is_empty();
                if closes {
                    self.0 = None;
                }
                closes
            }
            None if marker == '~' || !rest.contains('`') => {
                self.0 = Some((marker, run));
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fences_follow_commonmark_indentation_info_and_closing_rules() {
        let open_after = |lines: &[&str]| {
            let mut fence = Fence::default();
            lines.iter().for_each(|line| {
                fence.line(line);
            });
            fence.is_open()
        };
        assert!(open_after(&["```rust"]));
        assert!(open_after(&["   ~~~"]));
        assert!(
            !open_after(&["    ```"]),
            "four spaces is an indented code line"
        );
        assert!(!open_after(&["``` has `tick"]));
        assert!(open_after(&["````", "```"]), "a shorter run cannot close");
        assert!(
            open_after(&["```", "``` trailing"]),
            "closing fences carry no info"
        );
        assert!(!open_after(&["```", "````  "]));
        assert!(open_after(&["~~~", "```"]), "markers must match");
    }
}
