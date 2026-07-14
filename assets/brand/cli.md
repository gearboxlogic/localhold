# CLI output design

The terminal is LocalHold's primary surface. `hold` output follows the brand the
same way a web page would: tinctures, calm keeping, battlements used sparingly.
Run `assets/brand/cli-preview.sh` in a terminal to see everything below.

## Color

Respect the resident's theme first, assert the brand second:

- Honor `NO_COLOR`, `--no-color`, and non-TTY output (plain text, no escapes).
- Default to the 4-bit ANSI palette so user terminal themes apply; step up to
  256-color or truecolor only when the terminal advertises it.

| Tincture | Role in output | Truecolor | 256-color | 4-bit |
|---|---|---|---|---|
| or `#C89B3C` | The accent: highlighted values, the battlement rule | `38;2;200;155;60` | `38;5;179` | `33` (yellow) |
| azure (light) `#7FA3D4` | Identifiers, scopes, paths | `38;2;127;163;212` | `38;5;110` | `34` (blue) |
| vert `#3E7A54` | Success verbs only | `38;2;107;163;131`* | `38;5;65` | `32` (green) |
| gules `#A4433B` | Error verbs only | `38;2;200;106;97`* | `38;5;131` | `31` (red) |
| slate | Secondary text, table headers | `2` (dim) | `2` (dim) | `2` (dim) |

\* truecolor uses the dark-theme variants (`#6BA383`, `#C86A61`) since most
terminals are dark; they hold up on light grounds too.

Gold stays scarce: one gold element per screen of output, same as one gold
accent per composition.

## Message shapes

Verbs come from the vocabulary of keeping, and every message keeps the shape
`status-verb  sentence`:

```
✓ held      Memory saved to scope project:localhold.
✗ not held  Scope team:atlas isn't registered.
            Register it with: hold admin scope register team:atlas
! watch     Embedding endpoint answered in 4.2s; searches will feel slow.
· note      Storage is local. This command sent nothing anywhere.
```

- `✓ held` in vert, `✗ not held` in gules, `! watch` in or, `· note` dim.
- Errors are two lines: what happened, then how to fix it, indented to align.
  No apologies, nothing vague, exit codes documented.
- An action keeps its name through the whole flow: `remember` reports `held`,
  `forget` reports `released` — never "Success!".

## The battlement rule

The signature divider, built from upper-half blocks in dim gold, separates
major sections in long output (`hold doctor`, `hold embeddings status`):

```
▀▀▀▀▀▀▀▀    ▀▀▀▀▀▀▀▀    ▀▀▀▀▀▀▀▀    ▀▀▀▀▀▀▀▀
```

Groups of eight `▀` with four spaces, repeated to ~44 columns. One per section
break, exactly like `.lh-rule` on the web. ASCII fallback: `====    ====`.

## Tables: ledger style

No box-drawing borders. Dim uppercase headers, left-aligned columns, two-space
gutters, values in default ink, identifiers in azure:

```
SCOPE                 MEMORIES  VECTORS  LAST WRITE
project:localhold          412      412  2026-07-12
user:jeff                  118      118  2026-07-13
team:atlas                   7        0  2026-06-30
```

Numbers right-aligned. A missing value is `—`, never blank.

## Progress

Quiet. A single line that rewrites itself, ending in the status verb:

```
reindexing vectors  412/1024
```
becomes, on completion:
```
✓ held      1024 vectors rebuilt in 2m 14s.
```

No spinner menageries, no percent theater for sub-second work. Anything under
one second prints nothing until it's done.

## Copy rules

Sentence case, plain verbs, no exclamation marks. Name what the person
controls (`scope`, `memory`, `endpoint`), not internals. Durations and counts
are exact (`2m 14s`, not "a while"). The hold is calm: report, don't dramatize.
