# Beta Triage

Classify each beta report before changing code:

- **Blocker**: crash, data loss, cannot launch, or corrupt save.
- **High**: core workflow unusable, reproducible severe performance issue, or
  LSP/terminal failure affecting most users.
- **Medium**: feature regression with a workaround.
- **Low**: cosmetic issue, minor shortcut mismatch, or documentation gap.

For every accepted issue record:

- Stable reproduction steps.
- First affected commit if known.
- OS and hardware.
- Whether the issue reproduces with LSP disabled.
- Test or benchmark added to prevent recurrence.
- Fix commit and verification result.

Do not mark M7-09 complete from internal smoke tests alone. It requires an
external beta artifact, tester feedback, and a reviewed triage summary.
