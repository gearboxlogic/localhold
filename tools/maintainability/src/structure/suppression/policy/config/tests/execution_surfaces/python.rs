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

    for source in [
        "from pathlib import Path\nPath(\"Justfile\").write_text(Path(\"quality/Justfile\").read_text())\n",
        "from pathlib import Path\ntarget = Path(\"Justfile\")\ntarget.write_text(Path(\"quality/Justfile\").read_text())\n",
        "import shutil\nsource = \"quality/Justfile\"\ntarget = \"Justfile\"\nshutil.copy2(source, target)\n",
        "with open(file=\"Justfile\", mode=\"w\") as output:\n    output.write(\"lint:\\n    true\\n\")\n",
        "import os\nos.write(descriptor, payload)\n",
    ] {
        fs::write(workspace.path().join("script/check.py"), source).expect("Python filesystem writer");
        let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("lint-weakening argument"), "{source}: {error:#}");
    }

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
fn command_policy_rejects_opaque_python_process_bindings() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    git(workspace.path(), &["init", "-q"]);
    for source in [
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
        "import asyncio\nasyncio.create_subprocess_exec('quality/hidden.py')\n",
        "import asyncio as loop\nloop.create_subprocess_shell('sh quality/hidden.txt')\n",
        "from asyncio import create_subprocess_exec\ncreate_subprocess_exec('quality/hidden.py')\n",
        "import os\nos.posix_spawn('quality/hidden.py', ['quality/hidden.py'], os.environ)\n",
        "from posix import posix_spawnp\nposix_spawnp('quality/hidden.py', ['quality/hidden.py'], {})\n",
        "import pty\npty.spawn(['quality/hidden.py'])\n",
        "from pty import spawn\nspawn(['quality/hidden.py'])\n",
        "import pty\nlaunch = pty.spawn\nlaunch(['quality/hidden.py'])\n",
        "import asyncio, subprocess\nasyncio.create_subprocess_exec('quality/hidden.py')\nsubprocess.run(['git', 'status'])\n",
        "import os, subprocess\nos.posix_spawn('quality/hidden.py', ['quality/hidden.py'], os.environ)\nsubprocess.run(['git', 'status'])\n",
        "from os import posix_spawn\nimport subprocess\nposix_spawn('quality/hidden.py', ['quality/hidden.py'], {})\nsubprocess.run(['git', 'status'])\n",
        "from posix import posix_spawnp\nimport subprocess\nposix_spawnp('quality/hidden.py', ['quality/hidden.py'], {})\nsubprocess.run(['git', 'status'])\n",
        "import posix, subprocess\nposix.posix_spawn('quality/hidden.py', ['quality/hidden.py'], {})\nsubprocess.run(['git', 'status'])\n",
        "import pty, subprocess\npty.spawn(['quality/hidden.py'])\nsubprocess.run(['git', 'status'])\n",
        "message = f\"{sys.modules['os'].system('sh quality/hidden.txt')}\"\n",
        "message = f\"{globals()['os'].system('sh quality/hidden.txt')}\"\n",
        "message = f\"{os.__getattribute__('system')('sh quality/hidden.txt')}\"\n",
    ] {
        fs::write(workspace.path().join("script/check.py"), source).expect("opaque Python process binding");
        git(workspace.path(), &["add", "."]);
        let Err(error) = reject_checked_in_weakening(workspace.path()) else {
            panic!("accepted opaque Python process binding: {source}");
        };
        assert!(error.to_string().contains("opaque interpreter program"), "{source}: {error:#}");
    }
}

#[test]
fn command_policy_tracks_whitespace_qualified_python_process_calls() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir_all(workspace.path().join("script")).expect("script directory");
    fs::write(workspace.path().join("script/check.py"), "import os\nos . system('sh quality/hidden.txt')\n").expect("whitespace-qualified Python process call");
    git(workspace.path(), &["init", "-q"]);
    git(workspace.path(), &["add", "."]);

    let error = reject_checked_in_weakening(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("tracked path inventory"), "{error:#}");
}
