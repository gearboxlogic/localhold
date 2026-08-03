use super::{REVIEWED_DYNAMIC_DESTINATIONS, dispatch_is_opaque};

fn dispatch(path: &str, command: &str, arguments: &[String]) -> bool {
    dispatch_is_opaque(path, true, command, arguments)
}

fn opaque(command: &str, arguments: &[&str]) -> bool {
    dispatch("script/check.sh", command, &arguments.iter().map(|argument| (*argument).to_owned()).collect::<Vec<_>>())
}

#[test]
fn mutation_of_protected_check_inputs_fails_closed() {
    assert!(opaque("cp", &["quality/lint.data", "Justfile"]));
    assert!(opaque("mv", &["quality/lint.data", ".justfile"]));
    assert!(opaque("install", &["quality/lint.data", "script/check.sh"]));
    assert!(opaque("sed", &["-i", "s/check/skip/", "mise.toml"]));
    assert!(opaque("dd", &["if=quality/lint.data", "of=.github/workflows/ci.yml"]));
    assert!(opaque("dd", &["if=quality/lint.data", "of=$GITHUB_WORKSPACE/Justfile"]));
    assert!(opaque("patch", &["Justfile", "quality/lint.patch"]));
    assert!(opaque("patch", &["<", "quality/lint.patch"]));
    assert!(opaque("cp", &["quality/lint.data", "$destination"]));
    assert!(opaque("cp", &["--target-directory", "$destination", "quality/lint.data"]));
    assert!(opaque("cp", &["--target-directory=$destination", "quality/lint.data"]));
    assert!(opaque("cp", &["--t=$destination", "quality/lint.data"]));
    assert!(opaque("cp", &["-t$destination", "quality/lint.data"]));
    assert!(opaque("cp", &["quality/lint.data", "../Justfile"]));
    for destination in ["clippy.toml", "Cargo.toml", "src/lib.rs", "policy/maintainability/structure.json"] {
        assert!(opaque("cp", &["quality/lint.data", destination]), "accepted {destination:?}");
    }
    assert!(opaque("sed", &["-i", "s/warn/allow/", "clippy.toml"]));
    assert!(opaque("link", &["quality/Justfile", "Justfile"]));
    assert!(opaque("ln", &["-sf", "../Justfile", "quality/output"]));
    assert!(opaque("ln", &["--symbolic", "../Justfile", "quality/output"]));
    assert!(opaque("unlink", &["Justfile"]));
    assert!(opaque("unlink", &["$destination"]));
    assert!(!opaque("link", &["--help"]));
    assert!(!opaque("unlink", &["--version"]));
    assert!(!opaque("cp", &["input.txt", "output.txt"]));
    assert!(!opaque("cp", &["-ttarget/output", "input.txt"]));
    assert!(!opaque("cp", &["input.txt", "/tmp/output.txt"]));
    assert!(!opaque("cp", &["input.txt", r"C:\tmp\output.txt"]));
    assert!(opaque("cp", &["input.txt", r"\repo\Justfile"]));
    assert!(!opaque("dd", &["if=quality/lint.data", "of=target/output.txt"]));
    assert!(!opaque("ln", &["input.txt", "target/output.txt"]));
    assert!(opaque("cp", &["$option", "quality/check", "target/missing"]));
    assert!(opaque("dd", &["if=quality/payload", "$output"]));
    assert!(opaque("gzip", &["$flags", "Justfile.gz"]));
}

#[test]
fn removal_of_protected_or_unresolved_inputs_fails_closed() {
    for command in ["del", "del.exe", "erase", "erase.exe", "remove-item", "rm", "rm.exe"] {
        assert!(opaque(command, &["--", "clippy.toml"]), "{command}");
        assert!(opaque(command, &["$target"]), "{command}");
        assert!(!opaque(command, &["-f", "target/report.txt"]), "{command}");
        assert!(!opaque(command, &["/tmp/report.txt"]), "{command}");
        assert!(!opaque(command, &[r"C:\tmp\report.txt"]), "{command}");
        assert!(opaque(command, &[r"\repo\Justfile"]), "{command}");
    }
    assert!(opaque("remove-item", &["-Path:Justfile"]));
    assert!(opaque("remove-item", &["-LiteralPath:$target"]));
    assert!(!opaque("rm", &["--help", "Justfile"]));
    assert!(!opaque("rm.exe", &["--version", "Justfile"]));
}

