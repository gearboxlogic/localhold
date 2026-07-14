# Diagram language

Rules for architecture and flow diagrams in docs, so every diagram reads as one
system. Example: `diagram-example.svg`.

## Ground

Diagrams ship on a **transparent background** with mid-value inks that stay
legible on both GitHub themes — don't use `--lh-sable` (vanishes on dark) or
`--lh-argent` (vanishes on light):

| Ink | Hex | Means |
|---|---|---|
| Slate | `#7C8DA3` | External systems, connectors, labels |
| Azure (mid) | `#5C82B8` | LocalHold's own components |
| Or | `#C89B3C` | The held data — stores, memory in flight |

## Nodes

- Cut-stone rectangles: `rx="4"`, stroke 2, no fills heavier than a 6–8%
  tint of the stroke color.
- **LocalHold components**: solid azure stroke.
- **External systems** (agents, clients, embedding endpoints): slate stroke,
  `stroke-dasharray="6 5"` — outside the walls.
- **Stores** (SQLite, PostgreSQL): the one place data rests, so the node
  wears the brand: crenellated top edge, gold stroke. At most one gold node
  per diagram, same as everywhere else.

## Connectors and labels

- Lines: 2 px, square caps, right-angle routing preferred over splines.
- Arrowheads: small open chevrons, not filled triangles.
- Labels: mono face, uppercase, 0.12em tracking, slate — placed alongside the
  line, never on top of it.
- Transport/protocol names go on connectors (`STDIO`, `HTTP`, `POST /embeddings`);
  component names go in nodes.

## Don't

- No clouds, cylinders, or stick figures — the store is a crenellated stone,
  not a database cylinder.
- No drop shadows, no gradients, no more than ~7 nodes per diagram (split it).
