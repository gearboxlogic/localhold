# Icon set

Pictograms for docs and UI, drawn in the marks' language. Twelve glyphs mapping
to LocalHold's own vocabulary:

`memory` `remember` `recall` `forget` `scope` `history`
`doctor` `embeddings` `rerank` `transport` `config` `handoff`

## Grid rules (for new icons)

- 64×64 viewBox, live area roughly 8–56, optical center at (32, 32).
- Stroke: `currentColor`, width 4, `stroke-linecap="square"`, miter joins —
  cut stone, not rounded-friendly. Hairline connectors may drop to 2.5–3.
- **One gold element at most** (`var(--lh-or, #C89B3C)`), and it always means
  the same thing: *the memory* — the thing being held, found, or moved.
  Absence of gold is meaningful too (`forget` has none; the memory is gone).
- Arcs and battlements are the shape vocabulary; avoid circles-with-badges,
  gears, and clouds.
- Minimum rendered size 20 px; below that, use the Keep mark instead.

Verify new icons the same way the set was built: rasterize at 150 px and 18 px
and look at them before committing.
