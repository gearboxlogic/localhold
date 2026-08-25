use super::*;

#[test]
fn command_policy_rejects_python_command_wrapper_dispatch() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    fs::write(
        workspace.path().join("script/check.py"),
        "import subprocess\nsubprocess.run([\"env\", \"sh\", \"-c\", bytes.fromhex(\"636172676f20636c69707079202d2d202d41207761726e696e6773\").decode()])\n",
    )
    .expect("Python command-wrapper argv call");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("opaque interpreter program"), "{error:#}");

    fs::write(
        workspace.path().join("script/check.py"),
        "import shutil\nshutil.copyfile(\"quality/lint.data\", \"Justfile\")\n",
    )
    .expect("Python execution-surface mutation");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("lint-weakening argument"), "{error:#}");

    fs::write(
        workspace.path().join("script/check.py"),
        "import subprocess\nsubprocess.run([\"git\", \"status\"])\nrunner = subprocess.run\nrunner(bytes.fromhex(\"636172676f\").decode(), shell=True)\n",
    )
    .expect("assigned Python process callable");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("opaque interpreter program"), "{error:#}");

    fs::write(
        workspace.path().join("script/check.py"),
        "import os\nos.__dict__[\"sy\" + \"stem\"](bytes.fromhex(\"7368207175616c6974792f6c696e742e747874\").decode())\n",
    )
    .expect("mapping-based Python process lookup");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("opaque interpreter program"), "{error:#}");

    fs::write(
        workspace.path().join("script/check.py"),
        "exec(bytes.fromhex(\"696d706f7274206f733b206f732e73797374656d2827636172676f20636c69707079202d2d202d41207761726e696e67732729\"))\n",
    )
    .expect("Python dynamic code evaluation");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("opaque interpreter program"), "{error:#}");

    fs::write(
        workspace.path().join("script/check.py"),
        "__import__(\"os\").system(bytes.fromhex(\"636172676f20636c69707079202d2d202d41207761726e696e6773\").decode())\n",
    )
    .expect("Python dynamic import");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("opaque interpreter program"), "{error:#}");

    fs::write(workspace.path().join("script/check.py"), "import runpy\nrunpy.run_path('quality/lint.txt')\n").expect("Python runpy execution");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("opaque interpreter program"), "{error:#}");

    fs::write(
        workspace.path().join("script/check.py"),
        "import io, pickle\npickle.Unpickler(io.BytesIO(bytes.fromhex(payload))).load()\n",
    )
    .expect("Python Unpickler execution");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("opaque interpreter program"), "{error:#}");
}

#[test]
fn command_policy_applies_direct_dispatch_rules_to_python_argv() {
    assert_opaque_python_process_bindings(&[
        "import subprocess\nsubprocess.run(['awk', 'BEGIN { system(\"sh quality/lint.txt\") }'])\n",
        "import subprocess\nsubprocess.run(['find', '.', '-exec', 'sh', 'quality/lint.txt', ';'])\n",
        "import subprocess\nsubprocess.run(['git', '-c', 'alias.lint=!sh quality/lint.txt', 'lint'])\n",
        "import subprocess\nsubprocess.run(['git', 'grep', '--open-files-in-pager=quality/lint', 'lint'])\n",
        "import subprocess\nsubprocess.run(['cargo', 'run', '--manifest-path', 'quality/helper/Cargo.toml'])\n",
        "import subprocess\nsubprocess.run([\"cargo\", \"--config\", 'target.x86_64-unknown-linux-gnu.runner=[\"sh\",\"-c\",\"touch Justfile\"]', \"run\", \"--manifest-path\", \"tools/maintainability/Cargo.toml\"])\n",
        "import subprocess\nsubprocess.run(['cargo', '+nightly', '-Z', 'unstable-options', '-C', 'quality', 'build'])\n",
        "import subprocess\nsubprocess.run(['cargo', 'build', '--target', 'quality/host.json'])\n",
        "import subprocess\nsubprocess.run(['rustc', '--target=quality/host.json', '-', '-o', 'target/output'], input=source, text=True)\n",
        "import subprocess\nsubprocess.run(['rustdoc', '--target', r'quality\\host.JSON', '-', '-o', 'target/output'], input=source, text=True)\n",
        "import subprocess\nsubprocess.run(['rustc', 'quality/benign.rs', '-C', 'link-arg=-Wl,--plugin=quality/payload'])\n",
        "import subprocess\nsubprocess.run(['rustc', 'quality/benign.rs', '-C', 'link_arg=-Wl,--plugin=quality/payload'])\n",
        "import subprocess\nsubprocess.run(['rustc', '@quality/rustc.args'])\n",
        "import subprocess\nsubprocess.run(['rustc', '-C', '@quality/codegen.args'])\n",
        "import subprocess\nsubprocess.run(['gcc', '-fplugin=quality/lint.so', '-c', 'quality/input.c'])\n",
        "import subprocess\nsubprocess.run(['gcc', '-specs', 'quality/lint.specs', '-c', 'quality/input.c'])\n",
        "import subprocess\nsubprocess.run(['ssh-keygen', '-D', 'quality/lint.so'])\n",
        "import subprocess\nsubprocess.run(['ld.so', 'quality/lint'])\n",
        "import subprocess\nsubprocess.run(['tar', '--to-command=quality/lint', '-xf', 'payload.tar', '-C', 'extracted'])\n",
        "import subprocess\nsubprocess.run(['sed', '/foo;bar/e sh quality/hidden.txt', '/etc/hosts'], check=True)\n",
        "import subprocess\nsubprocess.run(['sed', '/foo/Ie sh quality/hidden.txt', '/etc/hosts'], check=True)\n",
        "import subprocess\nsubprocess.run(['sort', '--compress-program=quality/lint', 'input'])\n",
        "import subprocess\nsubprocess.run(['rg', '--pre', 'quality/lint', 'pattern', '.'])\n",
        "import subprocess\nsubprocess.run(['just', '--justfile', 'quality/lint.data', 'check-quality'])\n",
        "import subprocess\nsubprocess.run(['unknown-runner', '--eval', 'quality/lint'])\n",
        "import subprocess\nsubprocess.run(['tools/mv', 'quality/lint.data', 'script/check.sh'])\n",
        "import subprocess\nsubprocess.run(['/usr/bin/awk', 'BEGIN { system(\"true\") }'])\n",
        "import subprocess\nsubprocess.run(['AWK.EXE', 'program'])\n",
        "import subprocess\nsubprocess.run(['GIT.EXE', '-c', 'alias.lint=!quality/lint', 'lint'])\n",
        "import subprocess\nsubprocess.run(['SSH-KEYGEN.EXE', '-D', 'quality/lint.dll'])\n",
        "import subprocess\nsubprocess.run(['git', 'show', reference])\n",
        "import subprocess\nsubprocess.run(['gcc', compiler_argument])\n",
        "import subprocess\nsubprocess.run(['ssh-keygen', provider_option])\n",
    ]);
}

#[test]
fn command_policy_treats_python_argv_as_typed_values() {
    assert_opaque_python_process_bindings(&[
        "import subprocess\nsubprocess.run(['<', 'quality/lint'])\n",
        "import subprocess\nsubprocess.run(['if', 'quality/lint'])\n",
        "import subprocess\nsubprocess.run(['(', 'quality/lint'])\n",
        "import subprocess\nsubprocess.run(['-runner', 'quality/lint'])\n",
        "import subprocess\nsubprocess.run(['cd', 'quality'])\n",
        "import subprocess\nsubprocess.run(['compgen', 'quality'])\n",
        "import subprocess\nsubprocess.run(['mapfile', 'quality'])\n",
        "import subprocess\nsubprocess.run(['readarray', 'quality'])\n",
        "import subprocess\nsubprocess.run(['source', 'quality/lint'])\n",
        "import subprocess\nsubprocess.run(['trap'])\n",
        "import subprocess\nsubprocess.run(['git', 'grep', option])\n",
        "import subprocess\nsubprocess.run(['wc', path])\n",
        "import subprocess\nsubprocess.run(['quality/helper', option])\n",
        "import subprocess\nsubprocess.run(['mv', 'target/input.txt', 'target/output.txt'])\n",
    ]);
}