#[test]
fn reviewed_dynamic_removals_are_path_specific() {
    for (path, command, target) in [
        ("script/check-maintainability-bootstrap.sh", "rm_command", "$snapshot_root"),
        ("script/claude-review.sh", "rm", "$scratch_directory"),
        ("script/run-maintainability-gate.sh", "rm_command", "$target_directory"),
        ("script/tests/test_claude_review.sh", "rm", "$test_root/capture"),
    ] {
        let arguments = ["-rf".to_owned(), "--".to_owned(), target.to_owned()];
        assert!(!dispatch(path, command, &arguments), "{path}: {target}");
        assert!(dispatch("script/check.sh", "rm", &arguments), "{path}: {target}");
    }
    assert!(!dispatch("script/tests/test_maintainability_bootstrap.sh", "rm", &["$test_tool/Cargo.lock".to_owned()],));
    assert!(dispatch(
        "script/tests/test_maintainability_bootstrap.sh",
        "rm",
        &["$authenticated_fixture_path".to_owned()],
    ));
}

#[test]
fn reviewed_dynamic_destinations_are_path_specific() {
    let install = ["-m", "0755", "$build_dir/release/hold", "$bin_dir/hold"].map(str::to_owned);
    assert!(!dispatch("script/install.sh", "install", &install));
    assert!(dispatch("script/check.sh", "install", &install));

    let arbitrary_copy = ["quality/lint.data".to_owned(), "$bin_dir/hold".to_owned()];
    assert!(dispatch("script/install.sh", "cp", &arbitrary_copy));

    assert!(!REVIEWED_DYNAMIC_DESTINATIONS.is_empty());

    let changed = ["quality/lint.data".to_owned(), "$test_root/bin/Justfile".to_owned()];
    assert!(dispatch("script/tests/test_claude_review.sh", "ln", &changed));

    let reviewed = ["-s", "--", "$script_dir/test_claude_review.sh", "$test_root/bin/claude"].map(str::to_owned);
    assert!(!dispatch("script/tests/test_claude_review.sh", "ln", &reviewed));
    assert!(dispatch("script/check.sh", "ln", &reviewed));

    let runner_temp = ["report".to_owned(), ">$RUNNER_TEMP/reports/check.txt".to_owned()];
    assert!(!dispatch(".github/workflows/check.yml", "printf", &runner_temp));
    assert!(dispatch("script/check.sh", "printf", &runner_temp));
}

#[test]
fn sponge_outputs_cannot_replace_execution_surfaces() {
    for arguments in [
        &["Justfile"][..],
        &["-a", "script/check.sh"],
        &["--append", "$destination"],
        &["--", ".github/workflows/ci.yml"],
    ] {
        assert!(opaque("sponge", arguments), "{arguments:?}");
        assert!(opaque("sponge.exe", arguments), "{arguments:?}");
    }
    assert!(!opaque("sponge", &["target/report"]));
    assert!(!opaque("sponge", &[]));
}

#[test]
fn curl_output_to_literal_execution_surfaces_fails_closed() {
    for arguments in [
        &["--silent", "--output", "Justfile", "file:///tmp/payload"][..],
        &["--output=script/check.sh", "file:///tmp/payload"],
        &["-o", ".github/workflows/ci.yml", "file:///tmp/payload"],
        &["-sSomise.toml", "file:///tmp/payload"],
        &["--output", "$GITHUB_WORKSPACE/Justfile", "file:///tmp/payload"],
    ] {
        assert!(opaque("curl", arguments), "{arguments:?}");
        assert!(opaque("curl.exe", arguments), "{arguments:?}");
    }
    assert!(!opaque("curl", &["--output", "target/report.txt", "https://example.invalid/report"]));
    assert!(!opaque("curl", &["--output-dir", "Justfile", "https://example.invalid/report"]));
}

#[test]
fn every_tee_output_destination_fails_closed() {
    for arguments in [
        &["$GITHUB_WORKSPACE/Justfile", "target/report"][..],
        &["target/report", "$destination"],
        &["--append", "target/report", "Justfile"],
        &["--", "-dynamic", "$GITHUB_WORKSPACE/Justfile"],
    ] {
        assert!(opaque("tee", arguments), "{arguments:?}");
        assert!(opaque("tee.exe", arguments), "{arguments:?}");
    }
    assert!(!opaque("tee", &["--append", "target/report", "target/summary"]));
}

