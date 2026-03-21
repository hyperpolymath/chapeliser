// SPDX-License-Identifier: PMPL-1.0-or-later
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//
// ABI module — Rust-side types that mirror the Idris2 ABI definitions.
//
// The Idris2 proofs in src/interface/abi/*.idr guarantee at compile time:
//   1. Partition completeness — no items lost or duplicated
//   2. Partition disjointness — no two slices overlap
//   3. Gather conservation — all results preserved
//   4. Serialisation round-trip — deserialize(serialize(x)) ≡ x
//   5. Retry isolation — retrying item i only affects slot i
//
// This Rust module provides the runtime types and runtime verification.
// The Idris2 proofs provide the compile-time guarantees.

use serde::{Deserialize, Serialize};

/// FFI result codes. Must match Chapeliser.ABI.Types.Result in Types.idr.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(i32)]
pub enum FfiResult {
    /// Operation succeeded (0)
    Ok = 0,
    /// Generic error (1)
    Error = 1,
    /// Invalid parameter (2)
    InvalidParam = 2,
    /// Out of memory — buffer too small (3)
    OutOfMemory = 3,
    /// Null pointer in FFI call (4)
    NullPointer = 4,
    /// Item processing failed after all retries (5)
    RetryExhausted = 5,
    /// Checkpoint save/load failed (6)
    CheckpointError = 6,
}

/// Partition strategy. Must match Chapeliser.ABI.Types.PartitionStrategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PartitionStrategy {
    /// Even distribution: locale i gets items [i*N/K .. (i+1)*N/K)
    PerItem,
    /// Fixed-size chunks of grainSize items, round-robin across locales
    Chunk,
    /// Dynamic work-stealing via Chapel's DynamicIters
    Adaptive,
    /// Block-distributed domain decomposition
    Spatial,
    /// Route by key hash: same key → same locale
    Keyed,
}

impl PartitionStrategy {
    /// Parse from manifest string
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "per-item" => Some(Self::PerItem),
            "chunk" => Some(Self::Chunk),
            "adaptive" => Some(Self::Adaptive),
            "spatial" => Some(Self::Spatial),
            "keyed" => Some(Self::Keyed),
            _ => None,
        }
    }
}

/// Gather strategy. Must match Chapeliser.ABI.Types.GatherStrategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GatherStrategy {
    /// Concatenate all results
    Merge,
    /// Fold results into one via reduce function
    Reduce,
    /// Logarithmic pairwise reduction across locales
    TreeReduce,
    /// Results available incrementally
    Stream,
    /// Return first result matching a predicate
    First,
}

impl GatherStrategy {
    /// Parse from manifest string
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "merge" => Some(Self::Merge),
            "reduce" => Some(Self::Reduce),
            "tree-reduce" => Some(Self::TreeReduce),
            "stream" => Some(Self::Stream),
            "first" => Some(Self::First),
            _ => None,
        }
    }
}

/// A contiguous range of items assigned to one locale.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Slice {
    /// First item index in this slice
    pub start: u64,
    /// Number of items in this slice
    pub count: u64,
}

/// A partition of N items across K locales.
/// Proven properties (from Idris2 ABI):
///   - Union of all slices == original collection (completeness)
///   - Intersection of any two slices == empty (no duplicates)
///   - Sum of slice lengths == N (conservation)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Partition {
    /// Total number of items being partitioned.
    pub total_items: u64,
    /// Number of locales (partitions).
    pub num_locales: u32,
    /// Grain size: items per task.
    pub grain_size: u32,
    /// Slice assignments: one per locale.
    pub slices: Vec<Slice>,
}

impl Partition {
    /// Create a per-item partition: each locale gets total/K items.
    /// First (N mod K) locales get one extra item.
    pub fn per_item(total_items: u64, num_locales: u32) -> Self {
        let k = num_locales as u64;
        let base = total_items / k;
        let remainder = total_items % k;

        let mut slices = Vec::with_capacity(num_locales as usize);
        let mut offset = 0u64;
        for i in 0..k {
            let count = base + if i < remainder { 1 } else { 0 };
            slices.push(Slice { start: offset, count });
            offset += count;
        }

        Self {
            total_items,
            num_locales,
            grain_size: 1,
            slices,
        }
    }

    /// Create a chunked partition: items grouped into chunks of `grain_size`.
    pub fn chunked(total_items: u64, num_locales: u32, grain_size: u32) -> Self {
        let g = grain_size as u64;
        let num_chunks = (total_items + g - 1) / g;
        let chunks_per_locale = (num_chunks + num_locales as u64 - 1) / num_locales as u64;

        let mut slices = Vec::with_capacity(num_locales as usize);
        let mut offset = 0u64;
        for _ in 0..num_locales {
            let items_this_locale =
                (chunks_per_locale * g).min(total_items.saturating_sub(offset));
            slices.push(Slice {
                start: offset,
                count: items_this_locale,
            });
            offset += items_this_locale;
        }

        Self {
            total_items,
            num_locales,
            grain_size,
            slices,
        }
    }

