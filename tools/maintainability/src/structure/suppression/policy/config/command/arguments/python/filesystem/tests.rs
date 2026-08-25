use super::has_opaque_write;

#[test]
fn filesystem_reflection_capabilities_fail_closed() {
    for source in [
        "from pathlib import Path\ngetattr(Path('target/input'), 'co' + 'py')('Justfile')\n",
        "from pathlib import Path\nname = f\"write_{kind}\"\ngetattr(Path('Justfile'), name)(payload)\n",
        "from pathlib import Path\ntarget = Path('Justfile')\nwriter = target.__getattribute__('write_text')\nwriter(payload)\n",
        "from pathlib import Path\nPath('Justfile').__getattribute__('unlink')()\n",
        "from pathlib import Path\n(Path('Justfile')).__getattribute__('unlink')()\n",
        "from pathlib import Path\n(Path).__dict__['copy'](Path('target/input'), 'Justfile')\n",
        "from pathlib import Path\nPath.__dict__['copy'](Path('target/input'), 'Justfile')\n",
        "from pathlib import Path\nPath.__dict__.get('write_text')(Path('Justfile'), payload)\n",
        "from pathlib import Path\nwriter = Path.__dict__.__getitem__('unlink')\nwriter(Path('Justfile'))\n",
        "from pathlib import Path\nPath.__mro__[0].__dict__['copy'](Path('target/input'), 'Justfile')\n",
        "from pathlib import Path\npath_type = Path.__mro__[0]\nwriter = path_type.__dict__.get('write_text')\nwriter(Path('Justfile'), payload)\n",
        "from pathlib import Path\npath_type = Path.__mro__.__getitem__(0)\nvars(path_type)['unlink'](Path('Justfile'))\n",
        "from pathlib import Path\nvars(Path)['move'](Path('target/input'), 'Justfile')\n",
        "from pathlib import Path\nobject.__getattribute__(Path('Justfile'), 'write_text')(payload)\n",
        "from pathlib import Path\ntype.__getattribute__(Path, 'copy')(Path('target/input'), 'Justfile')\n",
        "import os\ngetattr(os, 're' + 'move')('Justfile')\n",
        "import builtins\nfrom pathlib import Path\nbuiltins.getattr(Path('Justfile'), 'write_text')(payload)\n",
        "import shutil\nfiles = shutil\ncopy = vars(files)['copyfile']\ncopy('target/input', 'Justfile')\n",
        "import shutil\nfiles = shutil\ncopy = files.__dict__.get('copyfile')\ncopy('target/input', 'Justfile')\n",
        "from pathlib import Path\nfirst = target = Path('Justfile')\ngetattr(target, 'write_text')(payload)\n",
        "from pathlib import Path\nfirst = factory = Path\nvars(factory)['move'](Path('target/input'), 'Justfile')\n",
        "import shutil\nfirst = files = shutil\nvars(files)['copyfile']('target/input', 'Justfile')\n",
        "from pathlib import Path\nholders = [Path('Justfile')]\ntarget = holders[0]\ngetattr(target, 'write_text')(payload)\n",
        "from pathlib import Path\nfactories = {'path': Path}\nfactory = factories['path']\nvars(factory)['copy'](Path('target/input'), 'Justfile')\n",
        "import shutil\nmodules = (shutil,)\nfiles = modules[0]\nfiles.__dict__.__getitem__('move')('target/input', 'Justfile')\n",
        "import pathlib\nfactory = pathlib.Path\ntarget = factory('Justfile')\ngetattr(target, operation)(payload)\n",
        "import pathlib\nfactory = (pathlib.Path)\ntarget = factory('Justfile')\nvars(target)[operation](payload)\n",
        "import pathlib as paths\nmessage = f\"{getattr(paths.Path('Justfile'), operation)(payload)}\"\n",
        "from pathlib import Path\n[target] = [Path('Justfile')]\ngetattr(target, 'write_text')(payload)\n",
        "from pathlib import Path\ntarget, = (Path('Justfile'),)\nvars(target)['unlink']()\n",
        "from pathlib import Path\n(target := Path('Justfile'))\ngetattr(target, 'write_text')(payload)\n",
        "from pathlib import Path\nitems = []\nitems.append(Path)\nfactory = items.pop()\ngetattr(factory('Justfile'), 'write_text')(payload)\n",
        "from pathlib import Path\nitems = (Path for _ in range(1))\nfactory = next(items)\nvars(factory)['copy'](Path('input'), 'Justfile')\n",
        "from pathlib import Path\nholders = {'path': Path}\nfactory = holders.get('path')\ngetattr(factory('Justfile'), 'unlink')()\n",
        "from pathlib import Path\nwriter = (\n    Path.__mro__[0]\n).__dict__.get('write_text')\nwriter(Path('Justfile'), payload)\n",
        "from pathlib import Path\nvalues = [Path('target/report.txt').exists()]\nvalue = values[0]\ngetattr(value, 'bit_length')()\n",
        "import os\nvalue = os.getcwd()\nvalue = getattr(value, 'strip')()\n",
        "import os\nvalues = [os.getcwd()]\nvalue = values[0]\nvalue = getattr(value, 'strip')()\n",
        "from pathlib import Path\nvalues = [Path('target/report.txt').read_text()]\nvalue = values[0]\nvalue = getattr(value, 'strip')()\n",
        "from pathlib import Path\nPath\nrecord.__dict__.get('label')\n",
        "from builtins import getattr as inspect\nfrom pathlib import Path\ntarget = Path('Justfile')\ninspect(target, 'write_text')(payload)\n",
        "from pathlib import Path\nvalue = getattr(record, 'label')\n",
        "from os import getcwd as current_directory\nvalue = getattr(current_directory(), 'strip')()\n",
        "import operator\nfrom pathlib import Path\nwriter = operator.attrgetter('write_text')(Path)\nwriter(Path('Justfile'), payload)\n",
        "import operator\nfrom pathlib import Path\noperator.methodcaller('unlink')(Path('Justfile'))\n",
        "import inspect\nfrom pathlib import Path\nwriter = inspect.getattr_static(Path, 'write_text')\nwriter(Path('Justfile'), payload)\n",
        "import inspect\nfrom pathlib import Path\ndict(inspect.getmembers(Path))['unlink'](Path('Justfile'))\n",
        "import inspect\nfrom pathlib import Path\ndict(inspect.getmembers_static(Path))['write_text'](Path('Justfile'), payload)\n",
        "import operator as operations\nfrom pathlib import Path\noperations.attrgetter('unlink')(Path)(Path('Justfile'))\n",
        "import operator as operations\nfrom pathlib import Path\nwriter = (operations\n    # qualified reflection lookup\n    .attrgetter)('write_text')(Path)\nwriter(Path('Justfile'), payload)\n",
        "from operator import attrgetter as field\nfrom pathlib import Path\nfield('write_text')(Path)(Path('Justfile'), payload)\n",
        "from operator import methodcaller as invoke\nfrom pathlib import Path\ninvoke('write_text', payload)(Path('Justfile'))\n",
        "from inspect import getattr_static as lookup\nfrom pathlib import Path\nlookup(Path, 'unlink')(Path('Justfile'))\n",
        "from inspect import getmembers as fields\nimport os\ndict(fields(os))['remove']('Justfile')\n",
        "from inspect import getmembers_static as fields\nimport shutil\ndict(fields(shutil))['copyfile']('target/input', 'Justfile')\n",
        "import operator, shutil\ncopy = operator.attrgetter('copyfile')(shutil)\ncopy('target/input', 'Justfile')\n",
        "import operator\nfrom pathlib import Path\nmessage = f\"{operator.methodcaller(operation, payload)(Path('Justfile'))}\"\n",
        "import _operator\nfrom pathlib import Path\nwriter = _operator.attrgetter('write_text')(Path)\nwriter(Path('Justfile'), payload)\n",
        "import _operator\nfrom pathlib import Path\n_operator.methodcaller('unlink')(Path('Justfile'))\n",
        "import _operator as operations\nfrom pathlib import Path\noperations.attrgetter('unlink')(Path)(Path('Justfile'))\n",
        "from _operator import attrgetter as field\nfrom pathlib import Path\nfield('write_text')(Path)(Path('Justfile'), payload)\n",
        "from _operator import methodcaller as invoke\nfrom pathlib import Path\ninvoke('write_text', payload)(Path('Justfile'))\n",
        "import _operator, os\nremove = _operator.attrgetter('remove')(os)\nremove('Justfile')\n",
        "import dataclasses\nfrom pathlib import Path\nwriter = dataclasses.inspect.getattr_static(Path, 'write_text')\nwriter(Path('Justfile'), payload)\n",
        "import dataclasses as records\nfrom pathlib import Path\nrecords.inspect.getattr_static(Path, 'unlink')(Path('Justfile'))\n",
        "from dataclasses import inspect as introspection\nfrom pathlib import Path\nwriter = introspection.getattr_static(Path, 'write_text')\nwriter(Path('Justfile'), payload)\n",
        "import dataclasses\nfrom pathlib import Path\nattributes = dataclasses.inspect.classify_class_attrs(Path)\nwriter = next(item.object for item in attributes if item.name == 'write_text')\nwriter(Path('Justfile'), payload)\n",
        "import dataclasses as records\nfrom pathlib import Path\nnext(item.object for item in records.inspect.classify_class_attrs(Path) if item.name == 'unlink')(Path('Justfile'))\n",
        "from pathlib import Path\nvalue = helpers.getattr_static(record, 'label')\nPath('target/report.txt').read_text()\n",
    ] {
        assert!(has_opaque_write(source), "{source}");
    }
}