#[test]
fn command_policy_allows_literal_python_argv_metacharacters() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    fs::create_dir_all(workspace.path().join("quality")).expect("quality directory");
    fs::write(workspace.path().join("quality/$helper"), "#!/bin/sh\ntrue\n").expect("repository executable");
    for program in ["if", "-runner", "cd", "mapfile", "source", "trap"] {
        fs::write(workspace.path().join("quality").join(program), "#!/bin/sh\ntrue\n").expect("repository executable");
    }
    git(workspace.path(), &["init", "-q"]);
    for source in [
        "import subprocess\nsubprocess.run(['rg', '$*?[{~', '.'])\n",
        "import subprocess\nsubprocess.run(['git', 'grep', '-e', '$*?[{~', '--', '.'])\n",
        "import subprocess\nsubprocess.run(['gcc', '-c', 'quality/$input.c', '-o', 'target/$output.o'])\n",
        "import subprocess\nsubprocess.run(['cp', 'quality/$report.txt', 'target/$report.txt'])\n",
        "import subprocess\nsubprocess.run(['wc', 'quality/$report.txt'])\n",
        "import subprocess\nsubprocess.run(['quality/$helper', '$*?[{~'])\n",
        "import subprocess\nsubprocess.run(['quality/if', '--check'])\n",
        "import subprocess\nsubprocess.run(['quality/-runner', '--check'])\n",
        "import subprocess\nsubprocess.run(['quality/cd', '--check'])\n",
        "import subprocess\nsubprocess.run(['quality/mapfile', '--check'])\n",
        "import subprocess\nsubprocess.run(['quality/source', '--check'])\n",
        "import subprocess\nsubprocess.run(['quality/trap', '--check'])\n",
        "import subprocess\nsubprocess.run(['cargo', 'metadata', '--target', 'x86_64-unknown-linux-gnu'])\n",
        "import subprocess\nsubprocess.run(['cargo', 'deny', 'check', '--config', 'deny.toml'])\n",
        "import subprocess\nsubprocess.run(['rustc', '--target', 'x86_64-unknown-linux-gnu', '-', '-o', 'target/output'], input='fn main() {}', text=True)\n",
    ] {
        fs::write(workspace.path().join("script/check.py"), source).expect("typed Python argv");
        git(workspace.path(), &["add", "."]);
        assert!(reject_checked_in_weakening(workspace.path()).is_ok(), "{source}");
    }
}

#[test]
fn command_policy_rejects_python_filesystem_writes() {
    assert_opaque_python_filesystem_writes(&[
        "from pathlib import Path\nPath(\"Justfile\").write_text(Path(\"quality/Justfile\").read_text())\n",
        "from pathlib import Path\ntarget = Path(\"Justfile\")\ntarget.write_text(Path(\"quality/Justfile\").read_text())\n",
        "import shutil\nsource = \"quality/Justfile\"\ntarget = \"Justfile\"\nshutil.copy2(source, target)\n",
        "with open(file=\"Justfile\", mode=\"w\") as output:\n    output.write(\"lint:\\n    true\\n\")\n",
        "import os\nos.write(descriptor, payload)\n",
        "message = f\"{Path('Justfile').write_text(payload)}\"\n",
        "message = f\"{open('Justfile', 'w').write(payload)}\"\n",
        "(Path('Justfile').write_text)(payload)\n",
        "(open)('Justfile', 'w').write(payload)\n",
        "message = f\"{(Path('Justfile').write_text)(payload)}\"\n",
        "message = f\"{(open)('Justfile', 'w').write(payload)}\"\n",
        "Path('Justfile').write_text.__call__(payload)\n",
        "open.__call__('Justfile', 'w').write(payload)\n",
        "writer = Path('Justfile').write_text\nwriter(payload)\n",
        "writer = open\nwriter('Justfile', 'w').write(payload)\n",
        "writer = Path('Justfile').write_text\nrunner = writer\nrunner(payload)\n",
        "writer = open\ncontainer = [writer]\n",
        "(writer := open)\n",
        "writer: Callable[[str], int] = open\n",
        "first = second = open\n",
        "holder.writer = open\n",
        "def invoke(opener=open):\n    return opener\n",
        "container = [open]\nopener = container[0]\nopener('target/report.txt', 'w')\n",
        "[opener] = [open]\nopener('target/report.txt', 'w')\n",
        "opener = [open][0]\nopener('target/report.txt', 'w')\n",
        "from functools import partial\nopener = partial(open, 'target/report.txt', 'w')\n",
        "from functools import partial\nwriter = partial(Path('Justfile').write_text, encoding='utf-8')\n",
        "import os\nremover = os.unlink\nremover('target/report.txt')\n",
        "import shutil\nmover = shutil.move\nmover('target/input.txt', 'target/output.txt')\n",
        "from pathlib import Path\nwriter = Path('Justfile').unlink\nwriter()\n",
        "from pathlib import Path\nwriter = Path.rename\nwriter(Path('target/input.txt'), 'target/output.txt')\n",
        "import tempfile\ntempfile.NamedTemporaryFile(dir='script')\n",
        "import tempfile\ntempfile.NamedTemporaryFile(dir='.github/workflows', suffix='.yml')\n",
        "import tempfile\ntempfile.NamedTemporaryFile(dir=destination)\n",
        "import tempfile\nfactory = tempfile.NamedTemporaryFile\nfactory(dir='target', suffix='.txt')\n",
        "import shutil as files\nfiles.copyfile('quality/Justfile', 'Justfile')\n",
        "from shutil import copyfile as copy\ncopy('quality/Justfile', 'Justfile')\n",
        "import urllib.request\nurllib.request.urlretrieve(source, 'Justfile')\n",
        "import tempfile as scratch\nscratch.NamedTemporaryFile(dir='script', suffix='.sh')\n",
        "from os import remove as erase\nerase('Justfile')\n",
        "import tempfile\ntempfile.NamedTemporaryFile(dir='target', prefix='../script/check-', suffix='.sh', delete=False)\n",
        "open('Just' 'file', 'w').write(payload)\n",
        "from pathlib import Path\nPath('Just' + 'file').write_text(payload)\n",
        "import shutil as files\nmessage = f\"{files.copyfile('quality/Justfile', 'Justfile')}\"\n",
        "from shutil import copyfile as copy\nmessage = f\"{copy('quality/Justfile', 'Justfile')!s}\"\n",
        "import tempfile as scratch\nmessage = f\"{scratch.NamedTemporaryFile(dir='script', suffix='.sh')}\"\n",
        "import _io as streams\nstreams.open('Justfile', 'w').write(payload)\n",
        "from _io import open as writer\nwriter('Justfile', 'w').write(payload)\n",
        "import _pyio as streams\nstreams.open('Justfile', 'w').write(payload)\n",
        "from _pyio import open as writer\nwriter('Justfile', 'w').write(payload)\n",
        "from posix import remove as erase\nerase('Justfile')\n",
        "import nt as backend\nbackend.remove('Justfile')\n",
        "open('MAKEFILE', 'w').write(payload)\n",
        "open('PACKAGE.JSON', 'w').write(payload)\n",
        "open('.GITHUB/workflows/ci.yml', 'w').write(payload)\n",
        "open('Justfile.', 'w').write(payload)\n",
        "open('Justfile ', 'w').write(payload)\n",
        "open('Justfile:$DATA', 'w').write(payload)\n",
        "import tempfile\ntempfile.NamedTemporaryFile(dir='.GITHUB/workflows', suffix='.yml', delete=False)\n",
    ]);
}