    /// Verify partition completeness: sum of all slice counts == total_items.
    /// This is the runtime check; the Idris2 ABI proves it at compile time.
    pub fn verify_completeness(&self) -> bool {
        let sum: u64 = self.slices.iter().map(|s| s.count).sum();
        sum == self.total_items
    }

    /// Verify no overlaps: no two slices share any index.
    pub fn verify_no_overlap(&self) -> bool {
        for (i, a) in self.slices.iter().enumerate() {
            for b in self.slices.iter().skip(i + 1) {
                let end_a = a.start + a.count;
                let end_b = b.start + b.count;
                if a.start < end_b && b.start < end_a {
                    return false;
                }
            }
        }
        true
    }

    /// Verify this is a valid partition (complete + disjoint).
    pub fn verify(&self) -> bool {
        self.verify_completeness() && self.verify_no_overlap()
    }
}

/// Metadata about a serialisation format.
/// Idris2 ABI proves: deserialize(serialize(x)) == x for all x.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializationContract {
    /// Name of the serialisation format (matches manifest options).
    pub format: String,
    /// Maximum buffer size needed for any single item.
    pub max_item_bytes: usize,
    /// Whether the format is self-describing (JSON, CBOR) or schema-dependent (bincode).
    pub self_describing: bool,
}

/// Result of a gather operation.
/// Proven property: |gathered| == sum(|locale_results|) for merge strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatherResult {
    /// Total results gathered across all locales.
    pub total_results: u64,
    /// Per-locale result counts (for verification).
    pub locale_counts: Vec<u64>,
    /// Gather strategy used.
    pub strategy: GatherStrategy,
}

impl GatherResult {
    /// Verify that total_results == sum of locale_counts (gather conservation).
    pub fn verify_conservation(&self) -> bool {
        let sum: u64 = self.locale_counts.iter().sum();
        sum == self.total_results
    }
}

/// Per-locale memory budget estimate.
/// Helps users determine whether their workload fits in available RAM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryBudget {
    /// Items assigned to this locale
    pub items_per_locale: u64,
    /// Input buffer memory (bytes)
    pub input_bytes: u64,
    /// Output buffer memory (bytes)
    pub output_bytes: u64,
    /// Size tracking arrays (bytes)
    pub metadata_bytes: u64,
    /// Total estimated memory per locale (bytes)
    pub total_bytes: u64,
    /// Total in megabytes (rounded up)
    pub total_mb: u64,
}

impl MemoryBudget {
    /// Calculate the per-locale memory budget for a workload.
    pub fn calculate(total_items: u64, num_locales: u32, max_item_bytes: u64) -> Self {
        let items_per_locale = total_items / num_locales as u64 + 1;
        let input_bytes = items_per_locale * max_item_bytes;
        let output_bytes = items_per_locale * max_item_bytes;
        let metadata_bytes = items_per_locale * 9; // 8 bytes size_t + 1 byte bool
        let total_bytes = input_bytes + output_bytes + metadata_bytes;
        let total_mb = (total_bytes + 1_048_575) / 1_048_576;

        Self {
            items_per_locale,
            input_bytes,
            output_bytes,
            metadata_bytes,
            total_bytes,
            total_mb,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_item_partition_is_valid() {
        let p = Partition::per_item(100, 4);
        assert!(p.verify(), "per-item partition should be valid");
        assert_eq!(p.slices.len(), 4);
        // 100 / 4 = 25 each
        assert_eq!(p.slices[0].count, 25);
        assert_eq!(p.slices[3].count, 25);
    }

    #[test]
    fn per_item_partition_handles_remainder() {
        let p = Partition::per_item(10, 3);
        assert!(p.verify());
        // 10 / 3 = 3 base, 1 remainder → first locale gets 4, others get 3
        assert_eq!(p.slices[0].count, 4);
        assert_eq!(p.slices[1].count, 3);
        assert_eq!(p.slices[2].count, 3);
    }

    #[test]
    fn chunked_partition_is_valid() {
        let p = Partition::chunked(100, 4, 10);
        assert!(p.verify(), "chunked partition should be valid");
    }

    #[test]
    fn gather_conservation() {
        let g = GatherResult {
            total_results: 100,
            locale_counts: vec![25, 25, 25, 25],
            strategy: GatherStrategy::Merge,
        };
        assert!(g.verify_conservation());
    }

    #[test]
    fn memory_budget_calculation() {
        let budget = MemoryBudget::calculate(500, 64, 10_485_760);
        // 500/64 + 1 = 8+1 = 9 items per locale (rounded)
        assert!(budget.items_per_locale > 0);
        assert!(budget.total_mb > 0);
        // With 10MB per item and ~9 items: ~180MB per locale
        assert!(budget.total_mb < 500, "should be reasonable for 9 items");
    }

    #[test]
    fn partition_strategy_parsing() {
        assert_eq!(PartitionStrategy::from_str("per-item"), Some(PartitionStrategy::PerItem));
        assert_eq!(PartitionStrategy::from_str("adaptive"), Some(PartitionStrategy::Adaptive));
        assert_eq!(PartitionStrategy::from_str("invalid"), None);
    }
}
