#!/usr/bin/env bash
# Run as root in a disposable UBI container with python3 and diffutils installed.
set -euo pipefail
rpm -ivh /input/first/*.rpm
rpm -ql scat
rpm -q --requires scat
rpm -q --scripts scat
for path in /usr/bin/scat /usr/share/doc/scat/help.txt /usr/share/bash-completion/completions/scat /usr/share/zsh/site-functions/_scat /usr/share/fish/vendor_completions.d/scat.fish; do
    test -s "$path"
done
printf '\n# administrator change\n' >> /etc/scat/scat.yaml
cp /etc/scat/scat.yaml /tmp/config.expected
mkdir -p /catalog/scripts
printf '# Purpose: package smoke\nprint(1)\n' > /catalog/scripts/package.py
scat --db /catalog/scripts.sqlite catalog build --scan-root /catalog/scripts -q
sha256sum /catalog/scripts.sqlite > /tmp/catalog.sha256
scat --db /catalog/scripts.sqlite catalog info
# scat only populates an existing cache root; it never creates the root itself.
mkdir -p /tmp/scat-cache
scat --db /catalog/scripts.sqlite --cache-dir /tmp/scat-cache search package --output json
scat --db /catalog/scripts.sqlite --no-cache search package --output json
"${PYTHON:-python3}" /input/tui_pty.py /usr/bin/scat
first_release=$(rpm -q --qf '%{RELEASE}' scat)
rpm -Uvh /input/second/*.rpm
second_release=$(rpm -q --qf '%{RELEASE}' scat)
test "$first_release" != "$second_release"
cmp /etc/scat/scat.yaml /tmp/config.expected
sha256sum -c /tmp/catalog.sha256
rpm -e scat
cmp /etc/scat/scat.yaml.rpmsave /tmp/config.expected
sha256sum -c /tmp/catalog.sha256
test -n "$(ls -A /tmp/scat-cache)"
test ! -e /usr/bin/scat
for path in /usr/share/bash-completion/completions/scat /usr/share/zsh/site-functions/_scat /usr/share/fish/vendor_completions.d/scat.fish; do
    test ! -e "$path"
done
