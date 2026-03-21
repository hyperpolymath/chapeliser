// SPDX-License-Identifier: PMPL-1.0-or-later
// Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
//
// Integration tests for Chapeliser CLI.

use std::fs;

#[test]
fn test_init_creates_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = dir.path().join("chapeliser.toml");

    chapeliser::manifest::init_manifest(dir.path().to_str().unwrap()).unwrap();
    assert!(manifest.exists(), "chapeliser.toml should be created");

    let content = fs::read_to_string(&manifest).unwrap();
    assert!(content.contains("[workload]"));
    assert!(content.contains("partition"));
    assert!(content.contains("gather"));
}

#[test]
fn test_validate_good_manifest() {
    let m = chapeliser::manifest::load_manifest("examples/panic-attacker/chapeliser.toml").unwrap();
    chapeliser::manifest::validate(&m).unwrap();
    assert_eq!(m.workload.name, "mass-panic");
    assert_eq!(m.workload.partition, "per-item");
    assert_eq!(m.workload.gather, "merge");
    assert_eq!(m.scaling.max_nodes, 64);
}

#[test]
fn test_generate_produces_files() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("generated");

    let m = chapeliser::manifest::load_manifest("examples/panic-attacker/chapeliser.toml").unwrap();
    chapeliser::codegen::generate_all(&m, output.to_str().unwrap()).unwrap();

    // Check all 4 artifacts exist
    assert!(output.join("chapel/mass_panic_distributed.chpl").exists());
    assert!(output.join("zig/mass_panic_ffi.zig").exists());
    assert!(output.join("include/mass_panic_chapeliser.h").exists());
    assert!(output.join("build.sh").exists());

    // Check Chapel file contains expected content
    let chpl = fs::read_to_string(output.join("chapel/mass_panic_distributed.chpl")).unwrap();
    assert!(chpl.contains("coforall"), "Chapel should contain coforall distribution");
    assert!(chpl.contains("mass_panic"), "Chapel should reference workload name");
    assert!(chpl.contains("c_process_item"), "Chapel should call c_process_item");
    assert!(chpl.contains("c_init"), "Chapel should call c_init for lifecycle");
    assert!(chpl.contains("c_load_item"), "Chapel should call c_load_item for data I/O");
    assert!(chpl.contains("c_store_result"), "Chapel should call c_store_result");
    assert!(chpl.contains("processWithRetry"), "Chapel should have retry logic");

    // Check Zig file contains expected exports
    let zig = fs::read_to_string(output.join("zig/mass_panic_ffi.zig")).unwrap();
    assert!(zig.contains("export fn c_process_item"), "Zig should export c_process_item");
    assert!(zig.contains("mass_panic_process_item"), "Zig should delegate to user's mass_panic_process_item");
    assert!(zig.contains("export fn c_init"), "Zig should export c_init");
    assert!(zig.contains("export fn c_load_item"), "Zig should export c_load_item");

    // Check C header contains guards and function declarations
    let h = fs::read_to_string(output.join("include/mass_panic_chapeliser.h")).unwrap();
    assert!(h.contains("#ifndef CHAPELISER_MASS_PANIC_H"), "Header should have include guard");
    assert!(h.contains("mass_panic_process_item"), "Header should declare process_item");
    assert!(h.contains("mass_panic_init"), "Header should declare init");
    assert!(h.contains("mass_panic_load_item"), "Header should declare load_item");
    assert!(h.contains("mass_panic_store_result"), "Header should declare store_result");
}

#[test]
fn test_validate_rejects_bad_partition() {
    let toml = r#"
    [workload]
    name = "test"
    entry = "src/lib.rs::func"
    partition = "invalid_strategy"
    gather = "merge"

    [data]
    input-type = "Vec<Item>"
    output-type = "Vec<Result>"

    [scaling]
    min-nodes = 1
    max-nodes = 4
    grain-size = 10
    "#;

    let m: chapeliser::manifest::Manifest = toml::from_str(toml).unwrap();
    let result = chapeliser::manifest::validate(&m);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Unknown partition strategy"));
}

