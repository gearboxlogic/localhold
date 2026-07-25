# Browse The Hold

`hold ui` opens an interactive terminal browser over the configured store.

## Panes And Navigation

The left pane lists authorized active contexts grouped by kind. The first row
is an intentional broad authorized search. Use `tab` or the left/right arrows
to change panes and `j`/`k` or the up/down arrows to move.

Press `space` to toggle several direct contexts, `x` to return to broad search,
and `D` to toggle descendant expansion. Selected children always include their
ancestor chain. The resulting memory filter uses OR within one context kind and
AND across different attached kinds.

## Search And Inspection

Use `/` to search, with `m` cycling keyword, text, semantic, hybrid, and
auto modes. Auto lets the engine choose the best available retrieval path and
fall back when embedding or full-text search is unavailable. The header shows
the requested mode while a search is pending and the concrete mode used after
results arrive. Use `enter` to inspect a memory with its audit trail.

## Editing

From the detail view, `e` edits content, ordered direct context IDs, tags,
importance, expiry, and card metadata; `d` deletes after confirmation.
`Ctrl+S` saves an edit, and `Esc` cancels. The first context ID becomes the
compatibility-primary membership.

Tags are edited as a JSON string array (for example
`["decision","client,west"]`) so punctuation inside a tag is preserved exactly.

## Context Manager

Move to a context and press `c` to open the operator Context Manager. Its panes
cover kind definitions, mutable definition fields, fingerprinted identities,
aliases, hierarchy, grants, archive/reactivation, principal policy, operator
defaults, and anchor overrides. Press `e` to edit the active pane as JSON and
`Ctrl+S` to apply an audited mutation. `Esc` protects dirty drafts with an
explicit discard confirmation.

Raw typed identity values entered in the identity pane are normalized and
fingerprinted before persistence. The UI subsequently shows only safe redacted
labels and fingerprints. Archived contexts and identities remain reserved and
must be reactivated here; ordinary agent tools cannot replace them.

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
