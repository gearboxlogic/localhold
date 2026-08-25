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
    for source in [
        "subprocess.run([\"-\" \"A\"])\n",
        r#"subprocess.run(["cargo", "clippy", "--", "\x2dA", "warnings"])"#,
        r#"subprocess.run(["cargo", "clippy", "--", b"\u002dA", "warnings"])"#,
        r#"subprocess.run(["cargo", "clippy", "--", chr(45) + "A", "warnings"])"#,
        "import subprocess\narguments = ['cargo', 'clippy']\nsubprocess.run(arguments)\n",
        "from subprocess import run\narguments = ['cargo', 'clippy']\nrun(arguments)\n",
        r#"os.execlp("cargo", "cargo", "clippy", "--", "-" + "A", "warnings")"#,
        r#"from os import execvpe
execvpe("cargo", ["cargo", "clippy", "--", "-" + "A", "warnings"], environment)"#,
        r#"from os import system as run
run(bytes.fromhex("636172676f20636c69707079202d2d202d41207761726e696e6773"))"#,
        r#"from os import system
system(bytes.fromhex("636172676f20636c69707079202d2d202d41207761726e696e6773"))"#,
        r#"runner = __import__("sub" + "process")
runner.run(["cargo", "clippy", "--", chr(45) + "A", "warnings"])"#,
        r#"runner = getattr(importlib.import_module("sub" + "process"), "r" + "un")
runner(["car" + "go", "clippy", "--", chr(45) + "A", "warnings"])"#,
        r#"import ctypes
ctypes.CDLL(None).system(bytes.fromhex("636172676f20636c69707079202d2d202d41207761726e696e6773"))"#,
        r#"from cffi import FFI
FFI().dlopen(None).system(bytes.fromhex("636172676f20636c69707079202d2d202d41207761726e696e6773"))"#,
        r#"os.system(bytes.fromhex("636172676f20636c69707079202d2d202d41207761726e696e6773"))"#,
        r#"posix.system(bytes.fromhex("636172676f20636c69707079202d2d202d41207761726e696e6773"))"#,
        r#"posix.popen(bytes.fromhex("636172676f20636c69707079202d2d202d41207761726e696e6773"))"#,
        r#"from posix import system as run
run(bytes.fromhex("636172676f20636c69707079202d2d202d41207761726e696e6773"))"#,
        r#"os.system("printf safe; " + command)"#,
        r#"subprocess.run(bytes.fromhex("2f7573722f62696e2f636172676f"))"#,
        "subprocess.Popen(command)",
        "import subprocess\nsubprocess.run([\"git\", \"status\"])\nrunner = subprocess.run\nrunner(bytes.fromhex(\"636172676f\").decode(), shell=True)\n",
    ] {
        assert!(has_opaque_process_arguments(source), "{source}");
    }
    for source in [
        r#"subprocess.run(["cargo", "clippy", "--", r"\x2dA", "warnings"])"#,
        r#"subprocess.run(["cargo", "metadata", "--locked"], cwd=repository, check=True)"#,
        "message = f'{value:os.system(payload)}'",
        r#"message = f"{len("safe")}""#,
        r#"subprocess.run([sys.executable, "script/check.py", value], check=True)"#,
        "from os import path\nprint(path.basename('/tmp/report'))",
        "head = (f'<svg viewBox=\"0 0 64 64\" ' f'role=\"img\">')\n",
        "PATTERN = (r'^v[0-9]+' r'(?:-dev)?$')\nimport subprocess\nsubprocess.run(['git', 'status'])\n",
        "# import ctypes and run cargo\nprint('safe')\n",
        "\"\"\"getattr(importlib, 'run') and cargo are documentation only\"\"\"\nprint('safe')\n",
    ] {
        assert!(!has_opaque_process_arguments(source), "{source}");
    }
    let safe_git_show = format!(
        r#"subprocess.run(["git", "show", f"{{{reference}}}:{{{source}}}"], check=False)"#,
        reference = "reference",
        source = "source"
    );
    assert!(!has_opaque_process_arguments(&safe_git_show));
}

