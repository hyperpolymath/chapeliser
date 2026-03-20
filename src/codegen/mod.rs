// SPDX-License-Identifier: PMPL-1.0-or-later
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//
// Code generation engine for Chapeliser.
// Generates three artifacts from a chapeliser.toml manifest:
//   1. Chapel wrapper (.chpl) — coforall distribution, locale management, gather
//   2. Zig FFI bridge (.zig) — C-ABI interop between Chapel runtime and user code
//   3. C headers (.h) — generated from Idris2 ABI definitions (or inferred)

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

use crate::manifest::Manifest;

/// Generate all artifacts from a manifest: Chapel wrapper, Zig FFI, C headers.
/// Writes output to the specified directory.
pub fn generate_all(manifest: &Manifest, output_dir: &str) -> Result<()> {
    let out = Path::new(output_dir);
    fs::create_dir_all(out.join("chapel"))
        .context("Failed to create chapel output directory")?;
    fs::create_dir_all(out.join("zig"))
        .context("Failed to create zig output directory")?;
    fs::create_dir_all(out.join("include"))
        .context("Failed to create include output directory")?;

    generate_chapel_wrapper(manifest, &out.join("chapel"))?;
    generate_zig_ffi(manifest, &out.join("zig"))?;
    generate_c_header(manifest, &out.join("include"))?;
    generate_build_script(manifest, out)?;

    Ok(())
}