#[test]
fn command_policy_rejects_python_filesystem_reflection() {
    assert_opaque_python_process_bindings(&[
        "from pathlib import Path\ngetattr(Path('target/input'), 'co' + 'py')('Justfile')\n",
        "from pathlib import Path\noperation = f\"write_{kind}\"\ngetattr(Path('Justfile'), operation)(payload)\n",
        "from pathlib import Path\ntarget = Path('Justfile')\nwriter = target.__getattribute__('write_text')\nwriter(payload)\n",
        "from pathlib import Path\nPath('Justfile').__getattribute__('unlink')()\n",
        "from pathlib import Path\n(Path('Justfile')).__getattribute__('unlink')()\n",
        "from pathlib import Path\n(Path).__dict__['copy'](Path('target/input'), 'Justfile')\n",
        "from pathlib import Path\nPath.__dict__['copy'](Path('target/input'), 'Justfile')\n",
        "from pathlib import Path\nPath.__dict__.get('write_text')(Path('Justfile'), payload)\n",
        "from pathlib import Path\nPath.__mro__[0].__dict__.__getitem__('unlink')(Path('Justfile'))\n",
        "from pathlib import Path\nvars(Path)['move'](Path('target/input'), 'Justfile')\n",
        "import os\ngetattr(os, 'remove')('Justfile')\n",
        "import builtins\nfrom pathlib import Path\nbuiltins.getattr(Path('Justfile'), 'write_text')(payload)\n",
        "import shutil\nfiles = shutil\ncopy = vars(files)['copyfile']\ncopy('target/input', 'Justfile')\n",
        "import shutil\nfirst = files = shutil\nfiles.__dict__.get('copyfile')('target/input', 'Justfile')\n",
        "from pathlib import Path\nfirst = target = Path('Justfile')\ngetattr(target, 'write_text')(payload)\n",
        "from pathlib import Path\nholders = [Path('Justfile')]\ntarget = holders[0]\nvars(target)['write_text'](payload)\n",
        "from pathlib import Path\nfactories = {'path': Path}\nfactory = factories['path']\nfactory.__dict__.get('move')(Path('target/input'), 'Justfile')\n",
        "import pathlib as paths\nfactory = paths.Path\ntarget = factory('Justfile')\ngetattr(target, operation)(payload)\n",
        "import pathlib as paths\nmessage = f\"{getattr(paths.Path('Justfile'), operation)(payload)}\"\n",
        "from pathlib import Path\n[target] = [Path('Justfile')]\ngetattr(target, 'write_text')(payload)\n",
        "from pathlib import Path\n(target := Path('Justfile'))\nvars(target)['unlink']()\n",
        "from pathlib import Path\nitems = []\nitems.append(Path)\nfactory = items.pop()\ngetattr(factory('Justfile'), 'write_text')(payload)\n",
        "from pathlib import Path\nholders = {'path': Path}\nfactory = holders.get('path')\nvars(factory)['copy'](Path('input'), 'Justfile')\n",
        "from pathlib import Path\nwriter = (\n    Path.__mro__[0]\n).__dict__.get('write_text')\nwriter(Path('Justfile'), payload)\n",
        "from pathlib import Path\nvalues = [Path('target/report.txt').exists()]\nvalue = values[0]\ngetattr(value, 'bit_length')()\n",
        "import operator\nfrom pathlib import Path\nwriter = operator.attrgetter('write_text')(Path)\nwriter(Path('Justfile'), payload)\n",
        "import operator\nfrom pathlib import Path\noperator.methodcaller('unlink')(Path('Justfile'))\n",
        "import operator as operations\nfrom pathlib import Path\n(operations\n    # continued lookup\n    .methodcaller)('unlink')(Path('Justfile'))\n",
        "import inspect\nfrom pathlib import Path\ninspect.getattr_static(Path, 'write_text')(Path('Justfile'), payload)\n",
        "import inspect, os\ndict(inspect.getmembers_static(os))['remove']('Justfile')\n",
        "from operator import methodcaller as invoke\nfrom pathlib import Path\ninvoke('write_text', payload)(Path('Justfile'))\n",
        "from inspect import getmembers as fields\nimport shutil\ndict(fields(shutil))['copyfile']('target/input', 'Justfile')\n",
        "import _operator\nfrom pathlib import Path\nwriter = _operator.attrgetter('write_text')(Path)\nwriter(Path('Justfile'), payload)\n",
        "import _operator as operations\nfrom pathlib import Path\noperations.methodcaller('unlink')(Path('Justfile'))\n",
        "from _operator import attrgetter as field\nfrom pathlib import Path\nfield('write_text')(Path)(Path('Justfile'), payload)\n",
        "from _operator import methodcaller as invoke\nimport os\ninvoke('remove', 'Justfile')(os)\n",
    ]);
}

#[test]
fn command_policy_rejects_python_filesystem_reflection_reexports() {
    assert_opaque_python_filesystem_writes(&[
        "import dataclasses\nfrom pathlib import Path\nwriter = dataclasses.inspect.getattr_static(Path, 'write_text')\nwriter(Path('Justfile'), payload)\n",
        "import dataclasses as records\nfrom pathlib import Path\nrecords.inspect.getattr_static(Path, 'unlink')(Path('Justfile'))\n",
        "from dataclasses import inspect as introspection\nfrom pathlib import Path\nwriter = introspection.getattr_static(Path, 'write_text')\nwriter(Path('Justfile'), payload)\n",
        "import dataclasses\nfrom pathlib import Path\nattributes = dataclasses.inspect.classify_class_attrs(Path)\nwriter = next(item.object for item in attributes if item.name == 'write_text')\nwriter(Path('Justfile'), payload)\n",
        "import dataclasses as records\nfrom pathlib import Path\nnext(item.object for item in records.inspect.classify_class_attrs(Path) if item.name == 'unlink')(Path('Justfile'))\n",
        "from pathlib import Path\nvalue = helpers.getattr_static(record, 'label')\nPath('target/report.txt').read_text()\n",
    ]);
}

#[test]
fn command_policy_rejects_python_argv_filesystem_mutations() {
    assert_opaque_python_process_bindings(&[
        "import subprocess\nsubprocess.run(['cp', 'quality/lint.data', 'Justfile'], check=True)\n",
        "import subprocess\nsubprocess.run(['mv', 'quality/lint.data', 'script/check.sh'], check=True)\n",
        "import subprocess\nsubprocess.run(['ln', '-s', 'script', 'docs-link'], check=True)\n",
        "import subprocess\nsubprocess.run(['cp', source, destination], check=True)\n",
        "import subprocess\nsubprocess.run(['ln', '-s', 'script', 'docs-link'], check=True)\nopen('docs-link/check.sh', 'w').write(payload)\n",
        "import subprocess\nsubprocess.run(['gzip', '--suffix', '--help', '-d', 'Justfile--help'], check=True)\n",
        "import subprocess\nsubprocess.run(['lz4', '-D', '--help', '-d', 'Justfile.lz4', 'Justfile'], check=True)\n",
        "import subprocess\nsubprocess.run(['unzip', '-oq', 'payload.zip', '-d', '-l'], check=True)\n",
        "import subprocess\nsubprocess.run(['unzip', '-P', '--help', 'payload.zip'], check=True)\n",
        "import subprocess\nsubprocess.run(['unzip', '-P', '-l', 'payload.zip'], check=True)\n",
        "import subprocess\nsubprocess.run(['tee', '--', '--help', 'Justfile'], check=True)\n",
    ]);
}

#[test]
fn command_policy_allows_non_filesystem_reflection_reexports() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    git(workspace.path(), &["init", "-q"]);
    for source in [
        "import dataclasses\nattributes = dataclasses.inspect.classify_class_attrs(str)\n",
        "import dataclasses as records\nvalue = records.inspect.getattr_static(str, 'strip')\n",
        "from dataclasses import inspect as introspection\nvalue = introspection.getattr_static(str, 'strip')\n",
    ] {
        fs::write(workspace.path().join("script/check.py"), source).expect("non-filesystem reflection re-export");
        git(workspace.path(), &["add", "."]);
        assert!(reject_checked_in_weakening(workspace.path()).is_ok(), "{source}");
    }
}