#[test]
fn test_all_partition_strategies_generate() {
    let strategies = ["per-item", "chunk", "adaptive", "spatial", "keyed"];

    for strategy in &strategies {
        let toml_str = format!(r#"
        [workload]
        name = "test-{strategy}"
        entry = "src/lib.rs::func"
        partition = "{strategy}"
        gather = "merge"

        [data]
        input-type = "Vec<Item>"
        output-type = "Vec<Result>"

        [scaling]
        min-nodes = 1
        max-nodes = 4
        grain-size = 10
        "#);

        let m: chapeliser::manifest::Manifest = toml::from_str(&toml_str).unwrap();
        chapeliser::manifest::validate(&m).unwrap();

        let dir = tempfile::tempdir().unwrap();
        chapeliser::codegen::generate_all(&m, dir.path().to_str().unwrap())
            .unwrap_or_else(|e| panic!("Failed to generate for strategy {strategy}: {e}"));

        let safe_name = format!("test_{}", strategy.replace('-', "_"));
        let chpl_path = dir.path().join(format!("chapel/{safe_name}_distributed.chpl"));
        assert!(chpl_path.exists(), "Chapel file missing for strategy {strategy}: expected {}", chpl_path.display());

        // Verify generated Chapel is non-trivial and contains distribution logic
        let chpl = fs::read_to_string(&chpl_path).unwrap();
        assert!(chpl.contains("c_init"), "Chapel {strategy} should call c_init");
        assert!(chpl.contains("c_load_item"), "Chapel {strategy} should load items");
        assert!(chpl.contains("c_process_item"), "Chapel {strategy} should process items");
        assert!(chpl.contains("processWithRetry"), "Chapel {strategy} should have retry logic");
        assert!(chpl.contains("c_store_result"), "Chapel {strategy} should store results");
        assert!(chpl.contains("c_shutdown"), "Chapel {strategy} should call shutdown");
    }
}

#[test]
fn test_all_gather_strategies_generate() {
    let gathers = ["merge", "reduce", "tree-reduce", "stream", "first"];

    for gather in &gathers {
        let toml_str = format!(r#"
        [workload]
        name = "test-{gather}"
        entry = "src/lib.rs::func"
        partition = "per-item"
        gather = "{gather}"

        [data]
        input-type = "Vec<Item>"
        output-type = "Vec<Result>"

        [scaling]
        min-nodes = 1
        max-nodes = 4
        grain-size = 10
        "#);

        let m: chapeliser::manifest::Manifest = toml::from_str(&toml_str).unwrap();
        chapeliser::manifest::validate(&m).unwrap();

        let dir = tempfile::tempdir().unwrap();
        chapeliser::codegen::generate_all(&m, dir.path().to_str().unwrap())
            .unwrap_or_else(|e| panic!("Failed to generate for gather {gather}: {e}"));

        let safe_name = format!("test_{}", gather.replace('-', "_"));
        let chpl_path = dir.path().join(format!("chapel/{safe_name}_distributed.chpl"));
        let chpl = fs::read_to_string(&chpl_path).unwrap();

        // Each gather strategy should produce strategy-specific output
        match *gather {
            "merge" => assert!(chpl.contains("merge"), "merge gather should mention merge"),
            "reduce" => assert!(chpl.contains("c_reduce"), "reduce gather should call c_reduce"),
            "tree-reduce" => assert!(chpl.contains("tree"), "tree-reduce should mention tree"),
            "stream" => assert!(chpl.contains("Stream"), "stream gather should mention stream"),
            "first" => assert!(chpl.contains("c_is_match"), "first gather should call c_is_match"),
            _ => unreachable!(),
        }
    }
}

#[test]
fn test_checkpoint_config_in_generated_chapel() {
    let toml_str = r#"
    [workload]
    name = "checkpoint-test"
    entry = "src/lib.rs::func"
    partition = "per-item"
    gather = "merge"

    [data]
    input-type = "Vec<Item>"
    output-type = "Vec<Result>"

    [scaling]
    min-nodes = 1
    max-nodes = 4
    grain-size = 10

    [resilience]
    retries = 5
    checkpoint = true
    checkpoint-interval-secs = 60
    "#;

    let m: chapeliser::manifest::Manifest = toml::from_str(toml_str).unwrap();
    let dir = tempfile::tempdir().unwrap();
    chapeliser::codegen::generate_all(&m, dir.path().to_str().unwrap()).unwrap();

    let chpl = fs::read_to_string(
        dir.path().join("chapel/checkpoint_test_distributed.chpl")
    ).unwrap();

    assert!(chpl.contains("maxRetries: int = 5"), "Should set maxRetries to 5");
    assert!(chpl.contains("enableCheckpoint: bool = true"), "Should enable checkpointing");
    assert!(chpl.contains("checkpointIntervalSecs: int = 60"), "Should set checkpoint interval");
    assert!(chpl.contains("c_checkpoint_save"), "Should declare checkpoint save FFI");
}

#[test]
fn test_zig_ffi_exports_all_functions() {
    let m = chapeliser::manifest::load_manifest("examples/panic-attacker/chapeliser.toml").unwrap();
    let dir = tempfile::tempdir().unwrap();
    chapeliser::codegen::generate_all(&m, dir.path().to_str().unwrap()).unwrap();

    let zig = fs::read_to_string(dir.path().join("zig/mass_panic_ffi.zig")).unwrap();

    // All required Chapel-facing exports
    let required_exports = [
        "export fn c_init",
        "export fn c_shutdown",
        "export fn c_get_total_items",
        "export fn c_load_item",
        "export fn c_store_result",
        "export fn c_process_item",
        "export fn c_process_chunk",
        "export fn c_reduce",
        "export fn c_is_match",
        "export fn c_key_hash",
        "export fn c_checkpoint_save",
        "export fn c_checkpoint_load",
    ];

    for export in &required_exports {
        assert!(zig.contains(export), "Zig FFI missing export: {export}");
    }

    // All user-facing externs
    assert!(zig.contains("mass_panic_init"), "Should declare user's init");
    assert!(zig.contains("mass_panic_shutdown"), "Should declare user's shutdown");
    assert!(zig.contains("mass_panic_process_item"), "Should declare user's process_item");
}
