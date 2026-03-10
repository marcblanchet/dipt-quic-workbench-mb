use quinn::Runtime;
use std::sync::Arc;

pub use tokio::spawn;
pub use tokio::task::JoinHandle;
pub use tokio::time;
pub use tokio::time::Sleep as Timer;

pub fn new_rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .expect("failed to initialize tokio")
}

pub fn active_rt() -> Arc<dyn Runtime> {
    Arc::new(quinn::TokioRuntime)
}