#[test]
fn non_filesystem_reflection_remains_allowed() {
    for source in [
        "value = getattr(record, 'label')\n",
        "value = getattr(record, 'la' + 'bel')\n",
        "value = getattr(record, f'{prefix}_label')\n",
        "value = getattr(record, field_name)\n",
        "value = vars(record)['label']\n",
        "value = record.__getattribute__('label')\n",
        "value = record.__dict__['label']\n",
        "alias = record\nvalue = vars(alias)[field_name]\n",
        "records = {'item': record}\nitem = records['item']\nvalue = item.__dict__.get('label')\n",
        "first = second = record\nvalue = vars(second)['label']\n",
        "PathLabel.__dict__.get('label')\n",
        "from builtins import getattr as inspect\nvalue = inspect(record, 'label')\n",
        "from builtins import vars as fields\nvalue = fields(record)['label']\n",
        "import operator as operations\nvalue = operations.attrgetter('label')(record)\n",
        "import operator as operations\nmodules = [operations]\n",
        "from operator import methodcaller as invoke\nvalue = invoke('strip')(' report ')\n",
        "import inspect as inspection\nvalue = inspection.getattr_static(record, 'label')\n",
        "import inspect as inspection\nvalue = inspection.signature(callback)\n",
        "import dataclasses\nattributes = dataclasses.inspect.classify_class_attrs(str)\n",
        "import dataclasses as records\nvalue = records.inspect.getattr_static(str, 'strip')\n",
        "from dataclasses import inspect as introspection\nvalue = introspection.getattr_static(str, 'strip')\n",
        "value = records.inspect.getattr_static(record, 'label')\n",
        "attributes = helpers.classify_class_attrs(record)\n",
        "value = helpers.attrgetter('label')(record)\n",
    ] {
        assert!(!has_opaque_write(source), "{source}");
    }
}

