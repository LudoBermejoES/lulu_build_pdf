# Design

## Context

The implementation being corrected here is not structurally wrong. The review verified, by re-deriving them, that `units.rs`'s matrix algebra, `rotation_bake`'s three non-identity cases, `split_spread_pages`'s half-widths and clipping, the five-band gutter table, the perfect-bound spine formula, and `case_wrap_geometry`'s live-verified `2·trim + spine + 2×0.875 in` are all correct. Credential handling, the retry policy, and the absence of shell interpolation in subprocess arguments also checked out.

What failed is narrower and more systematic: **the code reads PDF objects in the single shape the test fixtures write them**, and **swallows the failure when they are written another legal way**. `get_page_resources(..).ok().and_then(|(r, _)| r).cloned().unwrap_or_default()` is the archetype — it discards the `Vec<ObjectId>` that holds exactly the indirect and inherited dictionaries, then substitutes `<<>>`. The same expression appears at five call sites. `as_i64().unwrap_or(0)` on `/Rotate` is the same mistake against a different accessor.

That framing drives the whole design: fix the accessors once, in one place, and make the fallback a finding rather than a default.

## Goals / Non-Goals

**Goals**
- No input, however legally exotic, may produce a file that is reported `print-ready` while missing content or carrying a defect the tool claims to check.
- No input may hang the tool or abort the process.
- Every check the tool advertises must be able to see the crate's own output.
- One accessor per PDF concept, so a shape fix lands everywhere at once.

**Non-Goals**
- Not adding new print capabilities. Linen-wrap/dust-jacket geometry stays refused; ICC and Ghostscript stay optional and off by default.
- Not rewriting content streams. Nesting under a transform remains the mechanism; only resource resolution changes.
- Not pursuing byte-for-byte parity with Lulu's own normalizer. Lulu remains authoritative; this tool predicts.
- Not chasing pathological-but-harmless input (e.g. `/Rotate 45`) into a rendering feature — it becomes a finding, not a supported layout.

## Decisions

### Resource resolution becomes one accessor, and its failure is a finding

Add `pdf::effective_page_resources(doc, page_id) -> Result<Dictionary, PageGeometryError>` that resolves the page's own `/Resources` whether direct or indirect, walks the `/Parent` chain for the inherited case, and merges child over parent (child wins per key, matching PDF inheritance). All five current call sites — `normalize::nest_page`, `normalize::split_spread_pages`, `cover::copy_page_as_form`, `cover::extract_panel_as_preview_page`, `ctm_walk::walk_page_images`, and `preflight`'s resource reads — go through it.

The alternative — fixing each site locally — was rejected: five copies is how this drifted in the first place, and the review found the same expression already behaving inconsistently between `ctm_walk`'s XObject lookup (which does handle the indirect case) and its form-resources lookup (which does not).

Critically, **"no resolvable resources but a non-empty content stream" becomes a blocking finding**, not an empty dictionary. A content stream that names resources which cannot be resolved is exactly the blank-page case, and it must fail loudly.

### Preflight gains a form-XObject-aware mode rather than a second implementation

`check_font_embedding` and `check_colour_and_ink` currently inspect only page-level resources and the page's own content stream, which is why nesting hides everything. Rather than writing separate "nested" variants, both grow a descent over form XObjects, reusing `ctm_walk`'s existing traversal (which already walks forms, tracks the CTM, and has a depth guard) so there is one traversal implementation and not two that can disagree.

This is what makes the self-preflight honest, and it is also why the snapshot diff for this change will be large: checks that silently saw nothing will start seeing real content.

### `normalize_interior` preflights input and output, and reports the difference

Preflighting only the output cannot distinguish "fixed" from "invisible". The function will preflight the input, preflight the output, and report the output's findings plus any input finding that is *not* fixable by this tool (an unembedded font is the canonical case: normalization cannot embed a font, so that finding must survive into the report and force exit 1).

Deduplication is by finding code plus page set, so a genuinely fixed finding does not linger and a genuinely unfixed one is not doubled.

### Bounded traversal everywhere, with a named budget

