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
    assert!(chpl.contains("coforall"));
    assert!(chpl.contains("mass_panic"));
    assert!(chpl.contains("c_process_item"));

    // Check Zig file contains expected exports
    let zig = fs::read_to_string(output.join("zig/mass_panic_ffi.zig")).unwrap();
    assert!(zig.contains("export fn c_process_item"));
    assert!(zig.contains("mass_panic_process_item"));

    // Check C header contains guards and function declarations
    let h = fs::read_to_string(output.join("include/mass_panic_chapeliser.h")).unwrap();
    assert!(h.contains("#ifndef CHAPELISER_MASS_PANIC_H"));
    assert!(h.contains("mass_panic_process_item"));
    assert!(h.contains("mass_panic_serialize"));
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
    }
}