#[test]
fn filesystem_sources_without_reflection_remain_allowed() {
    for source in [
        "from pathlib import Path\nvalue = Path('target/report.txt').read_text()\n",
        "import os\nvalue = os.getcwd()\n",
        "import shutil\nshutil.copyfile('input.txt', 'target/report.txt')\n",
        "from pathlib import Path\nvalues = [Path('target/report.txt').exists()]\n",
        "from pathlib import Path\nprint('operator.attrgetter inspect.getattr_static')\n",
        "from pathlib import Path\n# operator.methodcaller('unlink')(Path('Justfile'))\nvalue = Path('target/report.txt').read_text()\n",
        "import inspect\nfrom pathlib import Path\nsignature = inspect.signature(callback)\nvalue = Path('target/report.txt').read_text()\n",
        "import operator\nfrom pathlib import Path\nlabel = operator.itemgetter('label')(record)\nvalue = Path('target/report.txt').read_text()\n",
        "import _operator\nfrom pathlib import Path\nlabel = _operator.itemgetter('label')(record)\nvalue = Path('target/report.txt').read_text()\n",
        "from pathlib import Path\nprint('_operator.attrgetter')\nvalue = Path('target/report.txt').read_text()\n",
        "from pathlib import Path\n# _operator.methodcaller('unlink')(Path('Justfile'))\nvalue = Path('target/report.txt').read_text()\n",
        "from pathlib import Path\nprint('dataclasses.inspect.getattr_static classify_class_attrs')\nvalue = Path('target/report.txt').read_text()\n",
        "from pathlib import Path\n# helpers.classify_class_attrs(Path)\nvalue = Path('target/report.txt').read_text()\n",
    ] {
        assert!(!has_opaque_write(source), "{source}");
    }
}

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
fn directory_selecting_copies_validate_the_implicit_child() {
    for source in [
        r#"shutil.copy("payload/config.toml", ".cargo")"#,
        r#"shutil.copy2("payload/check.py", "generated")"#,
        r#"shutil.move("payload/action.yml", ".github/actions/check")"#,
        r#"shutil.move("payload/report.txt", "target", copy_function=copy)"#,
        r#"shutil.copytree("payload", "target/tree")"#,
    ] {
        assert!(has_opaque_write(source), "{source}");
    }

    for source in [
        r#"shutil.copy("payload/report.txt", "target")"#,
        r#"shutil.copy2("payload/report.json", "target/data")"#,
        r#"shutil.move("payload/report.txt", "target")"#,
    ] {
        assert!(!has_opaque_write(source), "{source}");
    }
}

