"""Windows integration checks for a built installer payload (no application installation)."""
from pathlib import Path
import os
import subprocess
import tempfile
import unittest

from build_installer import REPO, pe_imports

PAYLOAD = REPO/'dist/installer-work/payload'


@unittest.skipUnless(os.name == 'nt' and (PAYLOAD/'MemRW3-runtime-check.exe').exists(), 'Build the Windows installer payload first')
class RuntimeChecks(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='MemRW3-runtime-test-')
        self.root = Path(self.temp.name)
        self.report = self.root/'report.txt'
        self.manifest = self.root/'dependencies.txt'

    def tearDown(self):
        self.temp.cleanup()

    def check(self, executable: Path, libraries: str):
        self.manifest.write_text(libraries, encoding='ascii')
        result = subprocess.run([str(PAYLOAD/'MemRW3-runtime-check.exe'), str(executable), str(self.manifest), str(self.report)], timeout=75)
        return result.returncode, self.report.read_text(encoding='utf-8')

    def test_bootstrap_requires_no_c_runtime(self):
        self.assertEqual(set(pe_imports(PAYLOAD/'MemRW3-runtime-check.exe')), {'kernel32.dll', 'shell32.dll'})

    def test_missing_dependency_stops_before_starting_application(self):
        code, report = self.check(self.root/'does-not-exist.exe', 'MemRW3MissingDependencyForTest.dll\n')
        self.assertEqual(code, 20)
        self.assertIn('MemRW3MissingDependencyForTest.dll', report)
        self.assertNotIn('Could not start renderer', report)

    def test_manifest_rejects_dll_paths(self):
        code, report = self.check(PAYLOAD/'MemRW3.exe', '../kernel32.dll\n')
        self.assertEqual(code, 65)
        self.assertIn('Invalid DLL name', report)

    def test_unstartable_application_fails(self):
        code, report = self.check(self.root/'does-not-exist.exe', 'kernel32.dll\n')
        self.assertEqual(code, 22)
        self.assertIn('Could not start renderer', report)

    def test_actual_program_initializes_renderer(self):
        libraries = (PAYLOAD/'runtime-dependencies.txt').read_text(encoding='ascii')
        code, report = self.check(PAYLOAD/'MemRW3.exe', libraries)
        self.assertEqual(code, 0, report)
        self.assertTrue(report.startswith('OK:'), report)

    def test_invalid_pe_is_rejected(self):
        broken = self.root/'broken.exe'
        broken.write_bytes(b'MZ\0')
        with self.assertRaises(ValueError):
            pe_imports(broken)


if __name__ == '__main__':
    unittest.main(verbosity=2)
