use etcetera::AppStrategyArgs;
use once_cell::sync::Lazy;

pub static APP_STRATEGY: Lazy<AppStrategyArgs> = Lazy::new(|| AppStrategyArgs {
    top_level_domain: "Block".to_string(),
    author: "Block".to_string(),
    app_name: "goose".to_string(),
});

pub mod computercontroller;
mod developer;
mod memory;
pub mod nostr_memory_mcp;
mod tutorial;

pub use computercontroller::ComputerControllerRouter;
pub use developer::DeveloperRouter;
pub use memory::MemoryRouter;
pub use nostr_memory_mcp::NostrMcpRouter;
pub use tutorial::TutorialRouter;
