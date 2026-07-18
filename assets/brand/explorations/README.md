# Plan of the Hold — parametric sources

Generator scripts and the variant library for the secondary mark. The shipped
mark is `../bastion-plan.svg`; these are its sources, kept so the mark can be
regenerated, resized, or extended without redrawing by hand.

Run the scripts from this directory (`fortgen.py`, then `round2.py`, then
`round3.py`; each writes SVGs into a subdirectory). All geometry is
parametric: curtain radius `rc`, tip radius `rt`, neck fraction `f`, shoulder
depth `s`, plus ward radius and gate/causeway gap widths. Adjust these to
produce a variant at different proportions — for example, a stubbier fort for
a constrained space — while keeping the construction rules from `../BRAND.md`.

The variant SVGs document the design space around the shipped mark: `b2c-bold`
is the chosen lineage (pentagon, counterscarp, causeway); the others show
adjacent geometry (square, hexagon, diamond stance, ravelins, full moat) and
why the pentagon holds — five-fold symmetry cannot be read as a blade, and the
curtain-dominant proportions keep it a fortification rather than a star.
