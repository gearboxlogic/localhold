use super::has_opaque_write;

#[test]
fn filesystem_copies_to_execution_surfaces_fail_closed() {
    for source in [
        r#"shutil.copyfile("quality/lint.data", "Justfile")"#,
        r#"shutil.copy2("quality/lint.data", "script/check.sh")"#,
        r#"shutil.copytree("quality/data", dst=".github/actions/check/action.yml")"#,
        r#"os.replace("quality/lint.data", r".cargo\config.toml")"#,
        r#"shutil.move("quality/lint.data", "script/check=lint.sh")"#,
    ] {
        assert!(has_opaque_write(source), "{source}");
    }
}

#[test]
fn direct_filesystem_writers_fail_closed() {
    for source in [
        r#"Path("Justfile").write_text(Path("quality/Justfile").read_text())"#,
        r#"pathlib.Path("script/check.sh").write_bytes(payload)"#,
        r#"Path("Justfile").open("w")"#,
        r#"open("Justfile", "w")"#,
        r#"open(mode="a", file=".github/workflows/ci.yml")"#,
        r#"io.open("mise.toml", mode)"#,
        r#"Path("Justfile").unlink()"#,
        r#"Path("quality/lint.data").replace("Justfile")"#,
        r#"Path.write_text(Path("Justfile"), payload)"#,
        r#"pathlib.Path.open(Path("Justfile"), mode="w")"#,
        r#"Path("quality/../Justfile").write_text(payload)"#,
        r#"Path("$destination").write_text(payload)"#,
        r#"Path("/workspace/Justfile").write_text(payload)"#,
        r"os.write(descriptor, payload)",
        r#"os.open("Justfile", os.O_WRONLY | os.O_TRUNC)"#,
        r#"os.fdopen(descriptor, "w")"#,
        r#"os.symlink("../Justfile", "quality/output")"#,
        "target = Path(\"Justfile\")\ntarget.write_text(payload)\n",
        "target = Path(\"Justfile\")\ntarget.open(\"wb\")\n",
        "target = Path(\"Justfile\")\ntarget.unlink()\n",
        "root = Path(\".\")\n(root / \"Justfile\").write_bytes(payload)\n",
        "root = Path(\".\")\n(root / \"Justfile\").open(\"wb\")\n",
        "shutil.copy2(source, destination)\n",
        "os.replace(source, destination)\n",
        "message = f\"{Path('Justfile').write_text(payload)}\"\n",
        "message = f\"{open('Justfile', 'w').write(payload)}\"\n",
        "message = f\"{value:{Path('Justfile').write_text(payload)}}\"\n",
        "message = f\"{value:{{Path('Justfile').write_text(payload)}}}\"\n",
        r#"message = f"{Path("Justfile").write_text(payload)}"\n"#,
        r#"(Path("Justfile").write_text)(payload)"#,
        r#"(open)("Justfile", "w").write(payload)"#,
        r#"message = f"{(Path('Justfile').write_text)(payload)}""#,
        r#"message = f"{(open)('Justfile', 'w').write(payload)}""#,
        "writer = Path('Justfile').write_text\nwriter(payload)\n",
        "writer = open\nwriter('Justfile', 'w').write(payload)\n",
        "writer = (builtins.open)\nwriter(file='Justfile', mode='a')\n",
        "writer = open; writer('Justfile', 'w')\n",
        "safe(); writer = open\n",
        "writer = (\n    open\n)\n",
        "writer = Path('Justfile').write_text\nrunner = writer\nrunner(payload)\n",
        "writer = open\ncontainer = [writer]\n",
        "(writer := open)\n",
        "(writer := Path('Justfile').write_bytes)\n",
    ] {
        assert!(has_opaque_write(source), "{source}");
    }
}

#[test]
fn filesystem_copies_to_data_paths_remain_allowed() {
    assert!(!has_opaque_write(r#"shutil.copyfile("quality/report.txt", "target/report.txt")"#));
    assert!(!has_opaque_write(r#"print('shutil.copyfile("a", "Justfile")')"#));
    assert!(!has_opaque_write(r#"Path("target/report.txt").write_text(report)"#));
    assert!(!has_opaque_write(r#"open("target/report.txt", "wb")"#));
    assert!(!has_opaque_write(r#"(open)("target/report.txt", "wb")"#));
    assert!(!has_opaque_write(r#"(Path("target/report.txt").write_text)(report)"#));
    assert!(!has_opaque_write("writer = Path('target/report.txt').write_text\nwriter(report)\n"));
    assert!(!has_opaque_write("writer = Path('target/report.txt').write_text\nmessage = f'{writer(report)}'\n"));
    assert!(!has_opaque_write("writer = formatter\nwriter('Justfile', 'w')\n"));
    assert!(!has_opaque_write("writer = open_writer\nwriter('Justfile', 'w')\n"));
    assert!(!has_opaque_write("writer = Path('target/report.txt').write_bytes\ncontainer = [writer]\n"));
    assert!(!has_opaque_write("message = 'writer = open; writer(Justfile, w)'\n"));
    assert!(!has_opaque_write("print(\"safe; writer = open\")\n"));
    assert!(!has_opaque_write("print(\"safe # writer = open\")\n"));
    assert!(!has_opaque_write("print('safe')  # writer = open; writer('Justfile', 'w')\n"));
    assert!(!has_opaque_write("message = \"\"\"safe; writer = open\n# writer = builtins.open\"\"\"\n"));
    assert!(!has_opaque_write("writer = (\n    Path('target/report.txt').write_text\n)\ncontainer = [writer]\n"));
    assert!(!has_opaque_write(r#"open("Justfile", "rb")"#));
    assert!(!has_opaque_write(r#"Path("Justfile").open("r")"#));
    assert!(!has_opaque_write(r#"(Path(".") / "Justfile").open("rb")"#));
    assert!(!has_opaque_write(r#"os.open("Justfile", os.O_RDONLY)"#));
    assert!(!has_opaque_write(r#"os.open("target/report.txt", os.O_WRONLY)"#));
    assert!(!has_opaque_write("\"\"\"\nPath('Justfile').write_text(payload)\n\"\"\"\nprint('safe')\n"));
    assert!(!has_opaque_write("# Path('Justfile').write_text(payload)\nprint('safe')\n"));
    assert!(!has_opaque_write("f\"Path('Justfile').write_text(payload) is inert text\"\n"));
    assert!(!has_opaque_write("f\"{label + 'Path(Justfile).write_text(payload)'}\"\n"));
    assert!(!has_opaque_write("f\"{value:─^9}\"\n"));
    assert!(!has_opaque_write("f\"{value:Path('Justfile').write_text(payload)}\"\n"));
    assert!(!has_opaque_write(r#"f"{len("safe")}"\n"#));
    assert!(has_opaque_write("# setup\nPath('Justfile').write_text(payload)\n"));
    assert!(has_opaque_write(r#"open(os.path.join(output_dir, filename), "w")"#));
    assert!(has_opaque_write(r#"open(os.path.join(output_dir, "report.svg"), "w")"#));
    assert!(has_opaque_write(r#"open(os.path.join(output_dir, "Justfile"), "w")"#));
    assert!(has_opaque_write(r#"open(prefix or os.path.join(output_dir, "report.svg"), "w")"#));
}
