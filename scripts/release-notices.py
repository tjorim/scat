#!/usr/bin/env python3
"""Build/check release license bundles from locked, target-specific Cargo inputs.

Includes complete published crate sources to retain nested/native notices and
MPL-covered source. Build dependencies are included conservatively; dev-only
and other-target dependencies are excluded. No SPDX expression replaces text.
"""
import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import shutil
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parent.parent
SUPPLEMENTS = ROOT / 'docs/dependency-notices'


def cargo(*args):
    return subprocess.check_output(['cargo', *args], cwd=ROOT, text=True)


def is_notice(path):
    return path.name.upper().startswith(('LICENSE', 'COPYING', 'NOTICE', 'UNLICENSE', 'COPYRIGHT', 'AUTHORS', 'THIRDPARTYNOTICES'))


def add_bytes(archive, name, data):
    if not data.strip():
        raise ValueError(f'empty release material: {name}')
    info = tarfile.TarInfo(name)
    info.size = len(data)
    info.mode = 0o644
    archive.addfile(info, io.BytesIO(data))


def build(package, target, output, native):
    if output.exists():
        raise ValueError(f'output already exists: {output}')
    metadata = json.loads(cargo('metadata', '--locked', '--format-version', '1'))
    lines = set(cargo('tree', '-p', package, '--locked', '--target', target,
                      '--edges', 'normal,build', '--prefix', 'none', '--format', '{p}',
                      '--no-dedupe').splitlines())
    packages = sorted((p for p in metadata['packages'] if
                       f"{p['name']} v{p['version']}" in lines),
                      key=lambda p: (p['name'], p['version']))
    if not packages:
        raise ValueError('empty dependency graph')
    supplements = {(p['name'], p['version']): p for p in
                   json.loads((SUPPLEMENTS / 'manifest.json').read_text())['packages']}
    output.mkdir(parents=True)
    shutil.copyfile(ROOT / 'LICENSE', output / 'LICENSE')
    shutil.copyfile(ROOT / 'NOTICE', output / 'NOTICE')
    inventory = {'package': package, 'target': target, 'packages': [], 'native': [], 'files': {}}
    # Fixed gzip header and tar metadata keep repeated RPM inputs byte-identical.
    with (output / 'third-party-sources.tar.gz').open('wb') as raw:
        with gzip.GzipFile(filename='', mode='wb', fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode='w') as archive:
                for p in packages:
                    directory = Path(p['manifest_path']).parent
                    files = sorted(f for f in directory.rglob('*') if f.is_file())
                    notices = [f for f in files if is_notice(f)]
                    label = f"{p['name']}-{p['version']}"
                    if not p['license'] and not p['license_file']:
                        raise ValueError(f'{label}: no license metadata')
                    supplement = supplements.get((p['name'], p['version']))
                    if p['name'] == 'ort-sys' and not supplement:
                        raise ValueError('ONNX Runtime notices need review for this ort-sys version')
                    supplemental = []
                    if supplement:
                        if supplement['license'] != p['license']:
                            raise ValueError(f'{label}: supplemental license mismatch')
                        for entry in supplement.get('bundled_checks', []):
                            if hashlib.sha256((directory / entry['path']).read_bytes()).hexdigest() != entry['sha256']:
                                raise ValueError(f'{label}: native runtime distribution mapping changed')
                        for entry in supplement['files']:
                            data = (SUPPLEMENTS / entry['path']).read_bytes()
                            if hashlib.sha256(data).hexdigest() != entry['sha256']:
                                raise ValueError(f'{label}: supplemental notice digest mismatch')
                            add_bytes(archive, f"supplemental/{entry['path']}", data)
                            supplemental.append(entry)
                    if not notices and not supplemental:
                        raise ValueError(f'{label}: missing original license/notice text')
                    for f in files:
                        data = f.read_bytes()
                        # Empty source files are legitimate; notices must contain text.
                        if is_notice(f) and not data.strip():
                            raise ValueError(f'{label}: empty notice {f.name}')
                        info = tarfile.TarInfo(f'crates/{label}/{f.relative_to(directory).as_posix()}')
                        info.size = len(data)
                        info.mode = 0o755 if f.stat().st_mode & 0o111 else 0o644
                        archive.addfile(info, io.BytesIO(data))
                    inventory['packages'].append({
                        'name': p['name'], 'version': p['version'], 'license': p['license'],
                        'source': p['source'],
                        'notices': [f.relative_to(directory).as_posix() for f in notices],
                        'supplemental': supplemental,
                    })
                sysroot = Path(subprocess.check_output(['rustc', '--print', 'sysroot'], text=True).strip())
                rustdocs = sysroot / 'share/doc/rust'
                for name in ('COPYRIGHT.html', 'COPYRIGHT-library.html'):
                    add_bytes(archive, f'rust/{name}', (rustdocs / name).read_bytes())
                rustlicenses = sorted((rustdocs / 'licenses').glob('*'))
                if not rustlicenses:
                    raise ValueError('missing Rust toolchain license texts')
                for f in rustlicenses:
                    if f.is_file():
                        add_bytes(archive, f'rust/licenses/{f.name}', f.read_bytes())
                # Native runtimes/toolchain components are not represented by Cargo.
                for index, directory in enumerate(native):
                    notices = sorted(f for f in directory.rglob('*') if f.is_file() and is_notice(f))
                    if not notices:
                        raise ValueError(f'no native notices in {directory}')
                    names = []
                    for f in notices:
                        name = f'native/{index}/{f.relative_to(directory).as_posix()}'
                        add_bytes(archive, name, f.read_bytes())
                        names.append(name)
                    inventory['native'].append({'name': directory.name, 'notices': names})
    for name in ('LICENSE', 'NOTICE', 'third-party-sources.tar.gz'):
        inventory['files'][name] = hashlib.sha256((output / name).read_bytes()).hexdigest()
    (output / 'inventory.json').write_text(json.dumps(inventory, indent=2) + '\n')
    check(output)


