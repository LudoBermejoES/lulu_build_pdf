# Harden PDF correctness and truthful reporting

## Why

A full code review of the shipped `prepare-pdf-for-lulu` implementation found that the tool's central promise — "hand it any PDF and get back files Lulu will accept, with an honest report of what could not be fixed" — does not hold for several common, entirely legal input shapes. The findings were reproduced against the built binary, not inferred.

Three of them are severe enough to cause a paid print run of wrong files, while the report says `print-ready` and the process exits `0`:

1. **Every glyph and image is silently dropped whenever a page's `/Resources` is an indirect reference or inherited from the `Pages` node.** Page nesting reads only the page's *direct* `/Resources` dictionary and substitutes an empty one otherwise, so the generated form XObject's content still says `/F1 Tf` and `/Im0 Do` while no such resource exists. Both shapes are what essentially every real producer emits (LaTeX, Word, InDesign, Chrome, Ghostscript). Verified: a page whose font lives behind `/Resources 4 0 R` normalizes to a form with `/Resources <<>>` and content `BT /F1 24 Tf … (HELLO WORLD) Tj ET` — a blank page, reported print-ready. The reason the existing 295 tests miss this is that every committed fixture writes `/Resources` as a direct dictionary, the one shape that works.

2. **`normalize_interior` preflights only its own output, and nesting hides content from every content-level check, so blocking findings disappear.** `NormalizeOutcome::report` is documented as repeating "any finding it could not fix… rather than silently dropping" it. The opposite happens: after nesting, `check_font_embedding` (which reads only page-level `/Font`) and `check_colour_and_ink` (which scans only the page's own content stream) can no longer see anything. Verified on the repo's own fixture: `check unembedded_font.pdf` reports a blocking unembedded Helvetica and exits 1; `interior` on the same file reports `print-ready`, exits 0, and writes a file that still contains that unembedded font.

3. **A `/Rotate` that is a real number or an indirect reference is read as zero, so the whole book prints sideways with no finding.** `as_i64` accepts only `Object::Integer`, and the failure is swallowed by `unwrap_or(0)`. Verified: a page carrying `/Rotate 90.0` is measured as 8.5 × 11 in instead of its displayed 11 × 8.5 in, which both mis-reports the geometry and bakes no rotation.

Beyond those, the review found a **372-byte PDF that hangs the tool forever** (a `/Parent` cycle in the inheritance walk, which has no visited-set or depth cap), **cover trim and safety guides that are wrong by 0.75 in for case wrap**, a **panic on a zero-page supplied cover**, a **`book --json` document that is not parseable JSON**, and roughly twenty smaller silent-failure and contract defects. Several capabilities are also wired to nothing: `pod_package_id`'s legacy-SKU deprecation notice, `interior_safety_margin`, `spine_too_narrow_for_text`, and `FitMode::StretchMargins`, which is an undocumented alias for `Center`.

The common thread is not sloppy arithmetic — the geometry formulas, matrix algebra, and spread-splitting math all verified correct. It is that **PDF object access is written for the shape the test fixtures happen to have**, and that **errors are swallowed at the exact points where a finding should have been raised instead**. Both are fixable as classes rather than one instance at a time.

## What Changes

- **Resolve PDF objects the way the spec allows them to be written, not the way fixtures happen to be.** One shared accessor for a page's effective resources (direct, indirect, and inherited, merged down the `Pages` chain), and dereferencing for `/Rotate`, page boxes and their elements, the catalog's `/Names` tree, and a form XObject's own `/Resources`. Every place that currently substitutes an empty dictionary or a zero on failure instead raises a blocking finding.
- **Make the self-preflight mean something.** Preflight the *input* as well as the output and carry forward unfixed findings, and teach the font, colour, and structure checks to descend into form XObjects so the crate's own output is actually inspectable.
- **Bound every walk over attacker-controlled structure.** A visited-set and depth cap on the `/Parent` inheritance walk, a depth budget and an operation budget on deep copying and the content-stream walker, and a real timeout plus concurrent pipe draining on the qpdf and Ghostscript invocations that currently have neither.
- **Fix cover trim geometry and the guides drawn from it.** Derive the trim rect from the product's trim size within the canvas rather than a hardcoded bleed inset, inset safety margins from the trim rather than the panel, refuse degenerate insets, and actually emit the narrow-spine warning.
- **Refuse or report degenerate and hostile input instead of producing nonsense.** No `NaN` may reach a content stream; a zero-page supplied cover, a non-UTF-8 path, and an unreadable `--doc-id` become errors with the documented exit code rather than panics.
- **Honour the CLI's own documented contracts.** One parseable JSON document per run, the specific exit code the write path already computed, an error on an invalid config value instead of a silent fall-through to a default, and `--gutter-floor-in` either wired up or removed.
- **Close the loop on unwired capabilities**: surface the legacy-SKU deprecation notice, check interior content against the safety margin, and either implement `StretchMargins` or reject it as unimplemented.
- **Extend the fixture corpus to cover the shapes that hid these bugs** — indirect and inherited resources, indirect boxes and `/Rotate`, a `/Parent` cycle, a reference chain, a zero-page file, degenerate sizes, an aliased page, and a `DCTDecode` image — so that this class of defect fails a test next time.

## Impact

- Affected specs: `pdf-preflight`, `interior-normalization`, `cover-preparation`, `external-tool-pipeline`, `lulu-api-verification`, `cli`
- Affected code: `crates/lulu-prep/src/{pdf,preflight,normalize,cover,ctm_walk,external_tools,pipeline,geometry,report,icc,lulu_api,units}.rs`, `crates/lulu-prep-cli/src/{main,commands,config,output_paths}.rs`, plus the fixture generator and test corpus
- **Behavioural change users will notice:** files that previously reported `print-ready` and exited 0 will now correctly report blocking findings and exit 1 — most visibly any file whose fonts were being silently dropped, and any file with an unembedded font run through `interior`. This is the point of the change, but it means output that looked clean before will start failing, and the reports are not comparable across the change.
- Snapshot tests will need regeneration once the checks see through form XObjects; that diff is the evidence the fix works, so it should be reviewed line by line rather than blanket-accepted.
