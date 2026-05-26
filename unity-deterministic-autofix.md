# Unity deterministic autofix

This note defines when GitButler can automatically repair Unity import or compile
failures, how it should collect evidence, and when it must stop and ask for
review instead of editing project code.

## Goal

GitButler should proactively validate Unity repositories when the user presses
Autofix, without relying on stale Unity logs or requiring the interactive Unity
editor to be open.

The intended flow is:

1. Detect that the repository is a Unity project.
2. Refuse headless validation if the Unity editor appears open for that project.
3. Launch Unity in a controlled batchmode process.
4. Collect structured import and compiler diagnostics during that run.
5. Apply only deterministic fixes.
6. For non-deterministic code repairs, generate a proposed patch and explain why
   user review is required.

## Definitions

A deterministic fix is a change where GitButler can prove the desired final
state from local project facts, without inventing gameplay or application
semantics.

The proof must come from one or more of:

- Unity project settings.
- Unity package and assembly definition metadata.
- Git attributes, Git config, and Git LFS configuration.
- Unity compiler diagnostics from the current validation run.
- Current repository contents.
- Git history or current diff showing a clear rename, move, or deletion.
- Unity-generated metadata such as `.meta` GUIDs and serialized references.

If a fix requires guessing intent, adding new domain behavior, choosing between
multiple valid APIs, or changing runtime semantics, it is not deterministic.

## Validation Source

GitButler should not use old `Editor.log` content as the source of truth for
proactive repair. Logs are useful for debugging, but they can be stale and may
belong to a different editor session.

For proactive validation, GitButler should run Unity itself:

```text
Unity -batchmode -quit -accept-apiupdate -projectPath <repo> -executeMethod <GitButlerDiagnostics.Run>
```

The diagnostics helper should be Editor-only and should collect compiler
messages from Unity compilation callbacks, then write a structured result file
for GitButler to read after Unity exits.

## Safe Automatic Fixes

These are valid candidates for automatic mutation because the target state is
known and low-risk.

### Unity Project Safety

GitButler may automatically:

- Set Asset Serialization to Force Text.
- Configure Unity Smart Merge drivers in repo-local Git config.
- Configure LFS-backed Unity YAML merge drivers when `.gitattributes` uses them.
- Add known generated/filter-managed paths to GitButler's local ignore list.
- Run Unity's API updater in batchmode by including `-accept-apiupdate`.

These changes are deterministic because Unity and Git define the expected
configuration values.

### Package and Restore Fixes

GitButler may automatically:

- Restore missing packages from `Packages/manifest.json` and lockfile state by
  letting Unity resolve packages during batchmode validation.
- Remove stale Unity cache artifacts only when they are generated directories
  such as `Library`, `Temp`, or GitButler-owned diagnostic output.
- Re-run validation after package restore or API update.

GitButler must not rewrite package versions unless there is a single exact
version pinned by committed project metadata.

### Assembly Definition Fixes

GitButler may automatically update `.asmdef` references only when all of these
are true:

- The compiler error is caused by a missing assembly reference.
- The referenced type exists in exactly one project assembly.
- Unity assembly metadata proves the failing source assembly does not reference
  that assembly.
- Adding the reference does not create a cycle.
- Platform/include/exclude constraints are compatible.

If multiple assemblies export the type, or adding the reference creates a cycle,
GitButler should propose options instead of editing.

### GUID and Meta Repairs

GitButler may automatically:

- Restore a missing `.meta` file from Git if the asset exists and the missing
  meta file is tracked in history.
- Restore a missing asset from Git if a tracked `.meta` file references it and
  Git history contains the paired asset.
- Remove orphaned generated files from GitButler local ignore handling only when
  they are known generated outputs.

GitButler must not generate new GUIDs for existing referenced assets as an
autofix. New GUIDs can break serialized references.

## Compiler Error Classes

Compiler diagnostics can drive deterministic fixes, but the error code alone is
not enough. GitButler needs local evidence for the specific repair.

### CS1061 Missing Member

Example:

```text
'DealerManager' does not contain a definition for 'Booths'
```

GitButler may automatically fix this only when there is a single proven mapping,
such as:

- Git history shows `Booths` was renamed to exactly one new member on
  `DealerManager`, and all current references should be updated.
- The current diff renamed the member and missed references in files affected by
  the same change.
- Unity serialized metadata or generated code has a known stale reference and
  can be regenerated without changing source behavior.

GitButler must not automatically:

- Add a new `Booths` property or field.
- Change the call site to a fuzzy match when multiple candidates exist.
- Guess casing or pluralization changes without a proven rename.
- Replace the receiver type with another type.
- Edit runtime logic to approximate the missing behavior.

For ambiguous CS1061 cases, GitButler should produce a proposed patch with:

- The failing line.
- Candidate members found on the target type.
- Candidate renames from Git history.
- Why the patch is not safe to apply automatically.

### Missing Type or Namespace

GitButler may automatically fix missing type or namespace errors only when:

- The type exists in exactly one assembly already present in the project.
- The fix is an `.asmdef` reference addition that passes the rules above.
- Or the missing `using` directive maps to exactly one namespace in the current
  project and does not introduce an ambiguous type.

If the type could come from a package, GitButler may suggest a package install
but should not add new dependencies automatically unless the package is already
pinned in committed Unity metadata.

### API Updater Errors

GitButler may automatically rerun Unity with `-accept-apiupdate`. If Unity
updates scripts, GitButler should show the resulting diff.

GitButler should not hand-edit API migrations unless the migration is documented
by Unity and has a one-to-one replacement with no semantic choice.

## Autofix Execution Policy

Autofix should run in phases.

### Phase 1: Local Configuration

Apply deterministic Git and Unity project safety configuration:

- Smart Merge drivers.
- Force Text serialization.
- Local ignore entries for generated/filter-managed paths.

### Phase 2: Controlled Unity Validation

Run Unity in batchmode only when:

- Unity is installed and matches the project version, or a compatible version is
  explicitly selected.
- The interactive Unity editor is not open for this project.
- The worktree state is known to GitButler.
- The user initiated Autofix or an equivalent explicit validation action.

### Phase 3: Classify Diagnostics

Parse structured diagnostics and classify each issue:

- deterministic autofix available
- safe generated/cache cleanup
- proposed patch only
- manual action required

### Phase 4: Apply or Propose

Apply deterministic fixes directly. For source edits that are not deterministic,
write no changes unless the user approves a proposed patch.

After any applied fix, rerun validation once to confirm the error disappeared.
Avoid infinite loops by recording which fixes were already applied in the run.

## When Not To Autofix

GitButler must not automatically edit when:

- Unity is open for the project.
- The fix changes gameplay or app behavior.
- Multiple candidate fixes are plausible.
- The target member, type, assembly, or package cannot be uniquely identified.
- The change adds new public API solely to satisfy a caller.
- The change deletes user-authored assets or scripts.
- The change rewrites broad package versions.
- The repository has unresolved merge conflicts in files needed for diagnosis.
- The worktree contains unrelated user edits that the fix would overlap.
- Unity validation cannot be completed reliably.

In these cases, GitButler should explain the evidence it found and offer a
reviewable patch or manual steps.

## User Experience

The Autofix button should report:

- Which deterministic fixes were applied.
- Which validation command was run.
- Which compiler/import errors remain.
- Which fixes require review and why.
- Whether Unity must be closed before validation can run.

The important distinction is that Autofix can be proactive and smart, but source
code changes must remain evidence-based. A compiler error is a starting point;
it is not permission to guess.
