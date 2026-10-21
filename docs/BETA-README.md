# Galaxy Closed Beta

## Status

The repository is ready for a local beta build. A closed beta release is not
complete until a signed/notarized build is distributed to external testers and
feedback is collected.

## Maintainer Checklist

- [ ] Run `scripts/test-all.sh` on the release commit.
- [ ] Run `scripts/measure-app.sh 5` and attach the JSON result.
- [ ] Run `scripts/profile-memory.sh 10000` and archive `dhat-heap.json`.
- [ ] Run the release packaging flow with a real Developer ID identity.
- [ ] Notarize and staple the app before distributing it.
- [ ] Create and verify the DMG checksum.
- [ ] Test a clean install on a second macOS account or machine.
- [ ] Test launch, open folder, edit/save, PHP/Laravel intelligence, terminal,
  search, Git, and session restore.
- [ ] Give each tester `docs/BETA-FEEDBACK.md` and the privacy note.
- [ ] Record tester OS, Mac model, Galaxy build, and project type (PHP,
  Laravel, CodeIgniter, Symfony, other).
- [ ] Review feedback and publish a triage summary before marking M7-09 done.

## Acceptance Criteria

- No release-blocking crash or data-loss issue remains open.
- At least one tester completes the core workflow from a clean install.
- Feedback includes startup, editing, navigation, PHP intelligence, terminal, Git, and
  RAM observations.
- Crash reports and logs are collected only with explicit consent.
- The build can be reproduced from a tagged commit and its checksum matches the
  distributed artifact.

## Tester Workflow

1. Open a PHP project (Laravel, CodeIgniter, Symfony or plain Composer).
2. Open and edit a file, then save and undo the edit.
3. Use Go to File, Go to Symbol, workspace search, and Problems navigation.
4. Open the terminal, create a second terminal tab, resize it, and click a
   `path:line:column` link.
5. Verify Git decorations and gutter diff.
6. Restart Galaxy and verify session restore.
7. Record observations in `docs/BETA-FEEDBACK.md`.
