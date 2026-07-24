---
name: distill-memories
description: Read `.agents/docs/JOURNAL.md` and `.agents/docs/LTM/` documents, find durable knowledge that belongs in the canonical docs, and update those documents with concise synthesized findings. The canonical target is the root `README.md` (the as-built, human-reader-ready description of the binding); working-practice rules live in `AGENTS.md`. Use when journal or LTM notes have accumulated implementation or design knowledge that should be promoted into the project's canonical documentation.
user-invocable: true
allowed-tools: Bash, Read, Write, Edit, Grep, Glob
---

# Distill Memories

## Overview

Promote durable facts from `.agents/docs/JOURNAL.md` and `.agents/docs/LTM/` into the project's canonical documentation, keeping it up to date:

a. `README.md` (repository root) ... the canonical, human-reader-ready description of imbh-c as-built: what the binding is (a panic-safe `extern "C"` ABI plus a header-only C++ RAII wrapper over IMBH), the C ABI surface and typed query builders, the LGTM query languages, the discovery/admin/IPC surfaces, the zero-copy Arrow `ArrowArrayStream` handoff and its lifetime rules, the single-arrow rule, and the build integration (cargo + cbindgen-generated `include/imbh.h`, CMake/CTest). Favor durable design and narrative over patch-level history. This is the home for most durable design facts.
b. `AGENTS.md` (repository root) ... the working-practice contract for coding agents: the build/test gate, the ownership model, file-management and git rules, and documentation style. Update it only when a *rule* or a *gate step* changes, not for feature-level knowledge.

`README.md` is where as-built structure and the public surface live; `AGENTS.md` carries only the durable rules an agent must follow. Keep the two in their own registers rather than duplicating prose. If the project later grows an `.agents/docs/OVERVIEW.md` or `ARCHITECTURE.md`, treat those as additional targets in the same spirit.

## Sources: JOURNAL and LTM

- `.agents/docs/JOURNAL.md` is the append-only log of findings, insights, and code-review history. It holds the **freshest** durable facts, including changes not yet consolidated into LTM. The `good-sleep` / `reconcile-journal-ltm` skills consolidate JOURNAL entries into `.agents/docs/LTM/`; distill runs against whatever is present.
- `.agents/docs/LTM/` holds the consolidated, topic-organized reference material (`INDEX.md` is its table of contents). It may not exist yet if `good-sleep` has not run.

Read both. When JOURNAL and LTM overlap, treat LTM as the settled synthesis and JOURNAL as the source of anything newer than the last consolidation. If JOURNAL records a substantial code change that the target docs do not yet reflect, that is exactly the gap this skill closes.

## Read in This Order

1. Read `README.md` first to see what the canonical description already records.
2. Read `.agents/docs/JOURNAL.md` (at least the entries since the last consolidation record) and `.agents/docs/LTM/INDEX.md` if it exists.
3. Read `AGENTS.md` to see which working-practice rules are already stated.
4. Open only the LTM documents that look relevant to the gaps, stale sections, or missing detail you identified.

Do not bulk-load every LTM file unless the set is still small enough that selective reading costs more than reading them all.

## Classify Findings Before Editing

For each candidate fact, decide whether it belongs in:

- `README.md`: the public ABI surface, the typed/LGTM/discovery/admin entry points, the zero-copy handoff and its lifetime rules, the single-arrow rule, the build model, and current status/scope. This is the canonical home for most durable design facts; keep it human-reader-ready.
- `AGENTS.md`: a change to the build/test *gate*, the *ownership model rule*, or another agent-facing protocol. Update in its own register, without duplicating README prose.
- More than one: a change to the ownership/handoff model often belongs in both the README (as-built explanation) and AGENTS.md (the rule an agent must uphold).
- None of them: narrow bug history, one-off investigations, temporary workarounds, or details too fine-grained for canonical docs — leave those in JOURNAL/LTM.

Prefer durable knowledge over incident history. Convert timelines into timeless guidance.

The source of truth for implementation detail is always the code (`src/lib.rs`, `src/query.rs`, `src/lgtm.rs`, `src/discovery.rs`, `src/error.rs`, `src/arrow_stream.rs`, the generated `include/imbh.h` / `include/imbh.hpp`, and the upstream crates under `../imbh`). Verify a fact against the code before writing it into a canonical doc; JOURNAL/LTM point you at what changed, not the exact current symbol or ABI.

## Update Strategy

When updating the target docs:

- Synthesize; do not copy JOURNAL/LTM prose verbatim.
- Merge into existing sections when possible instead of appending random new sections.
- Add a new section only when the information represents a stable topic the current document truly lacks.
- Keep summaries compact. Canonical docs should stay easier to scan than the underlying notes.
- Preserve exact file paths, crate/feature names, C-ABI symbol names (`imbh_*`), and Arrow/DataFusion terms when they help precision.
- If multiple sources disagree, or JOURNAL and the current code disagree, call out the ambiguity or stop and ask the user before cementing one interpretation.

## Editing Heuristics

- Favor architectural patterns and contracts over patch-level history.
- Favor the current build/ownership/handoff model over implementation anecdotes.
- Omit test names unless they explain an important invariant or guarantee (e.g. the empty-result-still-typed or header-freshness gates).
- Omit details already covered well in the target document.
- Fix obvious typos or stale wording in the touched sections if doing so improves clarity.

## Validation

Before finishing:

1. Re-read the edited sections of every document you changed.
2. Check that each added fact is supported by a source note (JOURNAL/LTM) and, for implementation detail, verified against the code.
3. Check that public-surface material did not leak into agent-only rules, and vice versa.
4. Keep the documentation style rules in `AGENTS.md`: half-width parentheses and half-width colons.
