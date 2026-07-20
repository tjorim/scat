# vc Filesystem Contract

## Purpose of this document

This document describes the **observable, filesystem‑level contract** of **vc**, a
script change‑control tool.

The goal of this document is **not** to re‑implement or vendor vc, but to clearly
define what our tooling **relies on and observes** when interoperating with vc.

> **Important**
> - This repository does **not** include vc source code.
> - This repository does **not** import or depend on vc internals.
> - All integration is based solely on filesystem behavior and conventions described here.

If vc internals change but this contract remains true, integration remains valid.

---

## What vc is (and is not)

### vc *is*
- A **filesystem‑based change‑control helper**
- A way to **check out scripts for editing**
- A way to **check in scripts with limited version history**
- A tool that uses **copies, symlinks, and archival folders**

### vc is *not*
- A version control system (no branches, no merges)
- A catalog or search tool
- A metadata or ownership registry
- A dependency analyzer

These responsibilities are intentionally handled elsewhere.

---

## High‑level model

vc manages scripts using three concepts:

1. **Working directory**
   - The directory where the "active" script is referenced (often via symlink)
2. **DEVELOP directory**
   - Where checked‑out copies live while being edited
3. **ARCHIVE directory**
   - Where obsolete or superseded versions are stored

Editing a script happens in DEVELOP, not in place.

---

## Base directory layout

vc uses two base directories:

```

\<DEVELOP\_BASE>/
\<ARCHIVE\_BASE>/

```

Within both bases, files are further grouped by **OS flavor** subdirectories. The exact
folder names are environment‑specific and configured via the vc config file.

> ⚠️ The base paths and OS folder names are environment‑specific and **must be treated
> as configurable** in consuming tools.

---

## Checkout semantics

### What "checkout" means

A checkout creates a **private working copy** of a script in the DEVELOP directory.

### Observable behavior

Given a script named:

```

myscript.py

```

A checkout produces a file in DEVELOP similar to:

```

myscript.py\_<TIMESTAMP>\_<USER>

```

Where:
- `<TIMESTAMP>` is date-based, at varying precision — observed forms are
  `YYYYMMDD_HHMMSS` (e.g. `20260720_103044`), `YYYYMMDD_HHMM`, and
  occasionally date-only `YYYYMMDD`
- `<USER>` is the username performing the checkout

### Invariants relied upon

- Checked‑out files exist **only** under DEVELOP
- Username and timestamp are encoded in DEVELOP filenames; ARCHIVE and
  working‑directory version filenames encode **only the timestamp**
- Timestamp precision varies and must be tolerated at date, minute, or
  second granularity
- Multiple checkouts of the same script may exist concurrently

---

## Checkin semantics

### What "checkin" means

A checkin:
1. Moves the edited file from DEVELOP back to the working directory
2. Updates or creates a **symlink** representing the active version
3. Moves older versions to ARCHIVE if retention limits are exceeded

### Observable behavior

- The working directory contains:
  - One symlink (logical script name, e.g. `myscript.py`)
  - One or more versioned files named `myscript.py_<TIMESTAMP>` —
    **no user suffix** (observed retention: the two most recent versions)
- The symlink points to the latest checked‑in version
- Older versions are preserved (up to a configured limit)

---

## Version retention

### Observable behavior

- A fixed number of historical versions are retained
- When the limit is exceeded:
  - The oldest versions are moved to ARCHIVE
- ARCHIVE entries are named `myscript.py_<TIMESTAMP>` — like
  working‑directory versions, they carry **no user suffix**
  (e.g. `update_board_firmware.sh_20240921_135312`)
- ARCHIVE contents are treated as **immutable**

> The retention count is an implementation detail but appears consistently enforced.

---

## Rollback semantics

### What "rollback" means

A rollback:
- Re‑points the symlink for a script to an older version
- Does **not** restore files from ARCHIVE
- Does **not** modify DEVELOP

### Observable impact

- Symlink target changes
- File modification times may update
- No new files are created

---

## Obsolete semantics

### What "obsolete" means

Marking a script obsolete results in:
- All matching versions from the working directory being moved to ARCHIVE
- Any DEVELOP copies of the same script also being archived

After obsoleting:
- No active symlink remains

---

## Template usage

vc supports creating new scripts from templates.

Observable facts:
- Templates exist for at least:
  - Python
  - Shell
- New files are immediately checked out after creation

Template content itself is **out of scope** for this contract.

---

## Metadata conventions

Many vc‑managed scripts contain structured header comments, commonly using fields like:

```

@brief
@author
@techowner
@funcowner
@history

```

These headers are:
- Optional
- Not enforced by vc
- Highly valuable for discovery tooling

Consuming tools may parse these headers as **best‑effort metadata**, without assuming
they are always present or complete.

---

## Invariants this repository relies on

This repository **assumes only** the following invariants:

- Checked‑out files live under DEVELOP
- DEVELOP filenames encode user and timestamp; ARCHIVE and
  working‑directory version filenames encode timestamp only
- ARCHIVE is append‑only
- Symlinks represent the active version
- No writes are performed by our tooling

Everything else is considered an implementation detail.

---

## Explicit non‑assumptions

This repository does **not** assume:

- vc is implemented in Python
- vc CLI flags or argument names
- vc internal helper functions
- vc availability on all systems (e.g. cron servers)

Integration must degrade gracefully if vc is unavailable.

---

## Relationship with this repository

vc is treated as:

> **An external, filesystem‑based change‑control system**

This repository:
- Observes vc state
- Displays vc status
- Explains vc‑protected scripts
- Does **not** replace or modify vc

This separation is intentional and required for long‑term maintainability.

---

## Why we do not vendor or copy vc code

- To avoid multiple sources of truth
- To avoid accidental divergence
- To keep responsibilities clear
- To respect existing operational ownership

All integration is done via this documented contract.

---

## Change management

If vc behavior changes in a way that breaks this contract:
- Update this document first
- Then update consuming code accordingly

The document is the **single source of truth** for integration assumptions.