#[test]
fn iconv_output_to_execution_surfaces_fails_closed() {
    for arguments in [
        &["-f", "UTF-8", "-t", "UTF-8", "quality/lint.data", "-o", "Justfile"][..],
        &["--output=script/check.sh", "quality/lint.data"],
        &["--out", "$destination", "quality/lint.data"],
        &["-o$destination", "quality/lint.data"],
    ] {
        assert!(opaque("iconv", arguments), "{arguments:?}");
    }
    assert!(!opaque("iconv", &["-o", "target/output.txt", "quality/lint.data"]));
}

#[test]
fn openssl_output_to_execution_surfaces_fails_closed() {
    for arguments in [
        &["base64", "-d", "-in", "quality/Justfile.b64", "-out", "Justfile"][..],
        &["req", "-new", "-keyout", "script/check.sh"],
        &["rand", "-writerand", "$destination"],
        &["ca", "-CAserial", ".github/workflows/ci.yml"],
    ] {
        assert!(opaque("openssl", arguments), "{arguments:?}");
        assert!(opaque("openssl.exe", arguments), "{arguments:?}");
    }
    assert!(!opaque("openssl", &["base64", "-out", "target/output.txt", "-in", "input.txt"]));
}

#[test]
fn shuf_output_to_execution_surfaces_fails_closed() {
    for arguments in [
        &["--output=Justfile", "quality/Justfile"][..],
        &["--output", "$GITHUB_WORKSPACE/Justfile", "quality/Justfile"],
        &["-o", "script/check.sh", "quality/check.sh"],
        &["-o$destination", "quality/Justfile"],
        &["-eoJustfile", "quality/Justfile"],
    ] {
        assert!(opaque("shuf", arguments), "{arguments:?}");
        assert!(opaque("shuf.exe", arguments), "{arguments:?}");
    }
    assert!(!opaque("shuf", &["--output=target/report", "quality/report"]));
}

#[test]
fn curl_remote_names_and_objcopy_outputs_fail_closed() {
    for arguments in [
        &["-O", "file:///tmp/Justfile"][..],
        &["-sSO", "file:///tmp/Justfile"],
        &["--remote-name", "file:///tmp/Justfile"],
        &["--remote-n", "file:///tmp/Justfile"],
        &["-J", "-O", "https://example.invalid/payload"],
    ] {
        assert!(opaque("curl", arguments), "{arguments:?}");
    }
    assert!(!opaque("curl", &["-J", "https://example.invalid/payload"]));

    for command in ["objcopy", "objcopy.exe", "llvm-objcopy", "llvm-objcopy-19", "x86_64-linux-gnu-objcopy"] {
        assert!(opaque(command, &["-I", "binary", "-O", "binary", "quality/Justfile", "Justfile"]), "{command}");
        assert!(opaque(command, &["--dump-section", ".data=Justfile", "target/payload.o"]), "{command}");
        assert!(opaque(command, &["--dump-section=.data=script/check.sh", "target/payload.o"]), "{command}");
        assert!(opaque(command, &["$input", "$output"]), "{command}");
        assert!(!opaque(command, &["input.bin", "target/output.bin"]), "{command}");
        assert!(!opaque(command, &["--dump-section", ".data=target/payload", "target/payload.o"]), "{command}");
    }
}

#[test]
fn strip_outputs_and_in_place_targets_cannot_replace_protected_inputs() {
    for command in ["strip", "strip.exe", "llvm-strip", "llvm-strip-19", "x86_64-linux-gnu-strip"] {
        assert!(opaque(command, &["-o", "script/check-time-abstraction.sh", "target/payload"]), "{command}");
        assert!(opaque(command, &["-o$target", "target/payload"]), "{command}");
        assert!(opaque(command, &["clippy.toml"]), "{command}");
        assert!(opaque(command, &["$input"]), "{command}");
        assert!(!opaque(command, &["-o", "target/stripped", "clippy.toml"]), "{command}");
        assert!(!opaque(command, &["target/payload"]), "{command}");
        assert!(!opaque(command, &["/tmp/payload"]), "{command}");
        assert!(!opaque(command, &[r"C:\tmp\payload"]), "{command}");
        assert!(opaque(command, &[r"\repo\Justfile"]), "{command}");
        assert!(!opaque(command, &["-I", "Justfile", "target/payload"]), "{command}");
    }
    assert!(!opaque("strip", &["--help", "clippy.toml"]));
}