/// Generate the Chapel wrapper that distributes the workload across locales.
/// This is the core of Chapeliser — it turns a single-machine function into
/// a distributed Chapel program.
fn generate_chapel_wrapper(manifest: &Manifest, output_dir: &Path) -> Result<()> {
    let name = &manifest.workload.name;
    let safe_name = name.replace('-', "_");
    let grain = manifest.scaling.grain_size;

    // Select the distribution pattern based on partition strategy
    let distribution_code = match manifest.workload.partition.as_str() {
        "per-item" => format!(
            r#"// Per-item distribution: each task gets one item.
// Chapel coforall distributes items across locales automatically.
coforall loc in Locales do on loc {{
  const myChunk = localSlice(allItems, loc.id, numLocales);
  for item in myChunk {{
    const result = c_process_item(item);
    localResults.pushBack(result);
  }}
}}"#
        ),
        "chunk" => format!(
            r#"// Chunk distribution: items grouped into chunks of {grain}.
const numChunks = (totalItems + {grain} - 1) / {grain};
coforall chunkIdx in 0..#numChunks do on Locales[chunkIdx % numLocales] {{
  const lo = chunkIdx * {grain};
  const hi = min(lo + {grain}, totalItems);
  const chunkResult = c_process_chunk(allItems, lo, hi);
  localResults.pushBack(chunkResult);
}}"#,
            grain = grain
        ),
        "adaptive" => format!(
            r#"// Adaptive distribution: work-stealing with dynamic load balancing.
// Tasks are pulled from a shared queue, not pre-assigned.
var workQueue: shared WorkQueue({safe_name}Item);
workQueue.init(allItems);

coforall loc in Locales do on loc {{
  while true {{
    const (hasWork, item) = workQueue.tryPop();
    if !hasWork then break;
    const result = c_process_item(item);
    localResults.pushBack(result);
  }}
}}"#,
            safe_name = safe_name
        ),
        "spatial" => String::from(
            r#"// Spatial distribution: domain decomposition across locales.
// Each locale owns a contiguous region of the input domain.
const Space = allItems.domain;
const DistSpace = Space dmapped Block(boundingBox=Space);
var distData: [DistSpace] itemType = allItems;

forall item in distData {
  const result = c_process_item(item);
  localResults[item.locale.id].pushBack(result);
}"#
        ),
        "keyed" => format!(
            r#"// Keyed distribution: items routed to locales by key hash.
// All items with the same key land on the same locale.
coforall loc in Locales do on loc {{
  for item in allItems {{
    if keyHash(item) % numLocales:uint == loc.id:uint {{
      const result = c_process_item(item);
      localResults.pushBack(result);
    }}
  }}
}}"#
        ),
        _ => String::from("// Unknown partition strategy — see chapeliser.toml"),
    };

    // Select gather pattern
    let gather_code = match manifest.workload.gather.as_str() {
        "merge" => String::from(
            r#"// Merge: concatenate all locale results into final output.
var finalResults: [0..#totalResultCount] outputType;
var offset = 0;
for loc in Locales {
  on loc {
    const n = localResults.size;
    finalResults[offset..#n] = localResults.toArray();
    offset += n;
  }
}"#
        ),
        "reduce" => format!(
            r#"// Reduce: apply reduction across all locale results.
var finalResult = identity_{safe_name};
for loc in Locales {{
  on loc {{
    for r in localResults {{
      finalResult = c_reduce(finalResult, r);
    }}
  }}
}}"#,
            safe_name = safe_name
        ),
        "tree-reduce" => String::from(
            r#"// Tree-reduce: logarithmic reduction for associative operations.
// Halves the number of active locales each round.
var active = numLocales;
while active > 1 {
  const half = active / 2;
  coforall i in 0..#half {
    on Locales[i] {
      const partner = i + half;
      const partnerResults = localResults[partner];
      for r in partnerResults {
        localResults[i].pushBack(r);
      }
    }
  }
  active = (active + 1) / 2;
}"#
        ),
        "stream" => String::from(
            r#"// Stream: results sent to coordinator as they complete.
// Uses Chapel channels for back-pressure.
var resultChannel: Channel(outputType);
coforall loc in Locales do on loc {
  for r in localResults {
    resultChannel.send(r);
  }
}
resultChannel.close();"#
        ),
        "first" => String::from(
            r#"// First: return as soon as any locale produces a result.
// Used for search workloads where you want the first match.
var found: atomic bool;
var firstResult: outputType;
coforall loc in Locales do on loc {
  for item in myChunk {
    if found.read() then break;
    const result = c_process_item(item);
    if is_match(result) {
      if found.testAndSet() == false {
        firstResult = result;
      }
      break;
    }
  }
}"#
        ),
        _ => String::from("// Unknown gather strategy"),
    };

    let chapel_source = format!(
        r#"// SPDX-License-Identifier: PMPL-1.0-or-later
// Auto-generated by Chapeliser — do not edit manually.
// Workload: {name}
// Partition: {partition}, Gather: {gather}
// Regenerate with: chapeliser generate

module {safe_name}_Distributed {{

  // Import the C-ABI FFI bridge (generated Zig, compiled to .o)
  extern proc c_process_item(item: c_ptr(void)): c_ptr(void);
  extern proc c_process_chunk(items: c_ptr(void), lo: c_int, hi: c_int): c_ptr(void);
  extern proc c_reduce(a: c_ptr(void), b: c_ptr(void)): c_ptr(void);
  extern proc c_serialize(item: c_ptr(void), buf: c_ptr(c_uchar), len: c_ptr(c_size_t)): c_int;
  extern proc c_deserialize(buf: c_ptr(c_uchar), len: c_size_t): c_ptr(void);
  extern proc c_free_item(item: c_ptr(void));

  use CTypes;
  use BlockDist;
  use List;

  config const totalItems: int = 0;

  proc main() {{
    const numLocales = Locales.size;
    writeln("Chapeliser: distributing '", "{name}", "' across ", numLocales, " locale(s)");
    writeln("  partition={partition}, gather={gather}, grain={grain}");

    // Per-locale result accumulators
    var localResults: [LocaleSpace] list(c_ptr(void));

    // --- Distribution Phase ---
    {distribution_code}

    // --- Gather Phase ---
    {gather_code}

    writeln("Chapeliser: workload complete");
  }}

  // Helper: compute the slice of items belonging to a given locale.
  proc localSlice(allItems, localeId: int, numLocales: int) {{
    const total = allItems.size;
    const chunkSize = (total + numLocales - 1) / numLocales;
    const lo = localeId * chunkSize;
    const hi = min(lo + chunkSize, total);
    return allItems[lo..#(hi - lo)];
  }}

  // Helper: hash a key for keyed distribution.
  proc keyHash(item): uint {{
    // Default: use first 8 bytes of serialized item as hash.
    // Override by providing a custom key function in chapeliser.toml.
    var buf: [0..7] uint(8);
    var len: c_size_t = 8;
    c_serialize(item, c_ptrTo(buf[0]), c_ptrTo(len));
    var h: uint = 0;
    for b in buf do h = h * 31 + b:uint;
    return h;
  }}
}}
"#,
        name = name,
        safe_name = safe_name,
        partition = manifest.workload.partition,
        gather = manifest.workload.gather,
        grain = grain,
        distribution_code = distribution_code,
        gather_code = gather_code,
    );

    let out_path = output_dir.join(format!("{}_distributed.chpl", safe_name));
    fs::write(&out_path, chapel_source)
        .with_context(|| format!("Failed to write Chapel wrapper: {}", out_path.display()))?;
    println!("  Chapel wrapper: {}", out_path.display());

    Ok(())
}

/// Generate the Zig FFI bridge that connects Chapel's extern calls to the user's code.
fn generate_zig_ffi(manifest: &Manifest, output_dir: &Path) -> Result<()> {
    let safe_name = manifest.workload.name.replace('-', "_");

    let zig_source = format!(
        r#"// SPDX-License-Identifier: PMPL-1.0-or-later
// Auto-generated by Chapeliser — do not edit manually.
// Zig FFI bridge for workload: {name}
// Regenerate with: chapeliser generate

const std = @import("std");

// --- User-provided symbols (link against user's compiled code) ---
// These are the actual processing functions from the user's application.
// Chapeliser expects them to be exported with C linkage.
extern fn {safe_name}_process_item(item: ?*anyopaque) callconv(.C) ?*anyopaque;
extern fn {safe_name}_process_chunk(items: ?*anyopaque, lo: c_int, hi: c_int) callconv(.C) ?*anyopaque;
extern fn {safe_name}_reduce(a: ?*anyopaque, b: ?*anyopaque) callconv(.C) ?*anyopaque;
extern fn {safe_name}_serialize(item: ?*anyopaque, buf: [*]u8, len: *usize) callconv(.C) c_int;
extern fn {safe_name}_deserialize(buf: [*]const u8, len: usize) callconv(.C) ?*anyopaque;
extern fn {safe_name}_free(item: ?*anyopaque) callconv(.C) void;

// --- Chapel-facing C-ABI exports ---
// These are called by the generated Chapel code via `extern proc`.

export fn c_process_item(item: ?*anyopaque) callconv(.C) ?*anyopaque {{
    return {safe_name}_process_item(item);
}}

export fn c_process_chunk(items: ?*anyopaque, lo: c_int, hi: c_int) callconv(.C) ?*anyopaque {{
    return {safe_name}_process_chunk(items, lo, hi);
}}

export fn c_reduce(a: ?*anyopaque, b: ?*anyopaque) callconv(.C) ?*anyopaque {{
    return {safe_name}_reduce(a, b);
}}

export fn c_serialize(item: ?*anyopaque, buf: [*]u8, len: *usize) callconv(.C) c_int {{
    return {safe_name}_serialize(item, buf, len);
}}

export fn c_deserialize(buf: [*]const u8, len: usize) callconv(.C) ?*anyopaque {{
    return {safe_name}_deserialize(buf, len);
}}

export fn c_free_item(item: ?*anyopaque) callconv(.C) void {{
    {safe_name}_free(item);
}}
"#,
        name = manifest.workload.name,
        safe_name = safe_name,
    );

    let out_path = output_dir.join(format!("{}_ffi.zig", safe_name));
    fs::write(&out_path, zig_source)
        .with_context(|| format!("Failed to write Zig FFI bridge: {}", out_path.display()))?;
    println!("  Zig FFI bridge: {}", out_path.display());

    Ok(())
}

/// Generate C header that declares the FFI interface.
/// This is the contract between the user's code and the Chapel/Zig layer.
fn generate_c_header(manifest: &Manifest, output_dir: &Path) -> Result<()> {
    let safe_name = manifest.workload.name.replace('-', "_");

    let header = format!(
        r#"/* SPDX-License-Identifier: PMPL-1.0-or-later */
/* Auto-generated by Chapeliser — do not edit manually. */
/* C header for workload: {name} */
/* Regenerate with: chapeliser generate */

#ifndef CHAPELISER_{upper}_H
#define CHAPELISER_{upper}_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {{
#endif

/*
 * User must implement these 6 functions with C linkage.
 * They are called by the Chapeliser-generated Zig FFI bridge,
 * which is in turn called by the Chapel distributed wrapper.
 *
 * For Rust users: use #[no_mangle] extern "C" fn ...
 * For Zig users: use export fn ... callconv(.C) ...
 * For C users: just implement normally.
 */

/* Process a single work item. Returns a result pointer. */
void* {safe_name}_process_item(void* item);

/* Process a chunk of items from index lo to hi. Returns aggregated result. */
void* {safe_name}_process_chunk(void* items, int lo, int hi);

/* Reduce two results into one (for reduce/tree-reduce gather). */
void* {safe_name}_reduce(void* a, void* b);

/* Serialize an item/result into buf. Sets *len to actual bytes written. */
/* Returns 0 on success, non-zero on error. */
int {safe_name}_serialize(void* item, uint8_t* buf, size_t* len);

/* Deserialize bytes into an item/result. Returns pointer to deserialized object. */
void* {safe_name}_deserialize(const uint8_t* buf, size_t len);

/* Free an item/result allocated by process, reduce, or deserialize. */
void {safe_name}_free(void* item);

#ifdef __cplusplus
}}
#endif

#endif /* CHAPELISER_{upper}_H */
"#,
        name = manifest.workload.name,
        safe_name = safe_name,
        upper = safe_name.to_uppercase(),
    );

    let out_path = output_dir.join(format!("{}_chapeliser.h", safe_name));
    fs::write(&out_path, header)
        .with_context(|| format!("Failed to write C header: {}", out_path.display()))?;
    println!("  C header:       {}", out_path.display());

    Ok(())
}

