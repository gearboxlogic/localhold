# Browse The Hold

`hold ui` opens an interactive terminal browser over the configured store.

## Panes And Navigation

The left pane lists every non-empty scope visible to the current principal,
including unregistered scope keys, with exact-assignment counts. Registered
display names are used when available; long path-like keys are shortened to a
unique trailing name. Use `tab` or the left/right arrows to change panes and
`j`/`k` or the up/down arrows to apply a scope filter immediately. Scope
filters remain active while searching; tags are separate metadata and are not
scope rows.

## Search And Inspection

Use `/` to search, with `m` cycling keyword, text, semantic, hybrid, and
auto modes. Auto lets the engine choose the best available retrieval path and
fall back when embedding or full-text search is unavailable. The header shows
the requested mode while a search is pending and the concrete mode used after
results arrive. Use `enter` to inspect a memory with its audit trail.

## Editing

From the detail view, `e` edits content, tags, importance, expiry, and card
metadata; `d` deletes after confirmation. `Ctrl+S` saves an edit, and `Esc`
cancels.

Tags are edited as a JSON string array (for example
`["decision","client,west"]`) so punctuation inside a tag is preserved exactly.

## Authorization And Concurrency

Browsing remains side-effect-free, while mutations use the normal audited
authorization path and require `--principal` or `server.principal`. SQLite WAL
and PostgreSQL allow the UI to run alongside a serving LocalHold process.

The UI opens the configured store directly. `--principal` and
`server.principal` are trusted local assertions used for policy evaluation, not
authentication. Anyone who can run `hold ui` with the database credential can
select another principal, and direct database access bypasses LocalHold policy.
Protect the process, configuration, and database at the operating-system and
database boundaries; do not use the TUI principal for multi-user isolation.
