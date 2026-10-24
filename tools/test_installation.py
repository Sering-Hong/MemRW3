"""Windows setup smoke test in an isolated directory; restores existing shortcuts.

Refuses to run if MemRW3 is already registered as installed for this user.
Tests real install/upgrade/uninstall and a setup with a deliberately missing DLL.
"""
from pathlib import Path
import argparse
import ctypes
import json
import os
import shutil
import subprocess
import sys
import tempfile
import winreg
import time
from contextlib import contextmanager

from build_installer import REPO, inno_path

APP_KEY = r'Software\Microsoft\Windows\CurrentVersion\Uninstall\{59C6A927-5D9C-4E41-80B3-BC623BB3C81B}_is1'


def registered() -> bool:
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, APP_KEY, 0, winreg.KEY_READ | winreg.KEY_WOW64_64KEY):
            return True
    except FileNotFoundError:
        return False


def shell_folder(number: int) -> Path:
    buffer = ctypes.create_unicode_buffer(32768)
    if ctypes.windll.shell32.SHGetFolderPathW(None, number, None, 0, buffer):
        raise RuntimeError('Cannot resolve Windows shell folder')
    return Path(buffer.value)


@contextmanager
def test_directory():
    root = Path(tempfile.mkdtemp(prefix='MemRW3 安装测试-'))
    try:
        yield root
    finally:
        # Inno's detached final cleanup can briefly retain an uninstall log handle.
        for attempt in range(100):
            try:
                shutil.rmtree(root)
                break
            except FileNotFoundError:
                break
            except PermissionError:
                if attempt == 99:
                    raise
                time.sleep(0.1)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--iscc')
    args = parser.parse_args()
    if registered():
        raise RuntimeError('MemRW3 is already installed; smoke test will not modify that installation')
    setup, = (REPO/'dist/installer').glob('MemRW3-Setup-*-x64.exe')
    stage = REPO/'dist/installer-work/payload'
    manifest = stage/'runtime-dependencies.txt'
    original = manifest.read_bytes()
    desktop = shell_folder(0x10)/'MemRW3.lnk'
    menu = shell_folder(0x02)/'MemRW3'
    shortcuts = [desktop, menu/'MemRW3.lnk', menu/'安装说明.lnk', menu/'卸载 MemRW3.lnk']
    saved = {p: p.read_bytes() if p.exists() else None for p in shortcuts}
    with test_directory() as temp:
        target = temp/'app with spaces'
        negative_dir = temp/'negative-build'; negative_dir.mkdir()
        try:
            manifest.write_bytes(original+b'MemRW3MissingDependencyForTest.dll\n')
            try:
                subprocess.run([str(inno_path(args.iscc)), '/Qp', '/DPayloadDir='+str(stage), '/DOutputDir='+str(negative_dir), str(REPO/'installer/MemRW3.iss')], check=True, stdout=subprocess.DEVNULL)
            finally:
                manifest.write_bytes(original)
            bad, = negative_dir.glob('*.exe')
            result = subprocess.run([str(bad), '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/DIR='+str(target), '/LOG='+str(temp/'missing.log')], timeout=100)
            assert result.returncode != 0, 'Missing DLL was incorrectly accepted'
            assert not (target/'MemRW3.exe').exists(), 'Failed check copied application files'
            assert not registered(), 'Failed check registered an installation'
            report = Path(os.environ['LOCALAPPDATA'])/'MemRW3/install-dependencies.txt'
            assert 'MemRW3MissingDependencyForTest.dll' in report.read_text(encoding='utf-8-sig')
            print('PASS: missing dependency blocks setup before installing files', flush=True)
            command = [str(setup), '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/DIR='+str(target), '/LOG='+str(temp/'install.log')]
            subprocess.run(command, check=True, timeout=100)
            assert registered(), 'Uninstall registry entry missing'
            for name in ('MemRW3.exe', '安装说明.md', '使用说明.md', 'unins000.exe', 'assets/MemRW3-pink.ico'):
                assert (target/name).is_file(), name
            assert (target/'MemRW3.exe').read_bytes() == (stage/'MemRW3.exe').read_bytes()
            for p in shortcuts:
                assert p.is_file(), str(p)
            ps = Path(os.environ['SystemRoot'])/'System32/WindowsPowerShell/v1.0/powershell.exe'
            script = "[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); $s=New-Object -ComObject WScript.Shell; $l=$s.CreateShortcut('"+str(desktop).replace("'", "''")+"'); @{Target=$l.TargetPath;Icon=$l.IconLocation}|ConvertTo-Json"
            info = json.loads(subprocess.check_output([str(ps), '-NoProfile', '-Command', script], encoding='utf-8-sig'))
            assert Path(info['Target']) == target/'MemRW3.exe'
            assert str(target/'assets/MemRW3-pink.ico') in info['Icon']
            sentinel = target/'user-data.csv'; sentinel.write_text('keep user data', encoding='utf-8')
            source = target/'source/local-notes.txt'; source.parent.mkdir(); source.write_text('keep source', encoding='utf-8')
            subprocess.run(command, check=True, timeout=100)
            assert sentinel.read_text() == 'keep user data' and source.read_text() == 'keep source'
            print('PASS: install and upgrade, pink desktop/start menu shortcuts, uninstall entry, Unicode/spaced paths', flush=True)
            subprocess.run([str(target/'unins000.exe'), '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/LOG='+str(temp/'uninstall.log')], check=True, timeout=100)
            assert not (target/'MemRW3.exe').exists()
            assert not registered()
            assert sentinel.read_text() == 'keep user data' and source.read_text() == 'keep source'
            for p in shortcuts:
                assert not p.exists(), 'Uninstall left a test shortcut: '+str(p)
            print('PASS: uninstall removes owned files and preserves user data/source', flush=True)
        finally:
            manifest.write_bytes(original)
            if (target/'unins000.exe').exists():
                subprocess.run([str(target/'unins000.exe'), '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART'], timeout=100)
            for p, content in saved.items():
                if content is not None:
                    p.parent.mkdir(parents=True, exist_ok=True); p.write_bytes(content)
                elif p.exists():
                    p.unlink()
            if menu.exists() and not any(menu.iterdir()):
                menu.rmdir()
    print('Existing desktop/start menu shortcuts restored.', flush=True)


if __name__ == '__main__':
    sys.stdout.reconfigure(encoding='utf-8')
    main()
