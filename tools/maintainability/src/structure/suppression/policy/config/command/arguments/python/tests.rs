use std::fs;
use std::path::Path;

use super::{has_opaque_filesystem_write, mutates_process_environment, mutates_process_working_directory};

fn has_opaque_process_arguments(source: &str) -> bool {
    super::has_opaque_process_arguments("script/check.py", source)
}

#[test]
fn explicit_line_continuations_cannot_hide_filesystem_writes() {
    assert!(has_opaque_filesystem_write("script/check.py", "Path(\"Justfile\") \\\n            .write_text(payload)\n"));
    assert!(has_opaque_filesystem_write(
        "script/check.py",
        "open(\\\n            file=\"Justfile\", \\\n            mode=\"w\")\n"
    ));
    assert!(has_opaque_filesystem_write("script/check.py", "Path(\"Justfile\") \\\r\n.write_bytes(payload)\r\n"));
}

#[test]
fn opaque_process_arguments_detect_executable_code_without_matching_inert_text() {
    assert!(has_opaque_process_arguments("subprocess.run([\"-\" \"A\"])\n"));
    assert!(has_opaque_process_arguments(r#"subprocess.run(["cargo", "clippy", "--", "\x2dA", "warnings"])"#));
    assert!(has_opaque_process_arguments(r#"subprocess.run(["cargo", "clippy", "--", b"\u002dA", "warnings"])"#));
    assert!(has_opaque_process_arguments(r#"subprocess.run(["cargo", "clippy", "--", chr(45) + "A", "warnings"])"#));
    assert!(has_opaque_process_arguments(
        "import subprocess\narguments = ['cargo', 'clippy']\nsubprocess.run(arguments)\n"
    ));
    assert!(has_opaque_process_arguments(
        "from subprocess import run\narguments = ['cargo', 'clippy']\nrun(arguments)\n"
    ));
    assert!(has_opaque_process_arguments(r#"os.execlp("cargo", "cargo", "clippy", "--", "-" + "A", "warnings")"#));
    assert!(has_opaque_process_arguments(
        r#"from os import execvpe
execvpe("cargo", ["cargo", "clippy", "--", "-" + "A", "warnings"], environment)"#
    ));
    assert!(has_opaque_process_arguments(
        r#"from os import system as run
run(bytes.fromhex("636172676f20636c69707079202d2d202d41207761726e696e6773"))"#
    ));
    assert!(has_opaque_process_arguments(
        r#"from os import system
system(bytes.fromhex("636172676f20636c69707079202d2d202d41207761726e696e6773"))"#
    ));
    assert!(has_opaque_process_arguments(
        r#"runner = __import__("sub" + "process")
runner.run(["cargo", "clippy", "--", chr(45) + "A", "warnings"])"#
    ));
    assert!(has_opaque_process_arguments(
        r#"runner = getattr(importlib.import_module("sub" + "process"), "r" + "un")
runner(["car" + "go", "clippy", "--", chr(45) + "A", "warnings"])"#
    ));
    assert!(has_opaque_process_arguments(
        r#"import ctypes
ctypes.CDLL(None).system(bytes.fromhex("636172676f20636c69707079202d2d202d41207761726e696e6773"))"#
    ));
    assert!(has_opaque_process_arguments(
        r#"from cffi import FFI
FFI().dlopen(None).system(bytes.fromhex("636172676f20636c69707079202d2d202d41207761726e696e6773"))"#
    ));
    assert!(has_opaque_process_arguments(
        r#"os.system(bytes.fromhex("636172676f20636c69707079202d2d202d41207761726e696e6773"))"#
    ));
    assert!(has_opaque_process_arguments(
        r#"posix.system(bytes.fromhex("636172676f20636c69707079202d2d202d41207761726e696e6773"))"#
    ));
    assert!(has_opaque_process_arguments(
        r#"posix.popen(bytes.fromhex("636172676f20636c69707079202d2d202d41207761726e696e6773"))"#
    ));
    assert!(has_opaque_process_arguments(
        r#"from posix import system as run
run(bytes.fromhex("636172676f20636c69707079202d2d202d41207761726e696e6773"))"#
    ));
    assert!(has_opaque_process_arguments(r#"os.system("printf safe; " + command)"#));
    assert!(has_opaque_process_arguments(r#"subprocess.run(bytes.fromhex("2f7573722f62696e2f636172676f"))"#));
    assert!(has_opaque_process_arguments("subprocess.Popen(command)"));
    assert!(has_opaque_process_arguments(
        "import subprocess\nsubprocess.run([\"git\", \"status\"])\nrunner = subprocess.run\nrunner(bytes.fromhex(\"636172676f\").decode(), shell=True)\n"
    ));
    assert!(!has_opaque_process_arguments(r#"subprocess.run(["cargo", "clippy", "--", r"\x2dA", "warnings"])"#));
    assert!(!has_opaque_process_arguments(
        r#"subprocess.run(["cargo", "metadata", "--locked"], cwd=repository, check=True)"#
    ));
    assert!(!has_opaque_process_arguments(r#"subprocess.run(["git", "show", f"{reference}:{source}"], check=False)"#));
    assert!(!has_opaque_process_arguments("message = f'{value:os.system(payload)}'"));
    assert!(!has_opaque_process_arguments(r#"message = f"{len("safe")}""#));
    assert!(!has_opaque_process_arguments(r#"subprocess.run([sys.executable, "script/check.py", value], check=True)"#));
    assert!(!has_opaque_process_arguments("from os import path\nprint(path.basename('/tmp/report'))"));
    assert!(!has_opaque_process_arguments("head = (f'<svg viewBox=\"0 0 64 64\" ' f'role=\"img\">')\n"));
    assert!(!has_opaque_process_arguments(
        "PATTERN = (r'^v[0-9]+' r'(?:-dev)?$')\nimport subprocess\nsubprocess.run(['git', 'status'])\n"
    ));
    assert!(!has_opaque_process_arguments("# import ctypes and run cargo\nprint('safe')\n"));
    assert!(!has_opaque_process_arguments(
        "\"\"\"getattr(importlib, 'run') and cargo are documentation only\"\"\"\nprint('safe')\n"
    ));
}

#[test]
fn opaque_process_bindings_fail_closed() {
    for source in [
        "from os import (\n    system,\n)\nsystem('sh quality/hidden.txt')\n",
        "import os\nprocess = os\nprocess.system('sh quality/hidden.txt')\n",
        "import json, subprocess as process\nprocess.run(['git'])\n",
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
        "import os\ndef f(): pass\ngetattr(f, '__globals__')['os'].system('sh quality/hidden.txt')\n",
        "import operator\noperator.attrgetter('__globals__')(f)['os'].system('sh quality/hidden.txt')\n",
        "evilmock.patch.object(PACKAGE.subprocess, 'Popen', return_value=compressor)\n",
        "unittest.mock.patch.object(PACKAGE.subprocess, 'Popen', return_value=compressor)\n",
        "with (\n    mock.patch.object(PACKAGE, 'write_tar'),\n    mock.patch.object(PACKAGE.subprocess, 'Popen', return_value=compressor) as popen,\n):\n    PACKAGE.write_tar_zst(stage, destination)\n",
    ] {
        assert!(has_opaque_process_arguments(source), "{source}");
    }

    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let path = "script/tests/test_cuda_release.py";
    let source = fs::read_to_string(workspace.join(path)).expect("read reviewed CUDA release test");
    assert!(!super::has_opaque_process_arguments(path, &source));
    assert!(super::has_opaque_process_arguments(path, &(source + "\n# changed\n")));
}

#[test]
fn stdlib_code_evaluators_fail_closed_without_matching_unrelated_names() {
    assert!(super::imports_command_capable_api("import cProfile as profiler\n"));
    for source in [
        "import code\ncode.InteractiveInterpreter().runsource(payload, symbol='exec')\n",
        "import code as repl\nrepl.InteractiveConsole().push(payload)\n",
        "from code import InteractiveInterpreter as Runner\nRunner().runcode(compiled)\n",
        "from code import interact\ninteract(local={})\n",
        "import site\nsite.addsitedir('quality')\n",
        "import site as paths\npaths.addpackage('quality', 'hidden.pth', set())\n",
        "from site import addsitedir\naddsitedir('quality')\n",
        "import timeit\ntimeit.Timer(payload).timeit(number=1)\n",
        "import trace\ntrace.Trace().run(payload)\n",
        "import profile\nprofile.run(payload)\n",
        "import cProfile as profiler\nprofiler.run(payload)\n",
        "import pdb\npdb.run(payload)\n",
        "import doctest\ndoctest.DocTestRunner().run(example)\n",
        "import shelve\nshelve.open('quality/db')['payload']\n",
        "import shelve as storage\nstorage.open('quality/db')['payload']\n",
        "from shelve import open as open_shelf\nopen_shelf('quality/db')['payload']\n",
        "from pickle import loads as decode\ndecode(payload)\n",
        "from marshal import loads as decode\ndecode(payload)\n",
        "from types import FunctionType\nFunctionType(code, globals())\n",
        "from gc import get_objects\nget_objects()\n",
        "from multiprocessing.reduction import ForkingPickler\nForkingPickler.loads(payload)\n",
        "import multiprocessing.reduction as reduction\nreduction.ForkingPickler.loads(payload)\n",
        "import multiprocessing\nmultiprocessing.reduction.ForkingPickler.loads(payload)\n",
        "import _operator\n_operator.itemgetter('label')(record)\n",
        "from _operator import attrgetter as field\nfield('label')(record)\n",
    ] {
        assert!(has_opaque_process_arguments(source), "{source}");
    }
    for source in [
        "import codecs\ncode = response.code\nprint(code)\n",
        "# import shelve\nprint('shelve.open is inert text')\n",
    ] {
        assert!(!has_opaque_process_arguments(source), "{source}");
    }
}

#[test]
fn python_identifier_normalization_cannot_hide_process_calls_or_aliases() {
    for source in [
        "import os\nos.ｓｙｓｔｅｍ(bytes.fromhex(payload))\n",
        "import ｏｓ\nｏｓ.system(bytes.fromhex(payload))\n",
        "import os as ｐｒｏｃｅｓｓ\nprocess.system('sh quality/hidden.txt')\n",
        "from ｏｓ import ｓｙｓｔｅｍ as run\nrun(bytes.fromhex(payload))\n",
    ] {
        assert!(has_opaque_process_arguments(source), "{source}");
    }
    assert!(!has_opaque_process_arguments("print('ｏｓ.ｓｙｓｔｅｍ is inert text')\n"));
}

#[test]
fn ambient_process_resolution_mutations_are_distinguished_from_reads() {
    assert!(mutates_process_working_directory("os.chdir('quality')\n"));
    assert!(mutates_process_working_directory("posix.fchdir(descriptor)\n"));
    assert!(mutates_process_environment("os.environ['Path'] = 'quality'\n"));
    assert!(mutates_process_environment("os.environ['PATH']: str = 'target/bin'\n"));
    assert!(mutates_process_environment("os.environ['PATH']: list[str] = ['target/bin']\n"));
    assert!(mutates_process_environment("os.environ[keys[0]]: str = 'target/bin'\n"));
    assert!(mutates_process_environment("os.environ[keys[0]] = 'target/bin'\n"));
    assert!(mutates_process_environment("os.environ['PATH']: list[str)\n"));
    assert!(mutates_process_environment("os.environ.update({'PATH': 'quality'})\n"));
    assert!(mutates_process_environment("del os.environ['PATH']\n"));
    assert!(mutates_process_environment("del (os.environ['PATH'])\n"));
    assert!(mutates_process_environment("del ((os.environ['PATH']))\n"));
    assert!(mutates_process_environment("del [os.environ['PATH'], os.environ['HOME']]\n"));
    assert!(mutates_process_environment("del(os.environ['PATH'])\n"));
    assert!(mutates_process_environment("if enabled: del (os.environ['PATH'])\n"));
    assert!(!mutates_process_environment("model = os.environ.get('PATH')\n"));
    assert!(!mutates_process_environment("previous = deleted; value = os.environ.get('PATH')\n"));
    assert!(mutates_process_environment("environment = os.environ\n"));
    assert!(mutates_process_environment("os . environ['Path'] = 'quality'\n"));
    assert!(!mutates_process_environment("value = os.environ.get('PATH')\nenvironment = os.environ.copy()\n"));
    assert!(!mutates_process_environment("os.environ['PATH']: type[Literal[1 == 1]]\n"));
    assert!(!has_opaque_process_arguments("value = os . environ.get('PATH')\n"));
}

#[test]
fn dynamic_process_module_lookup_and_general_reflection_fail_closed() {
    assert!(has_opaque_process_arguments("import os\nos.__dict__[\"sy\" + \"stem\"](bytes.fromhex(payload).decode())\n"));
    assert!(has_opaque_process_arguments("import os\ngetattr(os, name)(payload)\n"));
    assert!(has_opaque_process_arguments("import subprocess\nvars(subprocess)[name](payload)\n"));
    assert!(has_opaque_process_arguments("import os\nclose = getattr(self.container, 'close')\nprint(os.name)\n"));
}