/// Generate a build script that compiles all generated artifacts.
fn generate_build_script(manifest: &Manifest, output_dir: &Path) -> Result<()> {
    let safe_name = manifest.workload.name.replace('-', "_");

    let script = format!(
        r#"#!/usr/bin/env bash
# SPDX-License-Identifier: PMPL-1.0-or-later
# Auto-generated by Chapeliser — build script for workload: {name}
# Regenerate with: chapeliser generate

set -euo pipefail

echo "=== Chapeliser Build: {name} ==="

# Step 1: Compile the Zig FFI bridge to a C-ABI object file
echo "[1/3] Compiling Zig FFI bridge..."
zig build-obj -O ReleaseFast zig/{safe_name}_ffi.zig -femit-bin={safe_name}_ffi.o

# Step 2: Compile the user's application (adjust for your build system)
echo "[2/3] Linking user code..."
# For Rust: cargo build --release && cp target/release/lib*.a .
# For C:    gcc -c -O2 your_code.c -o user_code.o
# For Zig:  zig build-obj -O ReleaseFast your_code.zig

# Step 3: Compile the Chapel wrapper, linking FFI + user code
echo "[3/3] Compiling Chapel distributed wrapper..."
chpl --fast \
    -I include/ \
    chapel/{safe_name}_distributed.chpl \
    {safe_name}_ffi.o \
    -o {safe_name}_distributed

echo "=== Build complete: ./{safe_name}_distributed ==="
echo "Run with: ./{safe_name}_distributed -nl <num_locales>"
"#,
        name = manifest.workload.name,
        safe_name = safe_name,
    );

    let out_path = output_dir.join("build.sh");
    fs::write(&out_path, &script)
        .with_context(|| format!("Failed to write build script: {}", out_path.display()))?;

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&out_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&out_path, perms)?;
    }

    println!("  Build script:   {}", out_path.display());
    Ok(())
}