#[test]
fn command_policy_allows_inert_python_writer_binding_text() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    git(workspace.path(), &["init", "-q"]);
    for source in [
        "print(\"safe; writer = open\")\n",
        "print(\"safe # writer = open\")\n",
        "print('safe')  # writer = open; writer('Justfile', 'w')\n",
        "writer = (\n    Path('target/report.txt').write_text\n)\ncontainer = [writer]\n",
        "writer: Callable[[str], int] = Path('target/report.txt').write_text\nwriter('report')\n",
        "container = [Path('target/report.txt').unlink]\ncontainer[0]()\n",
        "Path('target/report.txt').write_text.__call__('report')\n",
        "open.__call__('target/report.txt', 'w').write('report')\n",
        "import tempfile\ntempfile.NamedTemporaryFile(dir='target', suffix='.txt')\n",
        "import shutil as files\nfiles.copyfile('quality/report.txt', 'target/report.txt')\n",
        "from shutil import copyfile as copy\ncopy('quality/report.txt', 'target/report.txt')\n",
        "from os import remove as erase\nerase('target/report.txt')\n",
        "import tempfile as scratch\nscratch.NamedTemporaryFile(dir='target', suffix='.txt')\n",
        "open('target/' 'report.txt', 'w').write('report')\n",
        "import shutil as files\nmessage = f\"{files.copyfile('quality/report.txt', 'target/report.txt')}\"\n",
        "from shutil import copyfile as copy\nmessage = f\"{copy('quality/report.txt', 'target/report.txt')!s}\"\n",
        "import tempfile as scratch\nmessage = f\"{scratch.NamedTemporaryFile(dir='target', suffix='.txt')}\"\n",
        "from _io import open as writer\nmessage = f\"{writer('target/report.txt', 'w')}\"\n",
        "from _pyio import open as writer\nmessage = f\"{writer('target/report.txt', 'w')}\"\n",
        "from posix import remove as erase\nmessage = f\"{erase('target/report.txt')}\"\n",
        "from pathlib import Path\nprint('operator.attrgetter inspect.getattr_static')\nvalue = Path('target/report.txt').read_text()\n",
        "from pathlib import Path\n# operator.methodcaller('unlink')(Path('Justfile'))\nvalue = Path('target/report.txt').read_text()\n",
        "from pathlib import Path\nprint('_operator.attrgetter')\n# _operator.methodcaller('unlink')(Path('Justfile'))\nvalue = Path('target/report.txt').read_text()\n",
        "from pathlib import Path\nprint('dataclasses.inspect.getattr_static classify_class_attrs')\n# helpers.classify_class_attrs(Path)\nvalue = Path('target/report.txt').read_text()\n",
        "import subprocess\nsubprocess.run(['cp', 'quality/report.txt', 'target/report.txt'], check=True)\n",
        "import subprocess\nsubprocess.run(['chmod', '600', 'target/input.txt'], check=True)\n",
        "import subprocess\nsubprocess.run(['rg', 'pattern', '.'], check=True)\n",
        "import subprocess\nsubprocess.run(['printf', '%s', '>Justfile'], check=True)\n",
        "import subprocess\nsubprocess.run(['git', 'rev-parse', '--verify', 'HEAD'], check=True)\n",
        "import subprocess\nsubprocess.run(['git', 'grep', '-e', '$*?[{~', '--', '.'], check=False)\n",
        "import subprocess\nsubprocess.run(['cargo', 'metadata', '--no-deps'], check=True)\n",
        "import subprocess\nsubprocess.run(['gcc', '-c', 'quality/input.c', '-o', 'target/input.o'], check=True)\n",
        "import subprocess\nsubprocess.run(['tar', '-cf', 'target/archive.tar', 'data'], check=True)\n",
        "import subprocess\nsubprocess.run(['tee', 'target/report.txt'], check=True)\n",
        "import subprocess\nsubprocess.run(['unzip', '-P', 'secret', '-l', 'payload.zip'], check=True)\n",
        "import subprocess\nsubprocess.run(['unzip', '-Psecret', '-l', 'payload.zip'], check=True)\n",
        "import subprocess\nsubprocess.run(['unzip', '-Pindex', '-l', 'payload.zip'], check=True)\n",
        "import subprocess\nsubprocess.run(['unzip', '-PTEST', '-l', 'payload.zip'], check=True)\n",
        "import subprocess\nsubprocess.run(['/usr/bin/uname', '-a'], check=True)\n",
        "open('target/MAKEFILE.txt', 'w').write('report')\n",
        "open('.github/workflow/ci.yml', 'w').write('report')\n",
        "import tempfile\ntempfile.NamedTemporaryFile(dir='.GITHUB/artifacts', suffix='.yml')\n",
    ] {
        fs::write(workspace.path().join("script/check.py"), source).expect("safe Python writer text");
        git(workspace.path(), &["add", "."]);
        assert!(reject_checked_in_weakening(workspace.path()).is_ok(), "{source}");
    }
}

#[test]
fn command_policy_rejects_python_execution_surface_ancestor_mutations() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    fs::create_dir_all(workspace.path().join("quality")).expect("quality directory");
    fs::create_dir_all(workspace.path().join("payload")).expect("payload directory");
    fs::write(workspace.path().join("quality/check"), "#!/bin/sh\ntrue\n").expect("shebang-discovered command surface");
    fs::write(workspace.path().join("payload/report.txt"), "report\n").expect("copytree source");
    fs::write(workspace.path().join("script/check.py"), "print('initial')\n").expect("initial Python source");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    for source in [
        "import shutil\nshutil.rmtree('quality')\n",
        "import shutil\nshutil.copytree('payload', 'quality', dirs_exist_ok=True)\n",
        "import os\nos.rename('quality', 'moved')\n",
        "import shutil\nshutil.move('quality', 'moved')\n",
        "import os\nos.replace('quality', 'moved')\n",
    ] {
        fs::write(workspace.path().join("script/check.py"), source).expect("Python ancestor mutation");
        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("lint-weakening argument"), "{source}: {error:#}");
    }
}

#[test]
fn command_policy_allows_python_mutations_of_safe_siblings() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    fs::create_dir_all(workspace.path().join("quality")).expect("quality directory");
    fs::create_dir_all(workspace.path().join("quality-data")).expect("safe sibling directory");
    fs::write(workspace.path().join("quality/check"), "#!/bin/sh\ntrue\n").expect("shebang-discovered command surface");
    fs::write(workspace.path().join("quality-data/report.txt"), "report\n").expect("safe sibling content");
    fs::write(workspace.path().join("script/check.py"), "print('initial')\n").expect("initial Python source");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    for source in [
        "import shutil\nshutil.rmtree('quality-data')\n",
        "import os\nos.rename('quality-data', 'notes')\n",
        "import shutil\nshutil.move('quality-data', 'notes')\n",
        "import os\nos.replace('quality-data', 'notes')\n",
    ] {
        fs::write(workspace.path().join("script/check.py"), source).expect("safe Python sibling mutation");
        reject_checked_in_weakening(workspace.path()).unwrap_or_else(|error| panic!("{source}: {error:#}"));
    }
}

#[cfg(unix)]
#[test]
fn command_policy_resolves_python_writer_symlink_parents() {
    use std::os::unix::fs::symlink;

    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    fs::create_dir_all(workspace.path().join("docs")).expect("documentation directory");
    fs::create_dir_all(workspace.path().join("quality")).expect("quality directory");
    fs::create_dir_all(workspace.path().join("payload")).expect("payload directory");
    fs::write(workspace.path().join("script/check"), "#!/bin/sh\ntrue\n").expect("safe command surface");
    fs::write(workspace.path().join("quality/check"), "#!/bin/sh\ntrue\n").expect("shebang-discovered command surface");
    fs::write(workspace.path().join("payload/check"), "replacement\n").expect("copy payload");
    symlink("../quality/check", workspace.path().join("payload/link")).expect("tracked source symlink");
    symlink("..", workspace.path().join("docs/root")).expect("repository-relative directory symlink");
    symlink("../quality", workspace.path().join("docs/bridge")).expect("relocatable internal directory symlink");
    fs::write(workspace.path().join("script/check.py"), "open('docs/root/script/check', 'w').write(payload)\n").expect("redirected Python writer");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);
    symlink("../script/check", workspace.path().join("docs/check")).expect("implicit destination symlink");

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("lint-weakening argument"), "{error:#}");

    fs::write(workspace.path().join("script/check.py"), "import shutil\nshutil.copy('payload/check', 'docs')\n").expect("implicit symlink destination writer");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("lint-weakening argument"), "{error:#}");

    fs::write(
        workspace.path().join("script/check.py"),
        "import shutil\nshutil.copy(\n    'payload/link',\n    'docs',\n    follow_symlinks=False  # preserve the link\n)\nopen('docs/link', 'w').write(payload)\n",
    )
    .expect("copied symlink write-through");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("lint-weakening argument"), "{error:#}");

    fs::write(workspace.path().join("script/check.py"), "open('docs/root/quality/check', 'w').write(payload)\n").expect("redirected Python writer to discovered surface");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("lint-weakening argument"), "{error:#}");

    fs::write(
        workspace.path().join("script/check.py"),
        "import os\nos.rename('docs/bridge', 'docs/moved')\nopen('docs/moved/check', 'w').write(payload)\n",
    )
    .expect("relocated internal symlink writer");
    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("lint-weakening argument"), "{error:#}");

    git(workspace.path(), &["add", "."]);
    for source in [
        "open('docs/report.txt', 'w').write(payload)\n",
        "import shutil\nshutil.copy(\n    'payload/link',\n    'docs',\n    follow_symlinks=True  # ordinary copy\n)\n",
    ] {
        fs::write(workspace.path().join("script/check.py"), source).expect("safe Python writer");
        reject_checked_in_weakening(workspace.path()).unwrap_or_else(|error| panic!("{source}: {error:#}"));
    }
}

