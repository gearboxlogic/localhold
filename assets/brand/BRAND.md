# LocalHold brand

**Searchable context that stays yours.**

The identity is built on one idea: a *hold* — ground you keep. Everything in the
system comes from the hold's own world (heraldry, stonework, battlements) rather
than contemporary SaaS styling. The goal is **unique**, not sleek: when a choice
is between generic-modern and castle-flavored, pick the castle.

## Marks

| File | Name | Role |
|---|---|---|
| `keep.svg` | The Keep | Primary mark. A solid tower, door standing **open** — the hold has no lock, because there is no lock-in. |
| `bastion-plan.svg` | The Plan of the Hold | Secondary mark. A pentagonal star fort in plan view — solid like the Keep, courtyard carved out, gate open through the south wall, counterscarp walls leaving the approach clear, one gold point held at center. Banners, stickers, large decorative use. |
| `icon.svg` | App icon | The Keep in argent on an azure field. Favicons, avatars, launchers. |
| `lockup.svg` | Lockup | The Keep + wordmark. Headers and docs. |
| `banner.svg` | README banner | 1280×320 hero for GitHub. |
| `social-card.svg` | Social card | 1280×640 OG image for the repo's social preview. |
| `sticker-hex.svg` | Hex sticker | Standard pointy-top hex die-cut; the fort inside its own walls. |
| `rule.svg` | Battlement rule | Standalone crenellated divider for README/docs section breaks. One per break. |
| `exports/` | Raster exports | `favicon.ico` (16/32/48) and icon PNGs at 16/32/180/512. |

Rules:

- Marks are ink (`currentColor`) plus at most one gold accent (`var(--lh-or)`).
  Never recolor the gold; never add a second accent.
- Clear space around the Keep: at least the width of one merlon (1/4 mark width).
- Minimum sizes: Keep 16 px; Plan of the Hold 48 px with its counterscarp
  strokes, 24 px without them (drop the four detached strokes, keep the fort).
- Both marks are solid, never line-drawn. The Plan speaks in two tinctures:
  argent when it is the subject (stickers, standalone use), azure when it is
  the ground behind the lockup (card, banner). Never outline it.
- The door stays open. Never add a lock, keyhole, or shield overlay to the
  marks. On the Plan this is the south gate and the open approach.

## Tinctures

The palette is named after heraldic tinctures. Code and docs use these names.

| Token | Hex | Blazon | Use |
|---|---|---|---|
| `--lh-sable` | `#1C2733` | sable (black) | Ink, text, solid marks on light |
| `--lh-azure` | `#1F3A5F` | azure (blue) | The field: links, icon tile, emphasis |
| `--lh-or` | `#C89B3C` | or (gold) | **The** accent. One per composition. |
| `--lh-argent` | `#F2F4F6` | argent (white) | Light ground, marks on dark |
| `--lh-night` | `#10161E` | — | Dark ground |
| `--lh-gules` | `#A4433B` | gules (red) | Errors, destructive actions only |
| `--lh-vert` | `#3E7A54` | vert (green) | Success only |

Gules and vert are semantic, never decorative. Gold is the voice of the brand;
if gold is fighting the ground, use less gold, not brighter gold.

## Type

| Role | Face | Notes |
|---|---|---|
| Display | Iowan Old Style → Palatino → Georgia stack | Lapidary serif — stone-cut, not fashionable. Headings, wordmark, pull quotes. |
| Body | System sans | Quiet delivery; the display face carries the character. |
| Code / labels | JetBrains Mono → ui-monospace stack | LocalHold is a dev tool; the mono face is a first-class citizen. Uppercase labels get `0.12em` tracking. |

If webfonts are ever vendored, the recommended pairing is **Alegreya**
(display) + **Alegreya Sans** (body), both SIL OFL — same chiseled humanist
character, self-hostable.

## Shape and structure

- Radii are cut stone: 2 / 4 / 8 px. Never pills, never fully rounded avatars.
- No gradients, no drop shadows heavier than a 1 px line can do.
- The signature structural device is the **battlement rule** (`.lh-rule` in
  `tokens.css`): a merlon-notched divider used at major section breaks. One per
  break; it loses its power when it becomes wallpaper.
- Dividers, borders, and tables use hairlines in `--lh-line`.

## Voice

- The brand line is “Searchable context that stays yours.” Variants may use
  “your context, held.”
- Vocabulary of keeping: *remember, recall, hold, keep, yours*. Avoid cloud
  vocabulary (*sync, seamless, unified platform*) and fortress-of-war vocabulary
  (*military-grade, impenetrable*) — the hold is calm, not besieged.
- Plain verbs, sentence case, no exclamation marks. Specific beats clever.

## The rest of the system

- **`tokens.css`** — everything above as custom properties. Style through the
  semantic tokens (`--lh-bg`, `--lh-ink`, `--lh-accent`, …), not raw tinctures,
  and both light and dark themes come for free.
- **`cli.md`** — terminal output design: ANSI tincture mapping with fallbacks,
  message shapes, ledger tables, the battlement rule in text. Preview with
  `cli-preview.sh`.
- **`icons/`** — the twelve-glyph pictogram set and its grid rules. Gold always
  means *the memory*.
- **`diagrams.md`** — diagram language for docs, with `diagram-example.svg`.