#[test]
fn archive_extraction_fails_closed() {
    for source in [
        "import shutil\nshutil.unpack_archive('quality/payload.tar', '.')\n",
        "from shutil import unpack_archive\nunpack_archive('quality/payload.zip', 'target')\n",
        "import shutil as files\nfiles.unpack_archive('quality/payload.tar', 'target')\n",
        "import tarfile\ntarfile.open('q/p.tgz').extractall('.')\n",
        "import tarfile\narchive = tarfile.open('q/p.tgz')\narchive.extract('Justfile')\n",
        "import tarfile as archives\narchives.TarFile.open('q/p.tgz').extractall('target')\n",
        "from tarfile import TarFile as Archive\nArchive.open('q/p.tgz').extract('Justfile')\n",
        "import zipfile\nzipfile.ZipFile('quality/payload.zip').extractall('target')\n",
        "import zipfile\narchive = zipfile.ZipFile('quality/payload.zip')\narchive.extract('Justfile')\n",
        "import zipfile as archives\narchives.ZipFile('quality/payload.zip').extractall('target')\n",
        "from zipfile import ZipFile as Archive\nArchive('quality/payload.zip').extract('Justfile')\n",
        "import tarfile\narchive = tarfile.open('q/p.tgz')\nextract = archive.extractall\nextract('target')\n",
        "import shutil\nextract = shutil.unpack_archive\nextract('quality/payload.zip', 'target')\n",
        "import zipfile\narchive = zipfile.ZipFile('quality/payload.zip')\narchive.extractall.__call__('target')\n",
        "import zipfile\ngetattr(zipfile.ZipFile('quality/payload.zip'), 'extractall')('target')\n",
        "import tarfile\ngetattr(tarfile.open('q/p.tgz'), 'extractall')('target')\n",
    ] {
        assert!(has_opaque_write(source), "{source}");
    }

    for source in [
        "import tarfile\ntarfile.open('q/p.tgz').extractall('target'\n",
        "import zipfile\nzipfile.ZipFile('quality/payload.zip').extract('Justfile'\n",
    ] {
        assert!(has_opaque_write(source), "malformed archive extraction: {source}");
    }

    for source in [
        "message = \"zipfile.ZipFile('payload.zip').extractall('.')\"\n",
        "archive.extractfile('report.txt')\n",
        "import tarfile\ntarfile.open('q/p.tgz').extractfile('report.txt')\n",
        "import zipfile\nzipfile.ZipFile('quality/payload.zip').open('report.txt', mode='r')\n",
    ] {
        assert!(!has_opaque_write(source), "safe archive lookalike: {source}");
    }
}

#[test]
fn application_archive_creation_obeys_the_write_policy() {
    for source in [
        "import zipapp\nzipapp.create_archive('quality/payload', 'script/check.py')\n",
        "import zipapp as apps\napps.create_archive('quality/payload', target='Justfile')\n",
        "from zipapp import create_archive as pack\npack('script/check', None)\n",
        "import zipfile\nzipfile.ZipFile('script/check.py', 'w').writestr('__main__.py', payload)\n",
        "import zipfile as archives\narchives.ZipFile(file='Justfile', mode='x')\n",
        "from zipfile import PyZipFile as Archive\nArchive('script/check.py', 'a')\n",
        "import zipfile\nfactory = zipfile.ZipFile\nfactory('script/check.py', mode)\n",
        "import zipapp\nzipapp.main(arguments)\n",
        "import zipfile\nzipfile.main(arguments)\n",
        "import shutil\nshutil.make_archive('script/check', 'zip', 'quality')\n",
        "from zipfile import _path as private\nprivate.CompleteDirs('script/check.py', 'w')\n",
        "import zipapp\ngetattr(zipapp, operation)('quality/payload', 'script/check.py')\n",
    ] {
        assert!(has_opaque_write(source), "{source}");
    }

    for source in [
        "import zipfile\nzipfile.ZipFile('script/check.py')\n",
        "import zipfile\nzipfile.ZipFile('script/check.py', 'r').namelist()\n",
        "import zipfile\nzipfile.PyZipFile('script/check.py', mode='r').testzip()\n",
        "import zipfile\nzipfile.ZipFile('target/report.zip', 'w').writestr('report.txt', payload)\n",
        "import zipapp\nzipapp.create_archive('quality/payload', 'target/app.pyz')\n",
        "import zipapp\nzipapp.get_interpreter('script/check.py')\n",
        "import zipapp_helper\nzipapp_helper.create_archives('target')\n",
        "class ZipFiles:\n    pass\nprint(ZipFiles())\n",
    ] {
        assert!(!has_opaque_write(source), "{source}");
    }
}

#[test]
fn fileinput_inplace_writes_fail_closed() {
    for source in [
        "import fileinput\nfileinput.input('Justfile', inplace=True)\n",
        "from fileinput import input as lines\nlines('Justfile', True)\n",
        "import fileinput as fi\nfi.input('Justfile', inplace=True)\n",
        "from fileinput import FileInput\nFileInput(files='Justfile', inplace=enabled)\n",
        "from fileinput import FileInput as Reader\nReader(files='Justfile', inplace=True)\n",
        "import fileinput\nfileinput.input.__call__('Justfile', True)\n",
        "import fileinput\nfileinput.FileInput.__call__(files='Justfile', inplace=enabled)\n",
        "import fileinput\nfileinput.input('Justfile', enabled)\n",
        "import fileinput\nwriter = fileinput.input\nwriter('Justfile', inplace=True)\n",
        "import fileinput\ngetattr(fileinput, 'input')('Justfile', inplace=True)\n",
        "import fileinput\nfileinput.__dict__['input']('Justfile', inplace=True)\n",
        "import fileinput\nfileinput.__getattribute__('input')('Justfile', inplace=True)\n",
        "import fileinput\nvars(fileinput)['FileInput']('Justfile', inplace=True)\n",
        "import fileinput\nfileinput.input('Justfile', **options)\n",
        "import fileinput\nfileinput.input(*arguments)\n",
        "import fileinput\nfileinput.input('Justfile', inplace=True\n",
        "import fileinput as fi\nfi.input('Justfile', inplace=True\n",
    ] {
        assert!(has_opaque_write(source), "{source}");
    }
    for source in [
        "import fileinput\nfor line in fileinput.input('input.txt'): print(line)\n",
        "import fileinput as fi\nfor line in fi.input('input.txt'): print(line)\n",
        "import fileinput\nfileinput.input('input.txt', inplace=False)\n",
        "from fileinput import FileInput\nFileInput('input.txt', inplace=0)\n",
        "from fileinput import FileInput as Reader\nReader(files='input.txt', inplace=False)\n",
        "import fileinput\nfileinput.input.__call__('input.txt', False)\n",
        "import fileinput\nfileinput.input('target/report.txt', inplace=True)\n",
        "import fileinput\nfileinput.input('target/report.txt', inplace=enabled)\n",
    ] {
        assert!(!has_opaque_write(source), "{source}");
    }
}

