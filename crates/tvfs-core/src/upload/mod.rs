//! Upload orchestration (UI independent): chunk planning, retries with
//! back-pressure, resume, statistics and the engine that runs them.

pub mod backoff;
pub mod engine;
pub mod file;
pub mod plan;
pub mod resume;
pub mod run;
pub mod stats;

pub use engine::{Event, FileResult, Outcome, RunReport, UploadRequest, Uploader};
pub use file::{inspect, walk_folder, Rejected, UploadFile};
pub use plan::{ChunkMode, ChunkPlan};
pub use resume::{ResumeRecord, ResumeStore};
pub use run::{Control, Phase, Run, RunStatus, Snapshot};
pub use stats::{HistoryStore, StatsReport};

#[cfg(test)]
mod tests;
