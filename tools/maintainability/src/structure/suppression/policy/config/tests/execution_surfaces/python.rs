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
    ]);
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
    ] {
        fs::write(workspace.path().join("script/check.py"), source).expect("safe Python writer text");
        git(workspace.path(), &["add", "."]);
        assert!(reject_checked_in_weakening(workspace.path()).is_ok(), "{source}");
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
