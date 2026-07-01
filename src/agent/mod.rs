pub mod executor;
pub mod parser;
pub mod tools;

pub use tools::{
    Permission, PermissionMemory, ToolCall, ToolResult, ToolRisk,
    agent_system_prompt,
};
// Re-exported but currently unused; available for tool-schema / function-calling
// integrations and live-stream tool parsing.
#[allow(unused_imports)]
pub use tools::{ToolKind, tool_schemas};
#[allow(unused_imports)]
pub use parser::{StreamingParser, strip_tool_blocks};
pub use executor::execute;
