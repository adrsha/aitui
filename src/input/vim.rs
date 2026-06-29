/// The vim modal editing state.
#[derive(Debug, Clone, PartialEq, Eq)]
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

impl VimMode {
    pub fn is_command(&self) -> bool {
        matches!(self, VimMode::Command)
    }
}