#[test]
fn copying_symlinks_and_unpacked_writer_arguments_fail_closed() {
    for source in [
        r#"shutil.copy("payload/link", "target", follow_symlinks=False)"#,
        r#"shutil.copy2("payload/link", "target", follow_symlinks=setting)"#,
        r#"shutil.copyfile("payload/link", "target/link", follow_symlinks=None)"#,
        "open(*arguments)\n",
        "open(  # forwarded writer\n    *arguments\n)\n",
        "io.open(*arguments)\n",
        "open('target/report.txt', *arguments)\n",
        "os.fdopen(*arguments)\n",
        "Path('target/report.txt').open(*arguments)\n",
        "shutil.copyfile(*arguments)\n",
        "import tempfile\ntempfile.NamedTemporaryFile(*arguments)\n",
        "import tempfile\ntempfile.NamedTemporaryFile(**options)\n",
        "import tempfile\ntempfile.NamedTemporaryFile(\n    # forwarded options\n    **options\n)\n",
    ] {
        assert!(has_opaque_write(source), "{source}");
    }

    for source in [
        r#"shutil.copy("payload/report.txt", "target")"#,
        r#"shutil.copy("payload/report.txt", "target", follow_symlinks=True)"#,
        r#"shutil.copy("payload/report.txt", "target", follow_symlinks=(True))"#,
        r#"shutil.copy2("payload/report.txt", "target", follow_symlinks=True)"#,
        r#"shutil.copyfile("payload/report.txt", "target/report.txt", follow_symlinks=True)"#,
    ] {
        assert!(!has_opaque_write(source), "{source}");
    }
}

#[test]
fn filesystem_call_comments_preserve_argument_semantics() {
    for source in [
        "open(\n    'Justfile',\n    # positional mode follows\n    'w'  # positional mode ends\n).write(payload)\n",
        "open(\n    file='Justfile',\n    # keyword mode follows\n    mode='w'  # keyword mode ends\n).write(payload)\n",
        "from pathlib import Path\nPath('Justfile').open(  # positional mode follows\n    'w'  # final positional\n)\n",
        "import os\nos.fdopen(descriptor,  # positional mode follows\n    'w'  # final positional\n)\n",
        "import shutil\nshutil.copy('payload/link', 'target',\n    follow_symlinks=False  # preserve the link\n)\n",
        "import os\nos.remove('report.txt',\n    dir_fd=directory  # rebased destination\n)\n",
        "open('target/report.txt', 'w',\n    opener=custom_opener  # custom destination resolution\n)\n",
        "import tempfile\ntempfile.NamedTemporaryFile(dir='target',\n    prefix='../script/check-'  # traversing prefix\n)\n",
        "import tempfile\ntempfile.NamedTemporaryFile(dir='target',\n    suffix='.rs'  # protected suffix\n)\n",
        "import tempfile\ntempfile.NamedTemporaryFile(\n    dir='script'  # protected directory\n)\n",
    ] {
        assert!(has_opaque_write(source), "{source}");
    }

    for source in [
        "open('Justfile',\n    # read mode follows\n    'rb'  # final positional\n)\n",
        "open(file='target/report.txt',\n    # safe keyword follows\n    mode='w'  # final keyword\n)\n",
        "from pathlib import Path\nPath('target/report.txt').open(  # safe positional mode follows\n    'w'  # final positional\n)\n",
        "import os\nos.fdopen(descriptor,  # read mode follows\n    'rb'  # final positional\n)\n",
        "import shutil\nshutil.copy('payload/report.txt', 'target',\n    follow_symlinks=True  # ordinary copy\n)\n",
        "import os\nos.remove('target/report.txt',\n    dir_fd=None  # no rebasing\n)\n",
        "open('target/report.txt', 'w',\n    opener=None  # standard opener\n)\n",
        "import tempfile\ntempfile.NamedTemporaryFile(prefix='report-', suffix='.txt',\n    dir='target'  # explicit safe directory\n)\n",
    ] {
        assert!(!has_opaque_write(source), "{source}");
    }
}