#[test]
fn command_policy_rejects_dos_short_name_write_paths() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    fs::create_dir_all(workspace.path().join("quality")).expect("quality directory");
    fs::create_dir_all(workspace.path().join("maintenance-tools")).expect("long command directory");
    fs::write(workspace.path().join("quality/maintenance-check"), "#!/bin/sh\ntrue\n").expect("long command surface");
    fs::write(workspace.path().join("maintenance-tools/check"), "#!/bin/sh\ntrue\n").expect("command in long directory");
    fs::write(workspace.path().join("script/check.py"), "print('initial')\n").expect("initial Python source");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    for source in [
        "open('quality/MAINTE~1', 'w').write(payload)\n",
        "open('MAINTE~1/check', 'w').write(payload)\n",
        "open('target/REPORT~1.TXT', 'w').write(payload)\n",
        "open('target/report~1.txt', 'w').write(payload)\n",
    ] {
        fs::write(workspace.path().join("script/check.py"), source).expect("DOS short-name writer");
        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("lint-weakening argument"), "{source}: {error:#}");
    }

    for source in [
        "open('target/report~notes.txt', 'w').write(payload)\n",
        "open('target/verylong~1.txt', 'w').write(payload)\n",
        "open('target/report~1.long', 'w').write(payload)\n",
        "open('target/rep ort~1.txt', 'w').write(payload)\n",
    ] {
        fs::write(workspace.path().join("script/check.py"), source).expect("safe tilde writer");
        reject_checked_in_weakening(workspace.path()).unwrap_or_else(|error| panic!("{source}: {error:#}"));
    }
}

#[test]
fn command_policy_rejects_python_protected_input_mutations() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    fs::create_dir_all(workspace.path().join("src")).expect("source directory");
    fs::create_dir_all(workspace.path().join("src-data")).expect("safe sibling directory");
    fs::create_dir_all(workspace.path().join("payload")).expect("payload directory");
    fs::write(workspace.path().join("src/lib.rs"), "pub fn library() {}\n").expect("protected Rust source");
    fs::write(workspace.path().join("src-data/report.txt"), "report\n").expect("safe sibling content");
    fs::write(workspace.path().join("payload/report.txt"), "replacement\n").expect("replacement content");
    fs::write(workspace.path().join("script/check.py"), "print('initial')\n").expect("initial Python source");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    for source in [
        "import shutil\nshutil.rmtree('src')\n",
        "import shutil\nshutil.copytree('payload', 'src', dirs_exist_ok=True)\n",
        "import os\nos.rename('src', 'archived')\n",
        "import shutil\nshutil.copy('payload/report.txt', 'src')\n",
        "import shutil\nshutil.move('payload/report.txt', 'src')\n",
        "import shutil\nshutil.move('src/lib.rs', 'archived.rs')\n",
        "from pathlib import Path\nPath('src/lib.rs').rename('archived.rs')\n",
        "from pathlib import Path\nPath('payload/report.txt').replace('src/lib.rs')\n",
        "from pathlib import Path\nPath('payload/report.txt').copy('src/lib.rs')\n",
        "from pathlib import Path\nPath.copy(Path('payload/report.txt'), 'src/lib.rs')\n",
        "from pathlib import Path\nPath('payload/report.txt').copy_into('src')\n",
        "from pathlib import Path\nPath('payload/report.txt').move('src/lib.rs')\n",
        "from pathlib import Path\nPath('payload/report.txt').move_into('src')\n",
        "from pathlib import Path\nwriter = Path('payload/report.txt').copy\nwriter('src/lib.rs')\n",
        "open('src/lib.rs', 'w').write(payload)\n",
        "import zipapp\nzipapp.create_archive('quality/payload', 'script/check.py')\n",
        "import zipfile\nzipfile.ZipFile('script/check.py', 'w').writestr('__main__.py', payload)\n",
    ] {
        fs::write(workspace.path().join("script/check.py"), source).expect("protected-input mutation");
        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("lint-weakening argument"), "{source}: {error:#}");
    }

    // Python 3.14 tree and implicit-destination semantics are intentionally
    // unmodeled, so data-only invocations also fail closed for now.
    for source in [
        "source.copy('src-data/copy.txt')\n",
        "from pathlib import Path\nPath('payload/report.txt').copy('src-data/copy.txt')\n",
        "from pathlib import Path\nPath('payload/report.txt').copy_into('src-data')\n",
        "from pathlib import Path\nPath('payload/report.txt').move('src-data/moved.txt')\n",
        "from pathlib import Path\nPath('payload/report.txt').move_into('src-data')\n",
    ] {
        fs::write(workspace.path().join("script/check.py"), source).expect("conservative Python 3.14 mutation");
        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("lint-weakening argument"), "{source}: {error:#}");
    }

    fs::write(workspace.path().join("script/check.py"), "import shutil\nshutil.rmtree('src-data')\n").expect("safe sibling mutation");
    reject_checked_in_weakening(workspace.path()).expect("similarly named data directory remains mutable");
}

#[test]
fn command_policy_rejects_implicit_python_copy_destinations() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    for directory in ["script", "payload/tree", "generated", ".github/actions/check", "data"] {
        fs::create_dir_all(workspace.path().join(directory)).expect("fixture directory");
    }
    for (path, contents) in [
        ("payload/config.toml", "[build]\n"),
        ("payload/check.py", "print('safe')\n"),
        ("payload/report.txt", "report\n"),
        ("payload/report.json", "{}\n"),
        ("payload/tree/config.toml", "[build]\n"),
        ("script/check.py", "print('initial')\n"),
    ] {
        fs::write(workspace.path().join(path), contents).expect("fixture file");
    }
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    for source in [
        "import os, shutil\nos.mkdir('.cargo')\nshutil.copy('payload/config.toml', '.cargo')\n",
        "import shutil\nshutil.copy2('payload/check.py', 'generated')\n",
        "import shutil\nshutil.copy('payload/action.yml', '.github/actions/check')\n",
        "import os, shutil\nos.mkdir('.cargo')\nshutil.move('payload/config.toml', '.cargo')\n",
        "import shutil\nshutil.copytree('payload/tree', '.cargo', dirs_exist_ok=True)\n",
    ] {
        fs::write(workspace.path().join("script/check.py"), source).expect("implicit protected destination");
        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("lint-weakening argument"), "{source}: {error:#}");
    }

    for source in [
        "import shutil\nshutil.copy('payload/report.txt', 'data')\n",
        "import shutil\nshutil.copy2('payload/report.json', 'data')\n",
        "import shutil\nshutil.move('payload/report.txt', 'data')\n",
    ] {
        fs::write(workspace.path().join("script/check.py"), source).expect("safe implicit destination");
        reject_checked_in_weakening(workspace.path()).unwrap_or_else(|error| panic!("{source}: {error:#}"));
    }
}

#[test]
fn command_policy_rejects_python_working_directory_rebased_writes() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    fs::create_dir_all(workspace.path().join("quality")).expect("quality directory");
    fs::create_dir_all(workspace.path().join("data")).expect("data directory");
    fs::write(workspace.path().join("quality/check"), "#!/bin/sh\ntrue\n").expect("command surface");
    fs::write(workspace.path().join("data/report.txt"), "report\n").expect("data file");
    fs::write(workspace.path().join("script/check.py"), "print('initial')\n").expect("initial Python source");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    for source in [
        "import os\nos.chdir('quality')\nos.remove('check')\n",
        "import os\ndirectory = os.open('quality', os.O_RDONLY)\nos.fchdir(directory)\nos.remove('check')\n",
        "import contextlib, os\nwith contextlib.chdir('quality'):\n    os.remove('check')\n",
        "from contextlib import chdir as change\nimport os\nwith change('quality'):\n    os.remove('check')\n",
        "from os import fchdir as change\nimport os\ndirectory = os.open('quality', os.O_RDONLY)\nchange(directory)\nos.remove('check')\n",
        "import os\nos.chdir('quality')\nmessage = f\"{open('target/report.txt', 'w')}\"\n",
    ] {
        fs::write(workspace.path().join("script/check.py"), source).expect("working-directory rebased write");
        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("lint-weakening argument") || message.contains("opaque interpreter program"),
            "{source}: {error:#}"
        );
    }

    for source in [
        "import os\nos.chdir('quality')\n",
        "import os\nos.remove('data/report.txt')\n",
        "import os\nos.chdir('data')\nopen('report.txt', 'rb').read()\n",
    ] {
        fs::write(workspace.path().join("script/check.py"), source).expect("safe working-directory control");
        reject_checked_in_weakening(workspace.path()).unwrap_or_else(|error| panic!("{source}: {error:#}"));
    }
}

