"""Regression checks for release bundle tampering and missing original notices."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('notices', Path(__file__).with_name('release-notices.py'))
notices = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(notices)


class BundleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.crate = self.root / 'crate'
        self.crate.mkdir()
        (self.crate / 'Cargo.toml').write_text('[package]\nname="fixture"\n')
        (self.crate / 'LICENSE').write_text('original fixture notice')
        self.native = self.root / 'native'
        self.native.mkdir()
        (self.native / 'COPYRIGHT').write_text('native copyright and license')
        self.metadata = {'packages': [{'name': 'fixture', 'version': '1.0.0',
            'manifest_path': str(self.crate / 'Cargo.toml'), 'license': 'MIT',
            'license_file': None, 'source': 'registry+fixture'}]}
        self.output = self.root / 'bundle'

    def build(self):
        package = self.metadata['packages'][0]
        tree = f"{package['name']} v{package['version']}\n"
        with patch.object(notices, 'cargo', side_effect=[json.dumps(self.metadata), tree]):
            notices.build('scat', 'fixture-target', self.output, [self.native])

    def test_roundtrip_and_determinism(self):
        self.build()
        notices.check(self.output)
        original = (self.output / 'third-party-sources.tar.gz').read_bytes()
        self.output = self.root / 'second'
        self.build()
        self.assertEqual(original, (self.output / 'third-party-sources.tar.gz').read_bytes())

    def test_changed_or_missing_release_files_fail(self):
        self.build()
        path = self.output / 'NOTICE'
        path.write_text('changed notice')
        with self.assertRaises(ValueError):
            notices.check(self.output)
        path.unlink()
        with self.assertRaises(FileNotFoundError):
            notices.check(self.output)

    def test_missing_dependency_notice_fails(self):
        (self.crate / 'LICENSE').unlink()
        with self.assertRaisesRegex(ValueError, 'missing original'):
            self.build()

    def test_empty_native_notice_fails(self):
        (self.native / 'COPYRIGHT').write_text('')
        with self.assertRaisesRegex(ValueError, 'empty release material'):
            self.build()

    def test_missing_native_notices_fail(self):
        (self.native / 'COPYRIGHT').unlink()
        with self.assertRaisesRegex(ValueError, 'no native notices'):
            self.build()

    def test_changed_supplement_fails(self):
        supplement = self.root / 'supplements'
        supplement.mkdir()
        (supplement / 'LICENSE').write_text('unexpected replacement')
        (supplement / 'manifest.json').write_text(json.dumps({'packages': [{
            'name': 'fixture', 'version': '1.0.0', 'license': 'MIT',
            'files': [{'path': 'LICENSE', 'sha256': '0' * 64, 'source': 'fixture'}],
        }]}))
        with patch.object(notices, 'SUPPLEMENTS', supplement):
            with self.assertRaisesRegex(ValueError, 'supplemental notice digest mismatch'):
                self.build()

    def test_runtime_update_requires_review(self):
        self.metadata['packages'][0]['name'] = 'ort-sys'
        with self.assertRaisesRegex(ValueError, 'ONNX Runtime notices need review'):
            self.build()

    def test_runtime_mapping_change_fails(self):
        supplement = self.root / 'supplements'
        supplement.mkdir()
        (self.crate / 'dist.tsv').write_text('changed native distribution')
        (supplement / 'manifest.json').write_text(json.dumps({'packages': [{
            'name': 'fixture', 'version': '1.0.0', 'license': 'MIT',
            'files': [], 'bundled_checks': [{'path': 'dist.tsv', 'sha256': '0' * 64}],
        }]}))
        with patch.object(notices, 'SUPPLEMENTS', supplement):
            with self.assertRaisesRegex(ValueError, 'native runtime distribution mapping changed'):
                self.build()


if __name__ == '__main__':
    unittest.main()