#[test]
fn malformed_modeled_filesystem_calls_fail_closed() {
    for source in [
        "open('target/report.txt', 'w'",
        "from pathlib import Path\nPath('target/report.txt').open('w'",
        "import os\nos.fdopen(descriptor, 'rb'",
        "import shutil\nshutil.copy('payload/report.txt', 'target'",
        "import tempfile\ntempfile.NamedTemporaryFile(dir='target'",
        "open('target/report.txt', 'w)",
        "open('target/report.txt', 'w'])",
    ] {
        assert!(has_opaque_write(source), "{source}");
    }

    assert!(!has_opaque_write("print('ordinary call')\n"));
}

#[test]
fn temporary_files_require_explicit_literal_directories() {
    for source in [
        "import tempfile\ntempfile.NamedTemporaryFile()\n",
        "import tempfile\ntempfile.NamedTemporaryFile(dir=None)\n",
        "import tempfile\ntempfile.NamedTemporaryFile(prefix='report-', suffix='.txt')\n",
        "import tempfile\ntempfile.NamedTemporaryFile(dir='target', prefix=r'\\script\\check-', suffix='.txt')\n",
        "import tempfile\ntempfile.NamedTemporaryFile(dir='target', prefix=r'\\\\server\\share\\check-', suffix='.txt')\n",
        "import tempfile\ntempfile.NamedTemporaryFile(dir='target', prefix=r'C:\\script\\check-', suffix='.txt')\n",
    ] {
        assert!(has_opaque_write(source), "{source}");
    }
    assert!(!has_opaque_write(
        "import tempfile\ntempfile.NamedTemporaryFile(dir='target', prefix='report-', suffix='.txt')\n"
    ));
    assert!(!has_opaque_write(
        "import tempfile\ntempfile.NamedTemporaryFile(dir='target', prefix=r'reports\\check-', suffix='.txt')\n"
    ));
}

#[test]
fn direct_filesystem_writers_fail_closed() {
    for source in [
        r#"Path("Justfile").write_text(Path("quality/Justfile").read_text())"#,
        r#"pathlib.Path("script/check.sh").write_bytes(payload)"#,
        r#"Path("Justfile").open("w")"#,
        r#"open("Justfile", "w")"#,
        r#"open(mode="a", file=".github/workflows/ci.yml")"#,
        r#"codecs.open("Justfile", "w").write(payload)"#,
        r#"codecs.open(filename="script/check.sh", mode="a").write(payload)"#,
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
        r#"Path("Justfile").write_text.__call__(payload)"#,
        r#"open.__call__("Justfile", "w").write(payload)"#,
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
        "writer: Callable[[str], int] = open\n",
        "first = second = open\n",
        "holder.writer = open\n",
        "def invoke(opener=open):\n    return opener\n",
        "container = [open]\nopener = container[0]\nopener('target/report.txt', 'w')\n",
        "[opener] = [open]\nopener('target/report.txt', 'w')\n",
        "opener = [open][0]\nopener('target/report.txt', 'w')\n",
        "from functools import partial\nopener = partial(open, 'target/report.txt', 'w')\n",
        "from functools import partial\nwriter = partial(Path('Justfile').write_text, encoding='utf-8')\n",
        "remover = os.unlink\nremover('target/report.txt')\n",
        "mover = shutil.move\nmover('target/input.txt', 'target/output.txt')\n",
        "writer = Path('Justfile').unlink\nwriter()\n",
        "writer = Path.rename\nwriter(Path('target/input.txt'), 'target/output.txt')\n",
        "import tempfile\ntempfile.NamedTemporaryFile(dir='script')\n",
        "import tempfile\ntempfile.NamedTemporaryFile(dir='.github/workflows', suffix='.yml')\n",
        "import tempfile\ntempfile.NamedTemporaryFile(dir=destination)\n",
        "import tempfile\nfactory = tempfile.NamedTemporaryFile\nfactory(dir='target', suffix='.txt')\n",
    ] {
        assert!(has_opaque_write(source), "{source}");
    }
}

