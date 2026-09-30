mod compact;
mod permissions_instructions;
mod realtime;

pub use compact::SUMMARIZATION_PROMPT;
pub use compact::SUMMARY_PREFIX;
pub use permissions_instructions::ApprovalPromptContext;
pub use permissions_instructions::PermissionsInstructions;
pub use realtime::BACKEND_PROMPT;
pub use realtime::END_INSTRUCTIONS;
pub use realtime::START_INSTRUCTIONS;
