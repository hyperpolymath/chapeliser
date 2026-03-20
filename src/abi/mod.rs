// SPDX-License-Identifier: PMPL-1.0-or-later
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//
// ABI module — Rust-side types that mirror the Idris2 ABI definitions.
// The Idris2 proofs in src/abi/*.idr guarantee:
//   1. Data layout consistency across architectures
//   2. Partition completeness (no items lost or duplicated)
//   3. Gather correctness (all results preserved)
//   4. Serialization round-trip identity
//
// This Rust module provides the runtime types; Idris2 provides the compile-time proofs.

use serde::{Deserialize, Serialize};

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
    /// Slice assignments: slices[locale_id] = (start_index, count).
    pub slices: Vec<(u64, u64)>,
}

impl Partition {
    /// Create a per-item partition: each locale gets total/K items (last gets remainder).
    pub fn per_item(total_items: u64, num_locales: u32) -> Self {
        let k = num_locales as u64;
        let base = total_items / k;
        let remainder = total_items % k;

        let mut slices = Vec::with_capacity(num_locales as usize);
        let mut offset = 0u64;
        for i in 0..k {
            let count = base + if i < remainder { 1 } else { 0 };
            slices.push((offset, count));
            offset += count;
        }

        Self { total_items, num_locales, grain_size: 1, slices }
    }

    /// Create a chunked partition: items grouped into chunks of `grain_size`.
    pub fn chunked(total_items: u64, num_locales: u32, grain_size: u32) -> Self {
        let g = grain_size as u64;
        let num_chunks = (total_items + g - 1) / g;
        let chunks_per_locale = (num_chunks + num_locales as u64 - 1) / num_locales as u64;

        let mut slices = Vec::with_capacity(num_locales as usize);
        let mut offset = 0u64;
        for _ in 0..num_locales {
            let items_this_locale = (chunks_per_locale * g).min(total_items.saturating_sub(offset));
            slices.push((offset, items_this_locale));
            offset += items_this_locale;
        }

        Self { total_items, num_locales, grain_size, slices }
    }

    /// Verify partition completeness: sum of all slice counts == total_items.
    /// This is the runtime check; the Idris2 ABI proves it at compile time.
    pub fn verify_completeness(&self) -> bool {
        let sum: u64 = self.slices.iter().map(|(_, count)| count).sum();
        sum == self.total_items
    }

    /// Verify no overlaps: no two slices share any index.
    pub fn verify_no_overlap(&self) -> bool {
        for (i, (start_a, count_a)) in self.slices.iter().enumerate() {
            for (start_b, count_b) in self.slices.iter().skip(i + 1) {
                let end_a = start_a + count_a;
                let end_b = start_b + count_b;
                if start_a < &end_b && start_b < &end_a {
                    return false;
                }
            }
        }
        true
    }
}

/// Metadata about a serialization round-trip.
/// Idris2 ABI proves: deserialize(serialize(x)) == x for all x.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializationContract {
    /// Name of the serialization format.
    pub format: String,
    /// Maximum buffer size needed for any single item.
    pub max_item_bytes: usize,
    /// Whether the format is self-describing (JSON, CBOR) or schema-dependent (bincode, flatbuffers).
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
    pub strategy: String,
}

impl GatherResult {
    /// Verify that total_results == sum of locale_counts.
    pub fn verify_conservation(&self) -> bool {
        let sum: u64 = self.locale_counts.iter().sum();
        sum == self.total_results
    }
}
