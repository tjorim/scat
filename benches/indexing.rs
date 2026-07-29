//! Benchmarks for the indexing/query hot paths: tree-sitter based dependency
//! extraction (run once per indexed file) and FTS query sanitization (run
//! once per search request).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use scat_core::core::db::sanitize_fts_query;
use scat_core::indexer::ast_deps::extract_python_deps_with_module;
use scat_core::indexer::treesitter_deps::{TreeSitterExtractor, extract_reference_paths};

const PYTHON_SOURCE: &str = r#"
"""Nightly catalog maintenance job."""
import os
import sys
import json
from pathlib import Path
from collections import defaultdict
from . import helpers
from .helpers import run as start_run


class CatalogWorker:
    """Runs a single maintenance pass over the script catalog."""

    def __init__(self, root, dry_run=False):
        self.root = Path(root)
        self.dry_run = dry_run
        self.seen = set()

    @staticmethod
    def normalise(path):
        return str(Path(path).resolve())

    def scan(self):
        for entry in self.root.rglob("*.py"):
            self._visit(entry)
        return len(self.seen)

    def _visit(self, entry):
        key = self.normalise(entry)
        if key in self.seen:
            return
        self.seen.add(key)
        helpers.record(key)


def build_index(root, dry_run=False):
    worker = CatalogWorker(root, dry_run=dry_run)
    count = worker.scan()
    start_run()
    return count


def main():
    args = sys.argv[1:]
    root = args[0] if args else "."
    total = build_index(root, dry_run="--dry-run" in args)
    print(json.dumps({"indexed": total}))


if __name__ == "__main__":
    main()
"#;

const BASH_SOURCE: &str = r#"
#!/usr/bin/env bash
set -euo pipefail

source common.sh
. ./lib/logging.sh

REMOTE_HOST="build01"
REMOTE_SCRIPT="/catalog/scripts/jobs/nightly.py"

log_start() {
    echo "starting nightly maintenance"
}

run_remote() {
    ssh "$REMOTE_HOST" python3 "$REMOTE_SCRIPT" --dry-run
}

main() {
    log_start
    run_remote
    scp /catalog/scripts/lib/deploy.py "$REMOTE_HOST:/tmp/deploy.py"
}

main "$@"
"#;

const REFERENCE_CONTENT: &str = r#"
{
  "steps": [
    "/catalog/scripts/jobs/nightly.py",
    "/catalog/scripts/lib/deploy.py",
    "/catalog/scripts/jobs/cleanup.sh"
  ]
}
scp /catalog/scripts/lib/common.py host:/tmp/
ssh host python3 /catalog/scripts/jobs/report.py
"#;

fn bench_extract_python_deps(c: &mut Criterion) {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .unwrap();

    c.bench_function("extract_python_deps_with_module", |b| {
        b.iter(|| {
            extract_python_deps_with_module(
                &mut parser,
                black_box(PYTHON_SOURCE),
                black_box(Some("catalog.jobs.nightly")),
            )
        });
    });
}

fn bench_extract_bash_deps(c: &mut Criterion) {
    let mut extractor = TreeSitterExtractor::new().unwrap();

    c.bench_function("extract_bash_deps", |b| {
        b.iter(|| extractor.extract_deps(black_box(BASH_SOURCE), black_box("bash")));
    });
}

fn bench_extract_reference_paths(c: &mut Criterion) {
    c.bench_function("extract_reference_paths", |b| {
        b.iter(|| extract_reference_paths(black_box(REFERENCE_CONTENT)));
    });
}

fn bench_sanitize_fts_query(c: &mut Criterion) {
    let queries = [
        "nightly-backup",
        "owner:bob AND (nightly OR cleanup)",
        "\"exact phrase\" trailing*",
    ];

    c.bench_function("sanitize_fts_query", |b| {
        b.iter(|| {
            for q in &queries {
                black_box(sanitize_fts_query(black_box(q)));
            }
        });
    });
}

criterion_group!(
    benches,
    bench_extract_python_deps,
    bench_extract_bash_deps,
    bench_extract_reference_paths,
    bench_sanitize_fts_query
);
criterion_main!(benches);