/// Build the generated artifacts. Requires `chpl` and `zig` on PATH.
pub fn build(manifest: &Manifest, release: bool) -> Result<()> {
    let safe_name = manifest.workload.name.replace('-', "_");
    println!("Building Chapelised workload: {}", manifest.workload.name);

    // Check for required tools
    let chpl_check = std::process::Command::new("chpl").arg("--version").output();
    if chpl_check.is_err() {
        anyhow::bail!(
            "Chapel compiler (chpl) not found on PATH.\n\
             Install Chapel: https://chapel-lang.org/download.html\n\
             Or use: chapeliser generate  (to generate files without building)"
        );
    }

    let zig_check = std::process::Command::new("zig").arg("version").output();
    if zig_check.is_err() {
        anyhow::bail!(
            "Zig compiler not found on PATH.\n\
             Install Zig: https://ziglang.org/download/\n\
             Or use: chapeliser generate  (to generate files without building)"
        );
    }

    // Run the generated build script
    let build_dir = Path::new("generated/chapeliser");
    let status = std::process::Command::new("bash")
        .arg(build_dir.join("build.sh"))
        .current_dir(build_dir)
        .status()
        .context("Failed to run build script")?;

    if !status.success() {
        anyhow::bail!("Build failed with exit code: {:?}", status.code());
    }

    let _ = release; // TODO: pass optimisation level to Chapel compiler
    println!("Build successful: generated/chapeliser/{}_distributed", safe_name);
    Ok(())
}

/// Run the Chapelised workload with the specified number of locales.
pub fn run(manifest: &Manifest, locales: u32, cluster: Option<&str>, args: &[String]) -> Result<()> {
    let safe_name = manifest.workload.name.replace('-', "_");
    let binary = format!("generated/chapeliser/{}_distributed", safe_name);

    if !Path::new(&binary).exists() {
        anyhow::bail!(
            "Binary not found: {}. Run 'chapeliser build' first.",
            binary
        );
    }

    println!("Running {} on {} locale(s)", manifest.workload.name, locales);

    let mut cmd = std::process::Command::new(&binary);
    cmd.arg(format!("-nl{}", locales));

    if let Some(cluster_file) = cluster {
        // Chapel uses CHPL_COMM and environment variables for cluster config.
        // Parse cluster.toml and set the appropriate env vars.
        println!("Using cluster config: {}", cluster_file);
        // TODO: parse cluster.toml and set GASNET_SPAWNFN, SSH_SERVERS, etc.
    }

    for arg in args {
        cmd.arg(arg);
    }

    let status = cmd.status()
        .context("Failed to execute Chapel binary")?;

    if !status.success() {
        anyhow::bail!("Workload exited with code: {:?}", status.code());
    }

    Ok(())
}
