pub mod access;
pub mod executor;
mod file_search;
pub mod parser;
pub mod tools;

pub use access::{describe_call, needs_hard_prompt, AccessVerdict};
pub use tools::{
    agent_system_prompt, Permission, PermissionDecision, PermissionMemory, PermissionScope,
    ToolCall, ToolResult,
};
// Re-exported but currently unused; available for tool-schema / function-calling
// integrations and live-stream tool parsing.
pub use executor::{configure_search, execute, SearchSettings};
#[allow(unused_imports)]
pub use parser::{strip_tool_blocks, StreamingParser};
#[allow(unused_imports)]
pub use tools::{tool_schemas, ToolKind};