#[test]
fn aliases_and_composed_writer_paths_fail_closed() {
    for source in [
        "import shutil as files\nfiles.copyfile('quality/Justfile', 'Justfile')\n",
        "from shutil import copyfile as copy\ncopy('quality/Justfile', 'Justfile')\n",
        "from shutil import (\n    copy2 as copy,\n)\ncopy('quality/Justfile', 'Justfile')\n",
        "import tempfile as scratch\nscratch.NamedTemporaryFile(dir='script', suffix='.sh')\n",
        "from os import remove as erase\nerase('Justfile')\n",
        "from os import pwrite as writer\nwriter(descriptor, payload, 0)\n",
        "import shutil\nfiles = shutil\nfiles.copyfile('quality/Justfile', 'Justfile')\n",
        "import shutil as files\nstored = files\n",
        "from shutil import *\ncopyfile('quality/Justfile', 'Justfile')\n",
        "if enabled: import shutil as files\nfiles.copyfile('quality/Justfile', 'Justfile')\n",
        "from pathlib import Path as FilePath\nFilePath('Justfile').write_text(payload)\n",
        "import tempfile\ntempfile.NamedTemporaryFile(dir='target', prefix='../script/check-', suffix='.sh', delete=False)\n",
        "import tempfile\ntempfile.NamedTemporaryFile(prefix='../script/check-', suffix='.sh', delete=False)\n",
        "import tempfile\ntempfile.NamedTemporaryFile(dir='target', prefix='../' + 'script/check-', suffix='.sh', delete=False)\n",
        "import tempfile\ntempfile.NamedTemporaryFile(prefix=prefix, suffix='.sh', delete=False)\n",
        "open('Just' 'file', 'w').write(payload)\n",
        "Path('Just' 'file').write_text(payload)\n",
        "open('Just' + 'file', 'w').write(payload)\n",
        "Path('Just' + 'file').write_text(payload)\n",
        "open('target/' + name, 'w').write(payload)\n",
        "import shutil as files\nmessage = f\"{files.copyfile('quality/Justfile', 'Justfile')}\"\n",
        "from shutil import copyfile as copy\nmessage = f\"{copy('quality/Justfile', 'Justfile')!s}\"\n",
        "from shutil import copyfile as copy\nmessage = f\"{value:{copy('quality/Justfile', 'Justfile')}}\"\n",
        "from shutil import copyfile as copy\nmessage = f\"{f'{copy(\"quality/Justfile\", \"Justfile\")}'}\"\n",
        "import tempfile as scratch\nmessage = f\"{scratch.NamedTemporaryFile(dir='script', suffix='.sh')}\"\n",
        "import shutil as files\nmessage = f\"{files}\"\n",
        "import shutil as files\nmessage = f\"{(files := helper).copyfile('quality/Justfile', 'Justfile')}\"\n",
        "import _io as streams\nstreams.open('Justfile', 'w').write(payload)\n",
        "from _io import open as writer\nwriter('Justfile', 'w').write(payload)\n",
        "import _pyio as streams\nstreams.open('Justfile', 'w').write(payload)\n",
        "from _pyio import open as writer\nwriter('Justfile', 'w').write(payload)\n",
        "import codecs as streams\nstreams.open('Justfile', 'w').write(payload)\n",
        "from codecs import open as writer\nwriter('Justfile', 'w').write(payload)\n",
        "from posix import remove as erase\nerase('Justfile')\n",
        "import nt as backend\nbackend.remove('Justfile')\n",
        "open('MAKEFILE', 'w').write(payload)\n",
        "open('PACKAGE.JSON', 'w').write(payload)\n",
        "open('.GITHUB/workflows/ci.yml', 'w').write(payload)\n",
        "open('Justfile.', 'w').write(payload)\n",
        "open('Justfile ', 'w').write(payload)\n",
        "open('Justfile:$DATA', 'w').write(payload)\n",
        "import tempfile\ntempfile.NamedTemporaryFile(dir='.GITHUB/workflows', suffix='.yml', delete=False)\n",
    ] {
        assert!(has_opaque_write(source), "{source}");
    }
}

#[test]
fn python_314_path_tree_mutators_fail_closed() {
    for source in [
        "source.copy('Justfile')\n",
        "Path.copy(Path('payload/replacement'), 'Justfile')\n",
        "Path('payload/replacement').copy('Justfile')\n",
        "Path('payload/config.toml').copy_into('.cargo')\n",
        "Path('payload/replacement').move('Justfile')\n",
        "Path('payload/action.yml').move_into('.github/workflows')\n",
        "writer = Path('payload/replacement').copy\nwriter('Justfile')\n",
        "writer = Path.copy\nwriter(Path('payload/replacement'), 'Justfile')\n",
        "from pathlib import Path as FilePath\nFilePath('payload/replacement').move('Justfile')\n",
    ] {
        assert!(has_opaque_write(source), "{source}");
    }

    // Recursive trees, symlinks, and implicit destination basenames are not
    // modeled yet, so even apparently safe data-path invocations fail closed.
    for source in [
        "Path('payload/report.txt').copy('target/report.txt')\n",
        "Path('payload/report.txt').copy_into('target')\n",
        "Path('payload/report.txt').move('target/report.txt')\n",
        "Path('payload/report.txt').move_into('target')\n",
    ] {
        assert!(has_opaque_write(source), "conservative Python 3.14 mutator: {source}");
    }
}

#[test]
fn modeled_mutator_capabilities_fail_closed() {
    for capability in [
        "open",
        "builtins.open",
        "codecs.open",
        "io.open",
        "os.copy_file_range",
        "os.fchmod",
        "os.fchown",
        "os.ftruncate",
        "os.pwrite",
        "os.pwritev",
        "os.sendfile",
        "os.write",
        "os.writev",
        "os.fdopen",
        "os.open",
        "os.chmod",
        "os.chown",
        "os.lchown",
        "os.link",
        "os.makedirs",
        "os.mkdir",
        "os.remove",
        "os.removedirs",
        "os.rename",
        "os.renames",
        "os.replace",
        "os.rmdir",
        "os.symlink",
        "os.truncate",
        "os.unlink",
        "os.utime",
        "shutil.copy",
        "shutil.copy2",
        "shutil.copyfile",
        "shutil.copytree",
        "shutil.move",
        "shutil.rmtree",
        "shutil.unpack_archive",
        "tempfile.NamedTemporaryFile",
    ] {
        let source = format!("stored = {capability}\n");
        assert!(has_opaque_write(&source), "{capability}");
    }
    for method in [
        "chmod",
        "copy",
        "copy_into",
        "extract",
        "extractall",
        "hardlink_to",
        "lchmod",
        "mkdir",
        "move",
        "move_into",
        "open",
        "rename",
        "replace",
        "rmdir",
        "symlink_to",
        "touch",
        "unlink",
        "write_bytes",
        "write_text",
    ] {
        let source = format!("stored = target.{method}\n");
        assert!(has_opaque_write(&source), "{method}");
    }
}

