pub mod adapter;
pub mod adapters;
pub mod backend;
pub mod backup;
pub mod discover;
pub mod formats;
pub mod generic_adapter;
pub mod provider_sync;
pub mod run;

pub use adapter::AgentAdapter;
pub use discover::{augmented_path, discover, Discovery};
pub use divisi_protocol::McpServerSpec;