def check(output):
    manifest = json.loads((output / 'inventory.json').read_text())
    if not manifest['packages']:
        raise ValueError('empty release inventory')
    for name in ('LICENSE', 'NOTICE', 'third-party-sources.tar.gz'):
        data = (output / name).read_bytes()
        if not data.strip() or hashlib.sha256(data).hexdigest() != manifest['files'][name]:
            raise ValueError(f'missing, empty or changed release material: {name}')
    with tarfile.open(output / 'third-party-sources.tar.gz') as archive:
        names = set(archive.getnames())
        for p in manifest['packages']:
            prefix = f"crates/{p['name']}-{p['version']}/"
            if prefix + 'Cargo.toml' not in names:
                raise ValueError(f'missing source: {prefix}')
            expected = [prefix + n for n in p['notices']]
            expected += ['supplemental/' + n['path'] for n in p['supplemental']]
            if not expected:
                raise ValueError(f'missing notices: {prefix}')
            for name in expected:
                if not archive.extractfile(name).read().strip():
                    raise ValueError(f'empty notice: {name}')
        for native in manifest.get('native', []):
            if not native['notices']:
                raise ValueError('missing native notices')
            for name in native['notices']:
                if not archive.extractfile(name).read().strip():
                    raise ValueError(f'empty native notice: {name}')
        for name in ('rust/COPYRIGHT.html', 'rust/COPYRIGHT-library.html'):
            if name not in names:
                raise ValueError(f'missing toolchain notice: {name}')
    print(f"Verified {output}: {len(manifest['packages'])} dependencies")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    parser.add_argument('--package', choices=['scat', 'scat-embed'])
    parser.add_argument('--target')
    parser.add_argument('--native', type=Path, action='append', default=[])
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    if args.check:
        check(args.output)
    else:
        if not args.package or not args.target or (args.package == 'scat' and not args.native):
            parser.error('building requires --package, --target and (for scat) --native notice directories')
        build(args.package, args.target, args.output, args.native)


if __name__ == '__main__':
    main()