#[test]
fn command_policy_rejects_python_directory_descriptor_rebasing() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    fs::create_dir_all(workspace.path().join("quality/commands")).expect("command directory");
    fs::create_dir_all(workspace.path().join("payload")).expect("payload directory");
    fs::create_dir_all(workspace.path().join("data")).expect("data directory");
    fs::write(workspace.path().join("quality/check"), "#!/bin/sh\ntrue\n").expect("command surface");
    fs::write(workspace.path().join("quality/commands/check"), "#!/bin/sh\ntrue\n").expect("nested command surface");
    fs::write(workspace.path().join("payload/report.txt"), "replacement\n").expect("replacement data");
    fs::write(workspace.path().join("data/report.txt"), "report\n").expect("safe data");
    fs::write(workspace.path().join("script/check.py"), "print('initial')\n").expect("initial Python source");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    for source in [
        "import os\ndirectory = os.open('quality', os.O_RDONLY)\nos.remove('check', dir_fd=directory)\n",
        "open(\n    'quality/check',\n    # positional mode follows\n    'w'  # final positional\n).write(payload)\n",
        "open(\n    file='quality/check',\n    # keyword mode follows\n    mode='w'  # final keyword\n).write(payload)\n",
        "from pathlib import Path\nPath('quality/check').open(  # positional mode follows\n    'w'  # final positional\n)\n",
        "import os\nos.fdopen(descriptor,  # positional mode follows\n    'w'  # final positional\n)\n",
        "import os\ndirectory = os.open('quality', os.O_RDONLY)\nos.remove(\n    'check',\n    dir_fd=directory  # rebased destination\n)\n",
        "open('data/report.txt', 'w',\n    opener=custom_opener  # custom destination resolution\n)\n",
        "import tempfile\ntempfile.NamedTemporaryFile(dir='data',\n    prefix='../quality/check-'  # traversing prefix\n)\n",
        "import tempfile\ntempfile.NamedTemporaryFile(dir='target',\n    suffix='.rs'  # protected suffix\n)\n",
        "import tempfile\ntempfile.NamedTemporaryFile(\n    dir='script'  # protected directory\n)\n",
        "import tempfile\ntempfile.NamedTemporaryFile(dir='target', prefix=r'\\script\\check-', suffix='.txt')\n",
        "import tempfile\ntempfile.NamedTemporaryFile(dir='target', prefix=r'\\\\server\\share\\check-', suffix='.txt')\n",
        "import tempfile\ntempfile.NamedTemporaryFile(dir='target', prefix=r'C:\\script\\check-', suffix='.txt')\n",
        "import os\nsource = os.open('payload', os.O_RDONLY)\ndestination = os.open('quality', os.O_RDONLY)\nos.rename('report.txt', 'check', src_dir_fd=source, dst_dir_fd=destination)\n",
        "import os, shutil\ndirectory = os.open('quality', os.O_RDONLY)\nshutil.rmtree('commands', dir_fd=directory)\n",
        "import os\ndirectory = os.open('quality', os.O_RDONLY)\nos.open('check', os.O_WRONLY, dir_fd=directory)\n",
        "import os\ndirectory = os.open('quality', os.O_RDONLY)\noptions = {'dir_fd': directory}\nos.remove('check', **options)\n",
        "import os\ndirectory = os.open('quality', os.O_RDONLY)\nopen('check', 'w', opener=lambda path, flags: os.open(path, flags, dir_fd=directory))\n",
        "open('target/report.txt', **options)\n",
        "open(*arguments)\n",
        "import io\nio.open(*arguments)\n",
        "import os\nos.fdopen(*arguments)\n",
        "from pathlib import Path\nPath('target/report.txt').open(*arguments)\n",
        "from pathlib import Path\nPath('Justfile').open(**options)\n",
        "import tempfile\ntempfile.NamedTemporaryFile(**options)\n",
        "import tempfile\ntempfile.NamedTemporaryFile(*arguments)\n",
        "import tempfile\ntempfile.NamedTemporaryFile()\n",
        "import os, tempfile\nos.environ['TMPDIR'] = 'script'\ntempfile.NamedTemporaryFile(prefix='generated-', suffix='.py')\n",
        "import tempfile\ntempfile.tempdir = 'script'\ntempfile.NamedTemporaryFile(prefix='generated-', suffix='.py')\n",
        "open('data/report.txt', 'w'",
        "from pathlib import Path\nPath('data/report.txt').open('w'",
        "import os\nos.fdopen(descriptor, 'rb'",
        "import shutil\nshutil.copy('payload/report.txt', 'data'",
        "import tempfile\ntempfile.NamedTemporaryFile(dir='target'",
        "import fileinput\nfileinput.input('Justfile', inplace=True)\n",
        "open('data/report.txt', 'w)",
        "open('data/report.txt', 'w'])",
    ] {
        fs::write(workspace.path().join("script/check.py"), source).expect("directory-descriptor rebased write");
        let error = reject_checked_in_weakening(workspace.path())
            .err()
            .unwrap_or_else(|| panic!("accepted opaque Python writer: {source}"));
        assert!(error.to_string().contains("lint-weakening argument"), "{source}: {error:#}");
    }

    for source in [
        "import os\ndirectory = os.open('quality', os.O_RDONLY)\nos.open('check', os.O_RDONLY, dir_fd=directory)\n",
        "open('quality/check',\n    # read mode follows\n    'rb'  # final positional\n)\n",
        "open(file='data/report.txt',\n    # safe keyword follows\n    mode='w'  # final keyword\n)\n",
        "from pathlib import Path\nPath('data/report.txt').open(  # safe positional mode follows\n    'w'  # final positional\n)\n",
        "import os\nos.fdopen(descriptor,  # read mode follows\n    'rb'  # final positional\n)\n",
        "import os\nos.remove('data/report.txt', dir_fd=None)\n",
        "import os\nos.remove('data/report.txt',\n    dir_fd=None  # no rebasing\n)\n",
        "open('data/report.txt', 'w',\n    opener=None  # standard opener\n)\n",
        "import os\nos.remove('data/report.txt')\n",
        "import tempfile\ntempfile.NamedTemporaryFile(dir='target', prefix='report-', suffix='.txt')\n",
        "import tempfile\ntempfile.NamedTemporaryFile(prefix='report-', suffix='.txt',\n    dir='target'  # explicit safe directory\n)\n",
        "import tempfile\ntempfile.NamedTemporaryFile(dir='target', prefix=r'reports\\check-', suffix='.txt')\n",
    ] {
        fs::write(workspace.path().join("script/check.py"), source).expect("safe directory-descriptor control");
        reject_checked_in_weakening(workspace.path()).unwrap_or_else(|error| panic!("{source}: {error:#}"));
    }
}

#[test]
fn command_policy_rejects_runtime_code_object_construction() {
    assert_opaque_python_process_bindings(&[
        "code_type = (lambda: None).__code__.__class__\ncode_type(*arguments)\n",
        "function_type = (lambda: None).__class__\nfunction_type(code, globals())\n",
        "generator = (value for value in ())\ncode_type = generator.gi_code.__class__\ncode_type(*arguments)\n",
        "message = f\"{(lambda: None).__code__.__class__(*arguments)}\"\n",
    ]);
}