#[test]
fn filesystem_copies_to_data_paths_remain_allowed() {
    assert!(!has_opaque_write(r#"shutil.copyfile("quality/report.txt", "target/report.txt")"#));
    assert!(!has_opaque_write(r#"print('shutil.copyfile("a", "Justfile")')"#));
    assert!(!has_opaque_write(r#"Path("target/report.txt").write_text(report)"#));
    assert!(!has_opaque_write(r#"open("target/report.txt", "wb")"#));
    assert!(!has_opaque_write(r#"codecs.open("target/report.txt", "wb")"#));
    assert!(!has_opaque_write(r#"codecs.open("Justfile", "rb")"#));
    assert!(!has_opaque_write(r#"(open)("target/report.txt", "wb")"#));
    assert!(!has_opaque_write(r#"(Path("target/report.txt").write_text)(report)"#));
    assert!(!has_opaque_write(r#"Path("target/report.txt").write_text.__call__(report)"#));
    assert!(!has_opaque_write(r#"open.__call__("target/report.txt", "w").write(report)"#));
    assert!(!has_opaque_write("writer = Path('target/report.txt').write_text\nwriter(report)\n"));
    assert!(!has_opaque_write("writer: Callable[[str], int] = Path('target/report.txt').write_text\nwriter(report)\n"));
    assert!(!has_opaque_write("container = [Path('target/report.txt').unlink]\ncontainer[0]()\n"));
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
    assert!(!has_opaque_write("import tempfile\ntempfile.NamedTemporaryFile(dir='target', suffix='.txt')\n"));
    assert!(!has_opaque_write("import shutil as files\nfiles.copyfile('quality/report.txt', 'target/report.txt')\n"));
    assert!(!has_opaque_write("from shutil import copyfile as copy\ncopy('quality/report.txt', 'target/report.txt')\n"));
    assert!(!has_opaque_write("from os import remove as erase\nerase('target/report.txt')\n"));
    assert!(!has_opaque_write("import tempfile as scratch\nscratch.NamedTemporaryFile(dir='target', suffix='.txt')\n"));
    assert!(!has_opaque_write(
        "from pathlib import Path as FilePath\nFilePath('target/report.txt').write_text(payload)\n"
    ));
    assert!(!has_opaque_write("open('target/' 'report.txt', 'w').write(payload)\n"));
    assert!(!has_opaque_write("Path('target/' + 'report.txt').write_text(payload)\n"));
    assert!(!has_opaque_write("import shutil as files\nprint('files.copyfile is documentation')\n"));
    assert!(!has_opaque_write(
        "import shutil as files\nmessage = f\"{files.copyfile('quality/report.txt', 'target/report.txt')}\"\n"
    ));
    assert!(!has_opaque_write(
        "from shutil import copyfile as copy\nmessage = f\"{copy('quality/report.txt', 'target/report.txt')!s}\"\n"
    ));
    assert!(!has_opaque_write(
        "from shutil import copyfile as copy\nmessage = f\"{value:{copy('quality/report.txt', 'target/report.txt')}}\"\n"
    ));
    assert!(!has_opaque_write(
        "from shutil import copyfile as copy\nmessage = f\"{f'{copy(\"quality/report.txt\", \"target/report.txt\")}'}\"\n"
    ));
    assert!(!has_opaque_write(
        "import tempfile as scratch\nmessage = f\"{scratch.NamedTemporaryFile(dir='target', suffix='.txt')}\"\n"
    ));
    assert!(!has_opaque_write("from _io import open as writer\nmessage = f\"{writer('target/report.txt', 'w')}\"\n"));
    assert!(!has_opaque_write("from posix import remove as erase\nmessage = f\"{erase('target/report.txt')}\"\n"));
    assert!(!has_opaque_write("import shutil as files\nmessage = f\"files.copyfile is inert text\"\n"));
    assert!(!has_opaque_write(
        "import tempfile\ntempfile.NamedTemporaryFile(dir='target', prefix='report-' + 'safe-', suffix='.txt')\n"
    ));
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

#[test]
fn platform_writer_alias_controls_remain_allowed() {
    assert!(!has_opaque_write("from _pyio import open as writer\nmessage = f\"{writer('target/report.txt', 'w')}\"\n"));
    assert!(!has_opaque_write(r#"open("target/MAKEFILE.txt", "w")"#));
    assert!(!has_opaque_write(r#"open(".github/workflow/ci.yml", "w")"#));
    assert!(!has_opaque_write("import tempfile\ntempfile.NamedTemporaryFile(dir='.GITHUB/artifacts', suffix='.yml')\n"));
}
