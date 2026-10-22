#!/usr/bin/env bash
# Usage: build.sh STATIC_BINARY OUTPUT_DIR [RPM_RELEASE] [LICENSE_BUNDLE]
# Default bundle: sibling licenses/ directory from the Linux artifact.
set -euo pipefail
umask 022
root=$(cd "$(dirname "$0")/../.." && pwd)
binary=$(realpath "$1")
output=$(realpath -m "$2")
release=${3:-1}
notices=$(realpath "${4:-$(dirname "$binary")/licenses}")
python3 "$root/scripts/release-notices.py" "$notices" --check
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$root/Cargo.toml" | head -1)
[ -n "$version" ] || { echo "cannot read version from Cargo.toml" >&2; exit 1; }
SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --format=%ct 2>/dev/null)} \
    || { echo "no git history available: set SOURCE_DATE_EPOCH" >&2; exit 1; }
export SOURCE_DATE_EPOCH
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS} "$work/payload/usr/bin" "$work/payload/etc/scat" "$work/payload/usr/share/doc/scat"
mkdir -p "$work/payload/usr/share/licenses/scat"
cp -a "$notices/." "$work/payload/usr/share/licenses/scat/"
install -m755 "$binary" "$work/payload/usr/bin/scat"
install -m644 "$root/docs/scat.example.yaml" "$work/payload/etc/scat/scat.yaml"
cp "$root"/docs/{VENDORING.md,TUI_OPTIONS.md,scat.example.yaml} "$work/payload/usr/share/doc/scat/"
"$binary" --help > "$work/payload/usr/share/doc/scat/help.txt"
"$binary" tui --help > "$work/payload/usr/share/doc/scat/tui-help.txt"
for shell in bash zsh fish; do
    case $shell in
        bash) path=usr/share/bash-completion/completions/scat ;;
        zsh) path=usr/share/zsh/site-functions/_scat ;;
        fish) path=usr/share/fish/vendor_completions.d/scat.fish ;;
    esac
    mkdir -p "$work/payload/$(dirname "$path")"
    "$binary" --config "$work/no-config.yaml" completions "$shell" > "$work/payload/$path"
done
# Man pages come from the same binary; gzip -n keeps the bytes reproducible.
"$binary" --config "$work/no-config.yaml" man --dir "$work/payload/usr/share/man/man1"
gzip -9n "$work"/payload/usr/share/man/man1/*.1
tar --sort=name --mtime="@$SOURCE_DATE_EPOCH" --owner=0 --group=0 --numeric-owner -cf "$work/SOURCES/payload.tar" -C "$work/payload" .
rpmbuild -bb "$root/packaging/rpm/scat.spec" --define "_topdir $work" \
    --define "scat_version $version" --define "scat_release $release" \
    --define 'use_source_date_epoch_as_buildtime 1' --define 'clamp_mtime_to_source_date_epoch 1' \
    --define '_buildhost reproducible' --define '_binary_payload w9.gzdio'
mkdir -p "$output"
cp "$work"/RPMS/x86_64/*.rpm "$output/"