#[test]
fn rejected_execution_modules_fail_closed() {
    for source in [
        "import zipfile._path\n",
        "import imaplib\nimaplib.IMAP4_stream('sh quality/hidden.txt')\n",
        "from _aix_support import _read_cmd_output\n_read_cmd_output('sh quality/hidden.txt')\n",
        "from _osx_support import _read_output as run\nrun('sh quality/hidden.txt')\n",
        "from dataclasses import _FuncBuilder as Builder\nBuilder().add_fn('payload', '', '', '', '')\n",
        "from dataclasses import _create_fn\n_create_fn('run', [], ['print(1)'])\n",
        "import asyncio.unix_events as unix\nunix._UnixSubprocessTransport(loop, asyncio.SubprocessProtocol(), ['sh', 'quality/hidden.txt'], False, None, None, None, 0)\n",
        "import asyncio.windows_utils as windows\nwindows.Popen(['python', 'quality/hidden.txt'])\n",
        "import click\nclick.edit(filename='quality/hidden.txt', editor='sh')\n",
        "import http.cookiejar\nhttp.cookiejar.MozillaCookieJar('script/check.sh').save(ignore_discard=True)\n",
        "from imaplib import IMAP4_stream as stream\nstream('sh quality/hidden.txt')\n",
        "import mailcap\nmailcap.test()\n",
        "import pipes\npipeline = pipes.Template()\npipeline.append('sh quality/hidden.txt', '--')\npipeline.open_r('/dev/null').read()\n",
        "import pygments.lexers\npygments.lexers.load_lexer_from_file('quality/hidden.txt')\n",
        "import py_compile\npy_compile.compile('quality/hidden.txt', cfile='script/release.py', doraise=True)\n",
        "from _pyrepl.console import InteractiveColoredConsole\nInteractiveColoredConsole().runsource(payload)\n",
        "import typing\ntyping.get_type_hints(subject)\n",
        "import uuid\nuuid._get_command_stdout('sh', 'quality/hidden.txt')\n",
        "import wave\nwave.open('script/check.sh', 'wb').writeframes(payload)\n",
        "from wave import open as writer\nwriter('script/check.sh', 'wb').writeframes(payload)\n",
        "from xml.etree import ElementTree as ET\nET.ElementTree(root).write('script/check.sh', encoding='unicode', method='text')\n",
        "from typing import get_type_hints\nget_type_hints(subject)\n",
        "from uuid import _get_command_stdout as run\nrun('sh', 'quality/hidden.txt')\n",
        "import venv\nvenv.EnvBuilder()._call_new_python(context, 'quality/hidden.txt')\n",
    ] {
        assert!(has_opaque_process_arguments(source), "{source}");
    }
}

