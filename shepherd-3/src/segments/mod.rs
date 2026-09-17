//! Module for controlling the Segments and their ADBMS6830B chips.
//!
//! For context, there are 5 segments, each with two ADBMS6830B chips. So, there are 10 ADBMS6830B chips total.

mod cache;
mod chips;
mod core;

/// Allows you to read the cache data.
pub const fn cache() -> &'static CacheData {
    &cache::CACHE
}

// Re-exports
pub use core::task::{segments_task, signal::SEGMENTS_FRESH_DATA_SIGNAL};
pub use chips::{
    ChipId, IndexByChip, ChipKind, SegmentId,
    cells::{CellId, IndexByCell},
    gpios::{GpioId, IndexByGpio},
};
pub use cache::{CacheData, RegisterCacheData, Reading, fault_counts, redundant_aux, cell_voltages, average_cell_voltages, filtered_cell_voltages, s_voltages, status_c, status_d, aux, status_a, status_b, pwm};
