"""Build a Windows x64 setup EXE. Uses Python stdlib, Inno Setup 6.5+ and a C compiler."""
from __future__ import annotations

import argparse
import hashlib
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys

REPO = Path(__file__).resolve().parents[1]


def pe_imports(path: Path) -> list[str]:
    """Read the x64 PE import table, including API-set contracts; no third-party packages."""
    data = path.read_bytes()
    def unpack(fmt: str, offset: int):
        if offset < 0 or offset + struct.calcsize(fmt) > len(data):
            raise ValueError(f"Truncated PE file: {path.name}")
        return struct.unpack_from(fmt, data, offset)
    if data[:2] != b'MZ':
        raise ValueError('Expected a Windows PE executable')
    pe, = unpack('<I', 0x3C)
    if data[pe:pe+4] != b'PE\0\0':
        raise ValueError('Invalid PE signature')
    machine, count = unpack('<HH', pe+4)
    optional_size, = unpack('<H', pe+20)
    opt = pe+24
    magic, = unpack('<H', opt)
    if machine != 0x8664 or magic != 0x20B:
        raise ValueError('Installer requires an x64 PE32+ application')
    import_rva, import_size = unpack('<II', opt+120)
    sections = []
    for i in range(count):
        off = opt+optional_size+40*i
        vsize, va, rawsize, raw = unpack('<IIII', off+8)
        sections.append((va, max(vsize, rawsize), raw, rawsize))
    def offset(rva: int) -> int:
        for va, size, raw, rawsize in sections:
            if va <= rva < va+size and rva-va < rawsize:
                return raw+rva-va
        raise ValueError(f'Import RVA outside file sections: {rva:#x}')
    if not import_rva or not import_size:
        raise ValueError('Executable has no import table')
    result = set()
    for i in range(import_size//20):
        descriptor = unpack('<IIIII', offset(import_rva+20*i))
        if not any(descriptor):
            break
        name_start = offset(descriptor[3])
        end = data.find(b'\0', name_start, min(name_start+512, len(data)))
        if end < 0:
            raise ValueError('Unterminated DLL import')
        name = data[name_start:end].decode('ascii')
        if not re.fullmatch(r'[a-zA-Z0-9_.-]+\.dll', name, flags=re.I):
            raise ValueError(f'Invalid DLL import name: {name!r}')
        result.add(name.lower())
    if not result:
        raise ValueError('Empty DLL dependency list')
    return sorted(result)


def compiler_path(explicit: str | None) -> Path:
    if explicit:
        return Path(explicit).resolve()
    for name in ('gcc.exe', 'cl.exe'):
        found = shutil.which(name)
        if found:
            return Path(found)
    for p in (Path('C:/msys64/ucrt64/bin/gcc.exe'), Path('C:/msys64/mingw64/bin/gcc.exe')):
        if p.exists():
            return p
    vswhere = Path(os.environ.get('ProgramFiles(x86)', 'C:/Program Files (x86)'))/'Microsoft Visual Studio/Installer/vswhere.exe'
    if vswhere.exists():
        output = subprocess.check_output([str(vswhere), '-latest', '-products', '*', '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '-find', 'VC/Auxiliary/Build/vcvars64.bat'], text=True).strip()
        if output:
            return Path(output.splitlines()[0])
    raise RuntimeError('Install MSVC C++ build tools or MinGW GCC; alternatively pass --cc PATH')


def build_bootstrap(cc: Path, output: Path) -> None:
    source = REPO/'installer/check_runtime.c'
    if cc.suffix.lower() == '.bat':
        script = output.parent/'compile-bootstrap.cmd'
        script.write_text('@echo off\ncall "'+str(cc)+'" >nul\nif errorlevel 1 exit /b %errorlevel%\ncl.exe /nologo /O1 /GS- /Zl /Fo"'+str(output.with_suffix('.obj'))+'" /Fe"'+str(output)+'" "'+str(source)+'" /link /NODEFAULTLIB /ENTRY:mainCRTStartup /SUBSYSTEM:WINDOWS kernel32.lib shell32.lib\n', encoding='utf-8')
        subprocess.run([os.environ.get('COMSPEC', 'cmd.exe'), '/d', '/c', str(script)], check=True)
    elif cc.name.lower() in ('cl', 'cl.exe'):
        subprocess.run([str(cc), '/nologo', '/O1', '/GS-', '/Zl', '/Fo'+str(output.with_suffix('.obj')), '/Fe'+str(output), str(source), '/link', '/NODEFAULTLIB', '/ENTRY:mainCRTStartup', '/SUBSYSTEM:WINDOWS', 'kernel32.lib', 'shell32.lib'], check=True)
    else:
        compiler_env = os.environ.copy()
        compiler_env['PATH'] = str(cc.parent) + os.pathsep + compiler_env.get('PATH', '')
        subprocess.run([str(cc), '-Os', '-Wall', '-Wextra', '-Werror', '-nostdlib', '-fno-builtin', '-fno-stack-protector', '-fno-tree-loop-distribute-patterns', '-mwindows', '-Wl,--entry=mainCRTStartup', str(source), '-o', str(output), '-lkernel32', '-lshell32'], check=True, env=compiler_env)
    imports = pe_imports(output)
    if set(imports)-{'kernel32.dll', 'shell32.dll'}:
        raise RuntimeError(f'Bootstrap must not require a C runtime: {imports}')


def inno_path(explicit: str | None) -> Path:
    if explicit:
        return Path(explicit).resolve()
    if os.environ.get('ISCC_EXE'):
        return Path(os.environ['ISCC_EXE']).resolve()
    found = shutil.which('ISCC.exe')
    if found:
        return Path(found)
    for root in ('ProgramFiles(x86)', 'ProgramFiles', 'LOCALAPPDATA'):
        base = Path(os.environ.get(root, 'C:/Program Files (x86)'))
        for folder in ('Inno Setup 6', 'Inno Setup 7', 'Programs/Inno Setup 6', 'Programs/Inno Setup 7'):
            p = base/folder/'ISCC.exe'
            if p.exists():
                return p
    raise RuntimeError('Install Inno Setup 6.5+ or pass --iscc PATH')


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--skip-portable', action='store_true', help='Use the existing dist/MemRW3 payload; caller must build and test it first')
    parser.add_argument('--iscc', help='Path to Inno Setup ISCC.exe')
    parser.add_argument('--cc', help='Path to x64 gcc.exe, cl.exe or vcvars64.bat')
    args = parser.parse_args()
    if os.name != 'nt':
        parser.error('The installer must be built on Windows')
    iscc = inno_path(args.iscc)
    cc = compiler_path(args.cc)
    if not args.skip_portable:
        ps = Path(os.environ['SystemRoot'])/'System32/WindowsPowerShell/v1.0/powershell.exe'
        subprocess.run([str(ps), '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', str(REPO/'tools/package_windows.ps1'), '-Python', sys.executable], cwd=REPO, check=True)
    payload = REPO/'dist/MemRW3'
    imports = pe_imports(payload/'MemRW3.exe')
    work = REPO/'dist/installer-work'; work.mkdir(parents=True, exist_ok=True)
    # Work on a separate installer payload, so the portable archive's checksums stay valid.
    stage = work/'payload'
    stage.mkdir(exist_ok=True)
    for name in ('MemRW3.exe', '使用说明.md', '项目技术文档.md', 'CHANGELOG.md', 'package-info.json'):
        shutil.copy2(payload/name, stage/name)
    shutil.copytree(payload/'assets', stage/'assets', dirs_exist_ok=True)
    (stage/'runtime-dependencies.txt').write_text('\n'.join(imports)+'\n', encoding='ascii')
    bootstrap = stage/'MemRW3-runtime-check.exe'
    build_bootstrap(cc, bootstrap)
    report = work/'build-dependency-check.txt'
    result = subprocess.run([str(bootstrap), str(stage/'MemRW3.exe'), str(stage/'runtime-dependencies.txt'), str(report)], timeout=75)
    if result.returncode:
        details = report.read_text(encoding='utf-8', errors='replace') if report.exists() else 'No report generated'
        raise RuntimeError(f'Payload self-check failed ({result.returncode}): {details}')
    output = REPO/'dist/installer'; output.mkdir(exist_ok=True)
    subprocess.run([str(iscc), '/Qp', '/DPayloadDir='+str(stage), '/DOutputDir='+str(output), str(REPO/'installer/MemRW3.iss')], cwd=REPO, check=True)
    version = re.search(r'#define AppVersion "([^"]+)"', (REPO/'installer/MemRW3.iss').read_text(encoding='utf-8-sig')).group(1)
    setup = output/f'MemRW3-Setup-{version}-x64.exe'
    digest = hashlib.sha256(setup.read_bytes()).hexdigest()
    setup.with_suffix('.exe.sha256').write_text(f'{digest}  {setup.name}\n', encoding='ascii')
    print(f'Created {setup}\nSHA256 {digest}\nChecked {len(imports)} actual x64 DLL imports and renderer initialization.', flush=True)


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as exc:
        print(f'Installer build failed: {exc}', file=sys.stderr)
        sys.exit(1)