#[test]
fn opaque_process_bindings_fail_closed() {
    for source in [
        "from os import (\n    system,\n)\nsystem('sh quality/hidden.txt')\n",
        "from subprocess import Popen as launch\nlaunch(['sh', 'quality/hidden.txt'])\n",
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
        "import sys\nsys.path.insert(0, '.cache')\nimport payload\n",
        "from sys import path\npath.insert(0, '.cache')\nimport payload\n",
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
    for source in [
        "import code\ncode.InteractiveInterpreter().runsource(payload, symbol='exec')\n",
        "import annotationlib\nannotationlib.ForwardRef(payload).evaluate()\n",
        "import http.server\nhttp.server.CGIHTTPRequestHandler(*arguments)\n",
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
        "import tempfile\ntempfile._os.open('Justfile', flags)\n",
        "import tempfile as scratch\nscratch._io.FileIO('Justfile', 'w')\n",
        "from tempfile import _shutil as files\nfiles.copyfile('quality/hidden.txt', 'Justfile')\n",
    ] {
        assert!(has_opaque_process_arguments(source), "{source}");
    }
}

#[test]
fn stdlib_code_evaluator_lookalikes_remain_inert() {
    for source in [
        "import codecs\ncode = response.code\nprint(code)\n",
        "# import shelve\nprint('shelve.open is inert text')\n",
    ] {
        assert!(!has_opaque_process_arguments(source), "{source}");
    }
}

#[test]
fn native_stdlib_execution_escape_hatches_fail_closed() {
    for source in [
        "import sqlite3\nsqlite3.connect(':memory:').enable_load_extension(True)\n",
        "import _sqlite3 as database\ndatabase.connect(':memory:').load_extension('quality/payload')\n",
        "from sqlite3 import connect\nconnect(':memory:').load_extension('quality/payload')\n",
        "import dbm.sqlite3\ndatabase = dbm.sqlite3.open('quality/database')\ndatabase._cx.load_extension('quality/payload')\n",
        "from dbm import sqlite3 as database\ndatabase.open('quality/database')._cx.load_extension('quality/payload')\n",
        "connection.load_extension('quality/payload')\n",
        "loader = connection.load_extension\nloader('quality/payload')\n",
        "connection.setconfig(sqlite3.SQLITE_DBCONFIG_ENABLE_LOAD_EXTENSION, True)\nconnection.execute('select load_extension(?)', (payload,))\n",
        "getattr(connection, 'load_extension')('quality/payload')\n",
        "vars(connection)['load_extension']('quality/payload')\n",
        "connection.__dict__['load_extension']('quality/payload')\n",
        "connection.__getattribute__('load_extension')('quality/payload')\n",
        "import operator\noperator.attrgetter('load_extension')(connection)('quality/payload')\n",
        "import operator\noperator.methodcaller('load_extension', 'quality/payload')(connection)\n",
        "import tkinter\ntkinter.Tcl().call('exec', 'sh', 'quality/hidden.txt')\n",
        "import tkinter\ntkinter.Tcl().eval('exec sh quality/hidden.txt')\n",
        "import tkinter\ntkinter.Tcl().evalfile('quality/hidden.tcl')\n",
        "import tkinter\ntkinter.Tcl().call('source', 'quality/hidden.tcl')\n",
        "import tkinter\ntkinter.Tcl().call('open', '|sh quality/hidden.txt')\n",
        "import tkinter\ninterpreter = tkinter.Tcl()\ninvoke = interpreter.call\ninvoke('exec', 'sh', 'quality/hidden.txt')\n",
        "import _tkinter\nprint(_tkinter.TCL_VERSION)\n",
        "from tkinter import Tcl\nTcl().eval('exec sh quality/hidden.txt')\n",
        "import turtle\nturtle.TK.Tcl().call('exec', 'sh', 'quality/hidden.txt')\n",
        "import idlelib.pyshell as shell\nshell.Tcl().call('exec', 'sh', 'quality/hidden.txt')\n",
        "import turtledemo.__main__ as demo\ndemo.Tcl().call('exec', 'sh', 'quality/hidden.txt')\n",
        "import test.test_tcl as tests\ntests.Tcl().call('exec', 'sh', 'quality/hidden.txt')\n",
        "from test import test_tkinter\nprint(test_tkinter)\n",
        "import _xxsubinterpreters\nfrom pathlib import Path\n_xxsubinterpreters.run_string(_xxsubinterpreters.create(), Path('quality/hidden.txt').read_text())\n",
        "from _xxsubinterpreters import run_string as run\nrunner = run\nrunner.__call__(interpreter, payload)\n",
        "import _testcapi\n_testcapi.run_in_subinterp(payload)\n",
        "from _testcapi import run_in_subinterp as run\nrunner = run\nrunner(payload)\n",
        "import _testinternalcapi as api\nrunner = api.run_in_subinterp_with_config\nrunner(payload, config)\n",
        "from _testinternalcapi import exec_interpreter as run\nrun(interpreter, payload)\n",
        "import _testlimitedcapi as api\ncompiler = api.run_compilestring\nloader = api.PyImport_ExecCodeModule\ncode = compiler(payload, b'<payload>', 257)\nloader('_localhold_payload', code)\n",
        "from _ctypes import dlopen as load\nload('quality/payload')\n",
        "import _ctypes as native\nload = native.dlopen\nload('quality/payload')\n",
        "import _imp as loader\nloader.create_dynamic(spec)\n",
        "from _frozen_importlib_external import ExtensionFileLoader\nExtensionFileLoader(name, path).create_module(spec)\n",
        "import _frozen_importlib as loader\nprint(loader)\n",
        "__import__('_imp').create_dynamic(spec)\n",
        "import importlib\nimportlib.import_module('_ctypes').dlopen('quality/payload')\n",
        "from _interpreters import exec as run\nrun(interpreter, payload)\n",
        "import concurrent.interpreters\ninterpreter = concurrent.interpreters.create()\ninterpreter.exec(payload)\n",
        "from concurrent import interpreters\ninterpreter = interpreters.create()\ninterpreter.exec(payload)\n",
        "from concurrent import (\n    interpreters,\n)\ninterpreter = interpreters.create()\ninterpreter.exec(payload)\n",
        "from concurrent import (\n    interpreters,\n)\nprint(interpreters)\n",
        "import test.support.interpreters\ninterpreter = test.support.interpreters.create()\ninterpreter.exec(payload)\n",
        "from test.support import interpreters\ninterpreter = interpreters.create()\ninterpreter.exec(payload)\n",
        "import test.test__interpreters as tests\ntests._interpreters.run_string(tests._interpreters.create(), payload)\n",
        "import test.test_ttk as tests\ninterpreter = tests.tkinter.Tcl()\ninterpreter.call('exec', 'sh', 'quality/hidden.txt')\n",
        "import test.test_ttk_textonly as tests\ntests.ttk.tkinter.Tcl().call('exec', 'sh', 'quality/hidden.txt')\n",
    ] {
        assert!(has_opaque_process_arguments(source), "{source}");
    }
}

#[test]
fn native_stdlib_execution_lookalikes_remain_inert() {
    for source in [
        "print('sqlite3.connect is inert text')\n",
        "module_name = 'tkinter'\nprint(module_name)\n",
        "def run_string(value): return value\nprint(run_string('safe'))\n",
        "import concurrent.futures\nprint(concurrent.futures.ThreadPoolExecutor)\n",
        "# connection.load_extension('quality/payload')\nprint('load_extension is inert text')\n",
        "# import test.test_ttk\nprint('test.test__interpreters is inert text')\n",
        "# _testcapi.run_in_subinterp(payload)\ndef run_in_subinterp(value): return value\nprint(run_in_subinterp('safe'))\n",
        "print('_ctypes.dlopen is inert text')\n",
        "import _ctypes_helper\nprint(_ctypes_helper)\n",
        "import _ctypesashelper\nprint(_ctypesashelper)\n",
        "import _impact\nprint(_impact)\n",
        "import _impassembly\nprint(_impassembly)\n",
        "import _frozen_importlib_helper\nprint(_frozen_importlib_helper)\n",
        "from _ctypesimporter import safe\nprint(safe)\n",
        "from _impimporter import safe\nprint(safe)\n",
        "# import _imp\nprint('_frozen_importlib_external is inert text')\n",
        "def dlopened(value): return value\nprint(dlopened('safe'))\n",
        "def load_extensions(value): return value\nprint(load_extensions('safe'))\n",
    ] {
        assert!(!has_opaque_process_arguments(source), "{source}");
    }
}

#[test]
fn unconditional_process_module_imports_fail_closed() {
    for source in [
        "import _posixsubprocess\n_posixsubprocess.fork_exec(*arguments)\n",
        "import _posixsubprocess as native\nlaunch = native.fork_exec\nlaunch(*arguments)\n",
        "from _posixsubprocess import fork_exec as launch\nlaunch.__call__(*arguments)\n",
        "from _posixsubprocess import (\n    fork_exec,\n)\nfork_exec(*arguments)\n",
        "import _winapi\n_winapi.CreateProcess(*arguments)\n",
        "import _winapi as native\nlaunch = native.CreateProcess\nlaunch(*arguments)\n",
        "from _winapi import CreateProcess as launch\nlaunch(*arguments)\n",
        "from subprocess import _winapi as native\nnative.CreateProcess(*arguments)\n",
        "import asyncio.windows_utils as windows\nwindows._winapi.CreateProcess(*arguments)\n",
        "import pip\npip.main(arguments)\n",
        "import pip._internal as internal\ninternal.main(arguments)\n",
        "import pip._internal.cli.main as cli\nrunner = cli.main\nrunner.__call__(arguments)\n",
        "from pip._internal.cli.main import main as run\nrun(arguments)\n",
        "from pip . _internal . cli . main import main as run\nrun(arguments)\n",
        "from pip. \\\n+_internal.cli.main import main as run\nrun(arguments)\n",
        "from pip._internal.utils.entrypoints import _wrapper\n_wrapper(arguments)\n",
        "from pip._internal.commands import create_command\ncreate_command('wheel').main(arguments)\n",
        "from pip._internal.commands.wheel import WheelCommand\nWheelCommand('wheel', summary).main(arguments)\n",
        "import ensurepip\nensurepip._run_pip(arguments)",
        "from ensurepip import _run_pip as run\nrun(arguments)",
    ] {
        assert!(has_opaque_process_arguments(source), "{source}");
    }
}

#[test]
fn debugger_string_execution_imports_fail_closed() {
    for source in [
        "from bdb import Bdb\nBdb().run(payload)\n",
        "import bdb as debugger\nrunner = debugger.Bdb().run\nrunner.__call__(payload)\n",
        "from bdb import Bdb as Debugger\nDebugger().runeval(payload)\n",
        "import bdb\nbdb.Bdb().runctx(payload, globals(), locals())\n",
        "from bdb import Tdb\nTdb().run(payload)\n",
        "from bdb import *\nBdb().run(payload)\n",
    ] {
        assert!(has_opaque_process_arguments(source), "{source}");
    }
}

#[test]
fn debugger_string_execution_lookalikes_remain_inert() {
    for source in [
        "# import bdb\nprint('bdb.Bdb.run is inert text')\n",
        "import bdatabase\nprint(bdatabase)\n",
        "import bdbase\nprint(bdbase)\n",
        "from bdatabase import Bdb\nprint(Bdb)\n",
        "class Bdb:\n    def run(self, value): return value\nprint(Bdb().run('safe'))\n",
        "def bdb_run(value): return value\nprint(bdb_run('safe'))\n",
    ] {
        assert!(!has_opaque_process_arguments(source), "{source}");
    }
}

#[test]
fn subprocess_fork_exec_reexport_fails_closed() {
    for source in [
        "import subprocess\nsubprocess._fork_exec(*arguments)\n",
        "from subprocess import _fork_exec as launch\nlaunch(*arguments)\n",
        "import subprocess\nlaunch = subprocess._fork_exec\nlaunch.__call__(*arguments)\n",
        "import subprocess\ngetattr(subprocess, '_fork_exec')(*arguments)\n",
    ] {
        assert!(has_opaque_process_arguments(source), "{source}");
    }
}

#[test]
fn unconditional_process_module_lookalikes_remain_inert() {
    for source in [
        "# import pip\nprint('ensurepip and _posixsubprocess are inert text')\n",
        "# import _winapi\nprint('_winapi.CreateProcess is inert text')\n",
        "import pipeline\npipeline.main()\n",
        "from pipeline . helpers import main\nmain()\n",
        "import piper\nprint(piper)\n",
        "import pipassembly\nprint(pipassembly)\n",
        "import ensurepipeline\nprint(ensurepipeline)\n",
        "import _posixsubprocess_helper\nprint(_posixsubprocess_helper)\n",
        "import _posixsubprocessashelper\nprint(_posixsubprocessashelper)\n",
        "from _posixsubprocessimporter import fork_exec\nprint(fork_exec)\n",
        "import _winapi_helper\nprint(_winapi_helper)\n",
        "import _winapiashelper\nprint(_winapiashelper)\n",
        "from _winapiimporter import CreateProcess\nprint(CreateProcess)\n",
        "def pip_main(arguments): return arguments\nprint(pip_main([]))\n",
        "def fork_exec(arguments): return arguments\nprint(fork_exec([]))\n",
        "def CreateProcess(arguments): return arguments\nprint(CreateProcess([]))\n",
        "class Native:\n    def CreateProcesses(self, arguments): return arguments\nprint(Native().CreateProcesses([]))\n",
    ] {
        assert!(!has_opaque_process_arguments(source), "{source}");
    }
}

#[test]
fn logging_write_capabilities_fail_closed_at_the_module_boundary() {
    for source in [
        "import logging\nlogging.FileHandler('Justfile', mode='w')\n",
        "import logging as log\nlog.basicConfig(filename='Justfile')\n",
        "from logging import FileHandler\nFileHandler('Justfile')\n",
        "from logging import handlers\nhandlers.WatchedFileHandler('Justfile')\n",
        "import logging.handlers\nlogging.handlers.pickle.loads(payload)\n",
    ] {
        assert!(has_opaque_process_arguments(source), "{source}");
    }
    for source in [
        "# import logging\nprint('logging.FileHandler is inert text')\n",
        "import logging_helper\nprint(logging_helper)\n",
        "class FileHandler:\n    pass\nprint(FileHandler())\n",
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
