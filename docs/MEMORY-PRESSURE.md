# Memory Pressure

Galaxy installs a macOS `DispatchSourceMemoryPressure` source per window.

- Warning pressure clears Core Text layout generations, semantic-token and
  inlay-hint decorations, caret geometry, and the workspace FileFinder cache.
- Critical pressure performs the warning cleanup and suspends syntax workers and
  completed trees for every inactive tab, including recently inactive tabs.
- The active document, text, undo history, tab metadata, diagnostics state, and
  terminal PTY sessions are retained.
- The next draw or tab activation recreates derived caches and resumes syntax
  parsing as needed.

The handler is intentionally process-local and does not close user data. Status bar messages report cleanup without
blocking the main thread on allocation-heavy work.