#[test]
fn tool_output_options_cannot_replace_execution_surfaces() {
    for arguments in [
        &[".", "-maxdepth", "0", "-fprintf", "Justfile", "payload"][..],
        &[".", "-fprint", "script/check.sh"],
        &[".", "-fprint0", "$destination"],
        &[".", "-fls", ".github/workflows/ci.yml"],
    ] {
        assert!(opaque("find", arguments), "{arguments:?}");
    }
    assert!(!opaque("find", &[".", "-fprint", "target/files"]));

    for arguments in [
        &["log", "-1", "--output=Justfile"][..],
        &["diff", "--out", "script/check.sh", "HEAD^"],
        &["show", "--output", "$destination", "HEAD"],
    ] {
        assert!(opaque("git", arguments), "{arguments:?}");
    }
    assert!(!opaque("git", &["log", "--output=target/log", "-1"]));

    for arguments in [
        &["--output=Justfile", "quality/lint.data"][..],
        &["--out", "script/check.sh", "quality/lint.data"],
        &["-o", "$destination", "quality/lint.data"],
        &["-ro.github/workflows/ci.yml", "quality/lint.data"],
    ] {
        assert!(opaque("sort", arguments), "{arguments:?}");
    }
    assert!(!opaque("sort", &["-o", "target/sorted", "quality/lint.data"]));

    for command in ["gcc", "gcc-15", "x86_64-linux-gnu-g++-14", "clang", "rustc", "ld.lld-19"] {
        assert!(opaque(command, &["-o", "Justfile", "quality/lint.data"]), "{command}");
        assert!(opaque(command, &["-oscript/check.sh", "quality/lint.data"]), "{command}");
        assert!(opaque(command, &["--output=$destination", "quality/lint.data"]), "{command}");
        assert!(!opaque(command, &["-o", "target/output", "quality/lint.data"]), "{command}");
    }
}

#[test]
fn jar_extraction_and_dynamic_operations_fail_closed() {
    for arguments in [
        &["--extract", "--file", "target/payload.jar"][..],
        &["-xf", "target/payload.jar"],
        &["xvf", "target/payload.jar"],
        &["$operation", "target/payload.jar"],
        &["--list", "$operation"],
    ] {
        assert!(opaque("jar", arguments), "{arguments:?}");
        assert!(opaque("jar.exe", arguments), "{arguments:?}");
    }
    assert!(!opaque("jar", &["--list", "--file", "$archive"]));
    assert!(!opaque("jar", &["tf", "$archive"]));
}

#[test]
fn in_place_decompression_fails_closed() {
    for (command, arguments) in [
        ("gzip", &["-dkf", "Justfile.gz"][..]),
        ("gzip.exe", &["--decompress", "--force", "Justfile.gz"]),
        ("gunzip", &["Justfile.gz"]),
        ("bzip2", &["-d", "Justfile.bz2"]),
        ("unxz", &["Justfile.xz"]),
        ("zstd", &["--decompress", "Justfile.zst"]),
        ("unlz4.exe", &["Justfile.lz4"]),
        ("brotli", &["-d", "Justfile.br"]),
    ] {
        assert!(opaque(command, arguments), "{command}: {arguments:?}");
    }
    for (command, arguments) in [
        ("gzip", &["-dc", "Justfile.gz"][..]),
        ("gunzip", &["--stdout", "Justfile.gz"]),
        ("gzip", &["--list", "Justfile.gz"]),
        ("xz", &["--test", "Justfile.xz"]),
        ("zstd", &["--decompress", "--to-stdout", "Justfile.zst"]),
    ] {
        assert!(!opaque(command, arguments), "{command}: {arguments:?}");
    }
}

#[test]
fn output_redirection_to_literal_execution_surfaces_fails_closed() {
    assert!(opaque("cat", &["quality/lint.data", ">", "Justfile"]));
    assert!(opaque("printf", &["replacement", ">script/check.sh"]));
    assert!(opaque("printf", &["replacement", "2>>", "quality/check.py"]));
    assert!(opaque("cat", &["quality/Justfile", ">", "$GITHUB_WORKSPACE/Justfile"]));
    assert!(!opaque("printf", &["error", ">"]));
    assert!(!opaque("printf", &["report", ">", "target/report.txt"]));
}
