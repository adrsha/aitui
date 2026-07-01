/// The vim modal editing state.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VimMode {
    /// Normal mode: navigation, operators.
    Normal,
    /// Insert mode: typing text.
    Insert,
    /// Visual mode: character-wise selection.
    Visual,
    /// Command-line mode: typing a `:` command.
    Command,
    /// Operator pending: e.g. `d` waiting for a motion.
    Operator(char),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_variants_are_distinct() {
        use std::collections::HashSet;
        let modes = vec![
            VimMode::Normal,
            VimMode::Insert,
            VimMode::Visual,
            VimMode::Command,
            VimMode::Operator('d'),
            VimMode::Operator('y'),
        ];
        let unique: HashSet<_> = modes.into_iter().collect();
        assert_eq!(unique.len(), 6);
    }
}