#[test]
fn command_policy_rejects_opaque_python_process_bindings() {
    assert_opaque_python_process_bindings(&[
        "from os import (\n    system,\n)\nsystem('sh quality/hidden.txt')\n",
        "from posix import (\n    system,\n)\nsystem('sh quality/hidden.txt')\n",
        "from os import (\n    startfile,\n)\nstartfile('quality/hidden.txt')\n",
        "import os\nprocess = os\nprocess.system('sh quality/hidden.txt')\n",
        "import json, subprocess as process\nprocess.run(['git', 'status'])\n",
        "import json; import os as process; process.system('sh quality/hidden.txt')\n",
        "from platform import os as process\nprocess.system('sh quality/hidden.txt')\n",
        "import platform\nprocess = platform.os\nprocess.system('sh quality/hidden.txt')\n",
        "from contextlib import chdir\nwith chdir('quality'):\n    subprocess.run(['git'])\n",
        "import os\nchange = os.chdir\nchange('quality')\n",
        "import os\nchange = os.putenv\nchange('Path', 'quality')\n",
        "import os\nchange = os . chdir\nchange('quality')\n",
        "import os\nenvironment = os . environ\nenvironment['PATH'] = 'quality'\n",
        "import os\nchange = os.chdir.__call__\nchange('quality')\n",
        "import sys\nsys.modules['os'].system('sh quality/hidden.txt')\n",
        "import sys as registry\nregistry.modules['os'].system('sh quality/hidden.txt')\n",
        "import os\nos.__getattribute__('system')('sh quality/hidden.txt')\n",
        "import os\nos.__getattribute__('environ')['PATH'] = 'quality'\n",
        "import sys\nsys.__dict__['modules']['os'].system('sh quality/hidden.txt')\n",
        "import sys\nsys.__getattribute__('modules')['os'].system('sh quality/hidden.txt')\n",
        "import contextlib\nwith contextlib.__getattribute__('chdir')('quality'):\n    subprocess.run(['git'])\n",
        "globals()['os'].system('sh quality/hidden.txt')\n",
        "locals()['os'].system('sh quality/hidden.txt')\n",
        "vars()['os'].system('sh quality/hidden.txt')\n",
        "import os\nlookup = globals\nlookup()['os'].system('sh quality/hidden.txt')\n",
        "import os\nlookup = os.__getattribute__\nlookup('system')('sh quality/hidden.txt')\n",
        "import os, sys\nsys._getframe().f_globals['os'].system('sh quality/hidden.txt')\n",
        "import os\nfunction.__globals__['os'].system('sh quality/hidden.txt')\n",
        "object.__subclasses__()\n",
        "import os\ndef f(): pass\ngetattr(f, '__globals__')['os'].system('sh quality/hidden.txt')\n",
        "import operator\noperator.attrgetter('__globals__')(f)['os'].system('sh quality/hidden.txt')\n",
        "import os\nos.posix_spawn('quality/hidden.py', ['quality/hidden.py'], os.environ)\n",
        "from posix import posix_spawnp\nposix_spawnp('quality/hidden.py', ['quality/hidden.py'], {})\n",
        "import pty\npty.spawn(['quality/hidden.py'])\n",
        "from pty import spawn\nspawn(['quality/hidden.py'])\n",
        "import pty\nlaunch = pty.spawn\nlaunch(['quality/hidden.py'])\n",
        "import os, subprocess\nos.posix_spawn('quality/hidden.py', ['quality/hidden.py'], os.environ)\nsubprocess.run(['git', 'status'])\n",
        "from os import posix_spawn\nimport subprocess\nposix_spawn('quality/hidden.py', ['quality/hidden.py'], {})\nsubprocess.run(['git', 'status'])\n",
        "from posix import posix_spawnp\nimport subprocess\nposix_spawnp('quality/hidden.py', ['quality/hidden.py'], {})\nsubprocess.run(['git', 'status'])\n",
        "import posix, subprocess\nposix.posix_spawn('quality/hidden.py', ['quality/hidden.py'], {})\nsubprocess.run(['git', 'status'])\n",
        "import pty, subprocess\npty.spawn(['quality/hidden.py'])\nsubprocess.run(['git', 'status'])\n",
        "import pydoc\npydoc.pipepager('', 'sh quality/hidden.txt')\n",
        "import pydoc as docs\ndocs.pipepager('', 'sh quality/hidden.txt')\n",
        "if True: import pydoc as docs\ndocs.pipepager('', 'sh quality/hidden.txt')\n",
        "from pydoc import pipepager\npipepager('', 'sh quality/hidden.txt')\n",
        "import webbrowser\nwebbrowser.BackgroundBrowser('sh').open('quality/hidden.txt')\n",
        "import webbrowser as browser\nbrowser.BackgroundBrowser('sh').open('quality/hidden.txt')\n",
        "from webbrowser import BackgroundBrowser\nBackgroundBrowser('sh').open('quality/hidden.txt')\n",
        "import pkgutil\npkgutil.resolve_name('webbrowser:BackgroundBrowser')('sh').open('quality/hidden.txt')\n",
        "from logging.config import BaseConfigurator\nBaseConfigurator({}).resolve('webbrowser.BackgroundBrowser')('sh').open('quality/hidden.txt')\n",
        "from optparse import Values\nvalues = Values()\nvalues.read_module('webbrowser', 'loose')\nvalues.BackgroundBrowser('sh').open('quality/hidden.txt')\n",
        "import sys\nspec = next(spec for finder in sys.meta_path if (spec := finder.find_spec('webbrowser')) is not None)\nspec.loader.load_module('webbrowser').BackgroundBrowser('sh').open('quality/hidden.txt')\n",
        "print.__self__.__import__('webbrowser').BackgroundBrowser('sh').open('quality/hidden.txt')\n",
        "from unittest import mock\nmock.patch('webbrowser.BackgroundBrowser').getter().BackgroundBrowser('sh').open('quality/hidden.txt')\n",
        "import code\ncode.InteractiveInterpreter().runsource(Path('quality/hidden.txt').read_text(), symbol='exec')\n",
        "import code as repl\nrepl.InteractiveConsole().push(\"__import__('os').system('sh quality/hidden.txt')\")\n",
        "from code import InteractiveInterpreter as Runner\nRunner().runcode(compile('pass', '<string>', 'exec'))\n",
        "from code import interact\ninteract(local={})\n",
        "import site\nsite.addsitedir('quality')\n",
        "import site as paths\npaths.addpackage('quality', 'hidden.pth', set())\n",
        "from site import addsitedir\naddsitedir('quality')\n",
        "import shelve\nshelve.open('quality/db')['payload']\n",
        "import os\nos.ｓｙｓｔｅｍ(bytes.fromhex(payload))\n",
        "import os as ｐｒｏｃｅｓｓ\nprocess.system('sh quality/hidden.txt')\n",
        "from ｏｓ import ｓｙｓｔｅｍ as run\nrun(bytes.fromhex(payload))\n",
        "message = f\"{_＿import＿_('os')}\"\n",
        "message = f\"{os.posix＿spawn(path, argv, env)}\"\n",
        "breakpoint()\n",
        "import sys\nsys.breakpointhook()\n",
        "import sys\nsys.__breakpointhook__()\n",
        "message = f\"{sys.modules['os'].system('sh quality/hidden.txt')}\"\n",
        "message = f\"{globals()['os'].system('sh quality/hidden.txt')}\"\n",
        "message = f\"{os.__getattribute__('system')('sh quality/hidden.txt')}\"\n",
    ]);
}

#[test]
fn command_policy_rejects_native_stdlib_execution_escape_hatches() {
    assert_opaque_python_process_bindings(&[
        "import logging\nhandler = logging.FileHandler('Justfile', mode='w')\nhandler.stream.write(payload)\n",
        "import sqlite3\nsqlite3.connect(':memory:').enable_load_extension(True)\nsqlite3.connect(':memory:').load_extension('quality/payload')\n",
        "from _sqlite3 import connect\nconnect(':memory:').load_extension('quality/payload')\n",
        "import dbm.sqlite3\ndbm.sqlite3.open('quality/database')._cx.load_extension('quality/payload')\n",
        "connection.load_extension('quality/payload')\n",
        "connection.setconfig(SQLITE_DBCONFIG_ENABLE_LOAD_EXTENSION, True)\nconnection.execute('select load_extension(?)', (payload,))\n",
        "import tkinter\ntkinter.Tcl().call('exec', 'sh', 'quality/hidden.txt')\n",
        "from tkinter import Tcl\nTcl().call('open', '|sh quality/hidden.txt')\n",
        "import _tkinter\nprint(_tkinter.TCL_VERSION)\n",
        "import turtle\nturtle.TK.Tcl().call('exec', 'sh', 'quality/hidden.txt')\n",
        "import turtledemo.__main__ as demo\ndemo.Tcl().call('exec', 'sh', 'quality/hidden.txt')\n",
        "from test import test_tcl\ntest_tcl.Tcl().call('exec', 'sh', 'quality/hidden.txt')\n",
        "import _xxsubinterpreters\nfrom pathlib import Path\n_xxsubinterpreters.run_string(_xxsubinterpreters.create(), Path('quality/hidden.txt').read_text())\n",
        "import _testcapi\nrunner = _testcapi.run_in_subinterp\nrunner(payload)\n",
        "from _testinternalcapi import exec_interpreter as run\nrun(interpreter, payload)\n",
        "import _testlimitedcapi as api\ncompiler = api.run_compilestring\nloader = api.PyImport_ExecCodeModule\ncode = compiler(payload, b'<payload>', 257)\nloader('_localhold_payload', code)\n",
        "from _ctypes import dlopen as load\nload('quality/payload')\n",
        "from _interpreters import exec as run\nrun(interpreter, payload)\n",
        "from concurrent import interpreters\ninterpreter = interpreters.create()\ninterpreter.exec(payload)\n",
        "from concurrent import (\n    interpreters,\n)\ninterpreter = interpreters.create()\ninterpreter.exec(payload)\n",
        "from concurrent import (\n    interpreters,\n)\nprint(interpreters)\n",
        "from test.support import interpreters\ninterpreter = interpreters.create()\ninterpreter.exec(payload)\n",
        "import test.test__interpreters as tests\nrunner = tests._interpreters.run_string\nrunner(tests._interpreters.create(), payload)\n",
        "import test.test_ttk as tests\ninterpreter = tests.tkinter.Tcl()\ninterpreter.call('exec', 'sh', 'quality/hidden.txt')\n",
        "import pipes\npipeline = pipes.Template()\npipeline.append('sh quality/hidden.txt', '--')\npipeline.open_r('/dev/null').read()\n",
        "import venv\nvenv.EnvBuilder()._call_new_python(context, 'quality/hidden.txt')\n",
        "import _osx_support\n_osx_support._read_output('sh quality/hidden.txt')\n",
        "import dataclasses\nfrom pathlib import Path\nbuilder = dataclasses._FuncBuilder(globals())\nbuilder.add_fn('payload', [], ['arg=' + Path('quality/hidden.txt').read_text()])\nbuilder.add_fns_to_class(Target)\n",
    ]);
}