Three separate unbounded walks were found. Rather than three ad-hoc guards, each gets an explicit, documented budget in one place: `/Parent` inheritance takes a visited set (matching lopdf's own `Error::ReferenceCycle` behaviour in `get_page_resources`), `deep_copy_object` converts from recursion to an explicit worklist so a long reference chain cannot overflow the stack, and `ctm_walk` gains a total-operation budget on top of its existing depth cap, since 32 levels of two-forms-each is 2³² walks from a small file.

Exhausting a budget is a blocking finding, never a silent truncation — otherwise it degrades into the same class of bug being fixed.

### Cover trim geometry derives from the product, not from a bleed constant

`Rect::from_origin_size(geo.canvas).inset(geometry::bleed())` is only correct for perfect binding. Case wrap's canvas carries a 0.875 in overhang per side, so the trim edge sits 63 pt inside the canvas, not 9 pt. `CoverGeometry` will carry its trim rect explicitly, computed by whichever geometry builder made the canvas (which is the only code that knows the overhang), and the template's guides and the page's `TrimBox`/`ArtBox` will both read it from there. Safety margins inset from that trim rect rather than from the panel.

`Rect::inset` will additionally refuse to produce an inverted rectangle (`x0 > x1`), which is what currently lets the template draw a mirrored spine-safety box straddling both neighbouring panels.

### Degenerate numbers are rejected at the boundary, not formatted into the PDF

A zero-width page currently yields `inf` scale, `NaN` translation, and a literal `q NaN NaN NaN NaN NaN NaN cm` in the output. Two layers: `fit_placement` refuses a non-finite or non-positive dimension, and — as a backstop, because this class recurs — every `cm` operand is asserted finite immediately before it is written. `Length` also gains a guard against `NaN` construction, which contains the class at its origin.

### CLI contracts are fixed to match what is already documented

No new CLI surface. `book` emits one document (`{interior, cover}`) so `--json` stays parseable and `--report-out` stops truncating; `write_output`'s already-computed exit code is propagated instead of being flattened to 2; an invalid config value fails with exit 2 rather than silently falling through to a default; the default cover filename uses the full SKU rather than `file_stem`, which currently drops `.MXX` and collides gloss with matte; and `--gutter-floor-in` is wired into `gutter_allowance` (it is currently accepted, resolved, printed by `--print-config`, and read by nothing, which actively contradicts the findings the tool emits).

### Dead capabilities are connected or removed, not left ambiguous

`pod_package_id`'s legacy-SKU `DeprecationNotice` (Lulu's legacy form sunsets 2027-02-01), `interior_safety_margin`, and `spine_too_narrow_for_text` are each fully implemented and tested but called from nothing outside their own tests. Each is either wired to its report or deleted; leaving tested-but-unreachable code is worse than either, because it reads as coverage.

`FitMode::StretchMargins` is documented as filling the bleed with a flat colour and is in fact an alias for `Center`, silently. It gets implemented or rejected as unimplemented — a flag that quietly does something other than what its help text says is the same honesty failure as a report that quietly drops a finding.

## Risks / Trade-offs

**Files that used to pass will now fail** → That is the correction, but it is a real behavioural break: anyone with a green CI check against this tool may go red. The proposal calls this out; the tasks require the snapshot diff be reviewed rather than blanket-regenerated, since that diff is the primary evidence the blind spots are gone.

**Merging inherited resources could change rendering for files that currently happen to work** → A page whose direct `/Resources` shadows an inherited key must keep the direct value. Child-over-parent merge order is specified for exactly this, and needs a test with a deliberately conflicting key rather than only additive cases.

**Descending into form XObjects makes preflight slower on deeply nested files** → Bounded by the same operation budget added for safety, so the worst case is a finding rather than a hang.

**Making unresolvable resources blocking could reject files a lenient viewer would render** → Deliberate. A resource a viewer guesses at is a resource a RIP may guess differently, and this tool exists to predict rejection, not to hope.

## Migration Plan

The fixes are independent enough to land incrementally, but ordering matters: the shared accessors (`pdf.rs`) land first, since the normalization, cover, and preflight fixes all depend on them; the preflight descent lands before the input/output reporting change, so the honest report has working checks to draw on; the fixture corpus grows alongside each fix rather than at the end, since each new fixture is the proof for one finding.

No data migration and no persisted state. Reports carry `schema_version`, which stays as-is because no field changes shape — only which findings appear.

## Open Questions

- **How should an unresolvable-resource finding distinguish "empty page" from "broken page"?** A page with an empty content stream and no resources is legitimately blank (the padded pages this tool itself adds are exactly that). The check must key on "content stream names a resource that does not resolve", not merely "resources are empty", and the precise operator-name extraction for that is not yet specified.
- **Whether `--gutter-floor-in` should be able to raise the applied gutter or only the advisory threshold.** Wiring it to the advisory threshold is the conservative reading and matches the flag's help text; letting it change the applied shift would make it a geometry override, which is a different feature and would need its own scenario.
- **Whether `DCTDecode` and friends should be re-encoded or passed through on deep copy.** Pass-through (keep the original filter and bytes) is correct and lossless, and is what the tasks specify; re-encoding would need a decoder this crate deliberately does not have.