#[test]
fn command_policy_rejects_embedded_package_and_private_process_dispatch() {
    assert_opaque_python_process_bindings(&[
        "from _posixsubprocess import fork_exec as launch\nlaunch(*arguments)\n",
        "import _winapi\n_winapi.CreateProcess(None, 'cmd /c sh quality/hidden.txt', None, None, False, 0, None, None, startup_info)\n",
        "from subprocess import _winapi as native\nlaunch = native.CreateProcess\nlaunch(*arguments)\n",
        "import asyncio.windows_utils as windows\nwindows._winapi.CreateProcess(*arguments)\n",
        "import subprocess\nlaunch = subprocess._fork_exec\nlaunch(*arguments)\n",
        "from subprocess import _fork_exec as launch\nlaunch.__call__(*arguments)\n",
        "from subprocess import Popen as launch\nlaunch(['sh', 'quality/hidden.txt'])\n",
        "from pip._internal.cli.main import main\nmain(['install', 'quality/payload.tar.gz'])\n",
        "from pip . _internal . cli . main import main\nmain(['wheel', 'quality/payload.tar.gz'])\n",
        "from pip. \\\n+_internal.cli.main import main\nmain(['wheel', 'quality/payload.tar.gz'])\n",
        "import pip._internal.cli.main as cli\nrunner = cli.main\nrunner(['wheel', 'quality/payload.tar.gz'])\n",
        "from pip._internal.commands import create_command\ncreate_command('wheel').main(['quality/payload.tar.gz'])\n",
        "from ensurepip import _run_pip as run\nrun(['install', 'quality/payload.tar.gz'])\n",
    ]);
}

#[test]
fn command_policy_rejects_debugger_string_execution() {
    assert_opaque_python_process_bindings(&[
        "import bdb\nfrom pathlib import Path\nbdb.Bdb().run(Path('quality/hidden.txt').read_text())\n",
        "from bdb import Bdb\nrunner = Bdb().runctx\nrunner.__call__(payload, globals(), locals())\n",
    ]);
}

#[test]
fn command_policy_rejects_multiprocessing_deserialization() {
    assert_opaque_python_process_bindings(&["from multiprocessing.reduction import ForkingPickler\nForkingPickler.loads(payload)\n"]);
}

#[test]
fn command_policy_rejects_asyncio_process_bindings() {
    assert_opaque_python_process_bindings(&[
        "import asyncio\nasyncio.create_subprocess_exec('quality/hidden.py')\n",
        "import asyncio as loop\nloop.create_subprocess_shell('sh quality/hidden.txt')\n",
        "from asyncio import create_subprocess_exec\ncreate_subprocess_exec('quality/hidden.py')\n",
        "import asyncio.subprocess\nawait asyncio.subprocess.create_subprocess_exec('quality/hidden.py')\n",
        "from asyncio.subprocess import create_subprocess_shell\nawait create_subprocess_shell('sh quality/hidden.txt')\n",
        "from asyncio.subprocess import create_subprocess_exec as launch\nawait launch('quality/hidden.py')\n",
        "import asyncio\nawait asyncio.get_running_loop().subprocess_shell(asyncio.SubprocessProtocol, 'sh quality/hidden.txt')\n",
        "from asyncio import Runner, SubprocessProtocol\nawait Runner().get_loop().subprocess_exec(SubprocessProtocol, 'quality/hidden.py')\n",
        "import asyncio\nloop = asyncio.get_running_loop()\nlaunch = loop.subprocess_shell\nawait launch(asyncio.SubprocessProtocol, 'sh quality/hidden.txt')\n",
        "import asyncio\nloop = asyncio.get_running_loop()\nlaunch = (loop\n    .subprocess_exec)\nawait launch(asyncio.SubprocessProtocol, 'quality/hidden.py')\n",
        "import asyncio\nloop = asyncio.get_running_loop()\nlaunch = (loop.\n    subprocess_exec)\nawait launch(asyncio.SubprocessProtocol, 'quality/hidden.py')\n",
        "import asyncio\nloop = asyncio.get_running_loop()\nawait loop.__getattribute__('subprocess_exec')(asyncio.SubprocessProtocol, 'quality/hidden.py')\n",
        "import asyncio\nloop = asyncio.get_running_loop()\nlaunch = asyncio.BaseEventLoop.__dict__['subprocess_exec']\nawait launch(loop, asyncio.SubprocessProtocol, 'quality/hidden.py')\n",
        "import asyncio\nloop = asyncio.get_running_loop()\nlaunch = vars(asyncio.BaseEventLoop)['subprocess_exec']\nawait launch(loop, asyncio.SubprocessProtocol, 'quality/hidden.py')\n",
        "import asyncio, inspect\nloop = asyncio.get_running_loop()\nlaunch = inspect.getattr_static(loop, 'subprocess_exec').__get__(loop)\nawait launch(asyncio.SubprocessProtocol, 'quality/hidden.py')\n",
        "import asyncio\nfrom inspect import getattr_static as lookup\nloop = asyncio.get_running_loop()\nlaunch = lookup(loop, 'subprocess_exec').__get__(loop)\nawait launch(asyncio.SubprocessProtocol, 'quality/hidden.py')\n",
        "import asyncio, inspect\nloop = asyncio.get_running_loop()\nlookup = inspect.getattr_static\nlaunch = lookup(loop, 'subprocess_exec').__get__(loop)\nawait launch(asyncio.SubprocessProtocol, 'quality/hidden.py')\n",
        "import asyncio, inspect\nloop = asyncio.get_running_loop()\nlaunch = dict(inspect.getmembers(loop))['subprocess_exec']\nawait launch(asyncio.SubprocessProtocol, 'quality/hidden.py')\n",
        "import asyncio, operator\nloop = asyncio.get_running_loop()\nawait operator.methodcaller('subprocess_shell', asyncio.SubprocessProtocol, 'sh quality/hidden.txt')(loop)\n",
        "import asyncio\nfrom operator import methodcaller as invoke\nloop = asyncio.get_running_loop()\nawait invoke('subprocess_shell', asyncio.SubprocessProtocol, 'sh quality/hidden.txt')(loop)\n",
        "import asyncio, subprocess\nasyncio.create_subprocess_exec('quality/hidden.py')\nsubprocess.run(['git', 'status'])\n",
    ]);
}

fn assert_opaque_python_process_bindings(sources: &[&str]) {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    git(workspace.path(), &["init", "-q"]);
    for source in sources {
        fs::write(workspace.path().join("script/check.py"), source).expect("opaque Python process binding");
        git(workspace.path(), &["add", "."]);
        let Err(error) = reject_checked_in_weakening(workspace.path()) else {
            panic!("accepted opaque Python process binding: {source}");
        };
        assert!(error.to_string().contains("opaque interpreter program"), "{source}: {error:#}");
    }
}

fn assert_opaque_python_filesystem_writes(sources: &[&str]) {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    git(workspace.path(), &["init", "-q"]);
    for source in sources {
        fs::write(workspace.path().join("script/check.py"), source).expect("opaque Python filesystem writer");
        git(workspace.path(), &["add", "."]);
        let Err(error) = reject_checked_in_weakening(workspace.path()) else {
            panic!("accepted opaque Python filesystem writer: {source}");
        };
        assert!(error.to_string().contains("lint-weakening argument"), "{source}: {error:#}");
    }
}

#[test]
fn command_policy_tracks_whitespace_qualified_python_process_calls() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    git(workspace.path(), &["init", "-q"]);
    for (source, reason) in [
        ("import os\nos . system('sh quality/hidden.txt')\n", "opaque interpreter program"),
        ("import os\nos.ｓｙｓｔｅｍ('sh quality/hidden.txt')\n", "opaque interpreter program"),
    ] {
        fs::write(workspace.path().join("script/check.py"), source).expect("qualified Python process call");
        git(workspace.path(), &["add", "."]);
        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(error.to_string().contains(reason), "{source}: {error:#}");
    }
}
