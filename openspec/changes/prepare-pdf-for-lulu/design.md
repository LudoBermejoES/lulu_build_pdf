## Context

The project is empty: no Cargo workspace, no code. Everything here is greenfield, so the design's job is to pick the boundaries that will hold once the PDF-manipulation details get messy.

The domain constraints come from Lulu's own published documentation, and they are unusually concrete. Bleed is 0.125 in per side, so the page must be trim plus 0.250 in in each dimension. Every interior page must be the same size — Lulu's file validation rejects mixed sizes outright, along with unembedded fonts, corrupt images, and interiors under two pages. Interior safety margin is 0.500 in; gutter follows a five-band table keyed on page count. Perfect-bound spine width is `pages / ppi + 0.06 in` with ppi taken from the SKU's paper code (444 for standard papers, 460 for magazine and comic stock); hardcover spine comes from a 28-row stepped table. Cover canvas for perfect binding is `2 × trim_width + spine + 0.250 in` by `trim_height + 0.250 in`. Security, trim marks, and bleed marks are prohibited. The `pod_package_id` encodes trim, ink, quality, binding, paper, and finish, and Lulu is mid-migration from a 27-character undotted form to a dotted one, with legacy support ending 2027-02-01.

Two facts shape the architecture more than anything else. First, Lulu publishes a machine-readable product spec sheet — 3,277 SKUs with trim size, size-with-bleed, page-count limits, paper, and PPI — which means product knowledge can be data rather than code. Second, Lulu runs its own normalizer server-side, so this tool does not need to be a full prepress engine. Its value is in getting geometry and page count exactly right, and in telling the user what is wrong *before* they upload, which is the part Lulu's pipeline does not do until it is too late.

Against that, the honest limit: transparency flattening and faithful sRGB→CMYK conversion of vector content are not reasonable to reimplement in Rust. This design treats them as delegated stages, not core features.

## Goals / Non-Goals

**Goals:**

- One command turns an arbitrary PDF plus a `pod_package_id` into a Lulu-conformant interior and a correctly-sized cover.
- Every geometric fact traces to one place — the catalog or the geometry module — and to a Lulu-published figure, verified by tests against Lulu's own worked examples.
- Full usefulness with zero external binaries and zero network access; both are strictly additive.
- Findings are structured and stable enough to be diffed, suppressed by code, and consumed by CI.
- Vector content stays vector. No stage silently rasterizes or resamples.
- Reusable library boundary, so the CLI is not where logic lives.

**Non-Goals:**

- Not a prepress engine. No native transparency flattening, no native vector colour conversion, no overprint simulation, no trapping.
- No typesetting, reflow, or editorial layout. Content arrives as given.
- No ordering, pricing, or payment through the Print API, even though the catalog carries price columns.
- No GUI and no PDF viewer.
- No repair beyond delegating to qpdf.
- No support for print targets other than Lulu in this change.

## Decisions

### Two crates: `lulu-prep` library, `lulu-prep-cli` binary

A Cargo workspace with the domain logic in a library and a thin binary over it. The alternative — one crate with a `main.rs` — is faster to start and was rejected because the geometry and catalog logic is exactly the part worth testing in isolation and reusing later, and because a library boundary keeps `clap` types out of the domain. The CLI's own job is argument parsing, configuration precedence, report rendering, and exit codes; it should contain no arithmetic.

### The product catalog is vendored data, not Rust constants

Lulu's spec sheet is downloaded, converted to a CSV committed at `crates/lulu-prep/data/pod-packages.csv`, and embedded with `include_str!`, parsed once behind a `OnceLock`. A regeneration script (`xtask` or a shell script) refetches and rewrites it, and the CSV carries a header comment with the source URL and fetch date, surfaced in every report.

Alternatives rejected: hand-written Rust `const` tables (3,277 rows; drifts silently and no provenance), and fetching at runtime (breaks the offline goal and makes results non-reproducible). Prices are kept in the CSV because they come free with the source, but nothing reads them in this change — dropping columns would make regeneration a lossy transform.

A generated-at-build-time table via `build.rs` was also rejected: it hides the diff. A committed CSV means a catalog refresh shows up as a reviewable change.

### Lulu-published formulas live in one geometry module, tested against Lulu's own numbers

`geometry.rs` owns bleed, safety margins, the gutter band table, the spine formula and hardcover spine table, page-count rules, and cover canvas composition. Nothing else computes a dimension. Tests assert Lulu's published worked examples directly: 6 × 9 in → 6.25 × 9.25 in page; 210 pages perfect-bound at 444 ppi → 0.533 in spine; that same book's cover → 920 × 666 pt, the figure in Lulu's `cover-dimensions` documentation; 210 pages case wrap → 0.750 in from the table's 195–222 band. A cross-check test asserts derived size-with-bleed agrees with the catalog's own bleed columns for all 3,277 rows, which catches both a broken formula and a corrupted regeneration.

Units are a real hazard here: Lulu documents inches, PDF works in points, and the catalog carries both inches and millimetres. The module uses a single `Length` newtype stored in points, with explicit `from_inches` / `from_mm` constructors and no bare `f64` in any public signature.

### Hardcover cover dimensions are never computed from an inferred formula

Perfect-bound cover geometry is verified against Lulu's published figure, so it is computed. Hardcover case wrap and linen wrap are not: Lulu documents the *components* — 0.750 in wrap allowance, 0.250 in hinge either side of the spine, board overhang of 0.125 in at the fore edge and top and bottom — but not an authoritative composition, and its own guidance is to download a per-page-count template. So hardcover geometry comes from a checked-in table transcribed from those templates, or from the `cover-dimensions` endpoint, and a locally inferred value is marked unverified and refused as the basis for a final file.

This is deliberately a hard failure rather than a best guess. A wrong hardcover spine produces a cover that visibly does not fit, discovered after paying for print.

### `lopdf` for the object model; page nesting rather than content rewriting

`lopdf` 0.44 gives read-write access to the PDF object graph, which is what nearly every operation here needs: reading page boxes, editing them, removing dictionary entries, appending pages, decrypting empty-password files.

Geometry normalization wraps each source page as a form XObject on a fresh page under an affine transform, rather than rewriting the page's content stream. Nesting is a small, well-understood operation: it preserves vector content and image data exactly, needs no content-stream parser, and composes cleanly with rotation, spread splitting, and the gutter shift, since all three are just different matrices. Rewriting content streams would mean reimplementing the graphics state machine to get transforms right, for no gain.

The exception is the preflight image-resolution check, which genuinely needs the CTM at each draw site and therefore a read-only content-stream walker tracking `cm`/`q`/`Q` and descending into form XObjects. That walker is read-only and confined to preflight, so a bug in it degrades a warning rather than corrupting output.

`printpdf` and `pdf-writer` were considered for the whole job and rejected: both are writers, and most of this work is transforming an existing document. `pdf-writer` 0.15 may still be used for the generated cover template, which is a pure-write task.

### External tools are subprocesses, discovered at runtime, never linked

Ghostscript and qpdf are invoked as child processes found on `PATH` or at a configured path, probed once per run for their version, and recorded in the report along with the exact argument list used.

Ghostscript is AGPL, so subprocess invocation is the only acceptable coupling — linking would put the whole binary under AGPL. The `qpdf` crate exists (0.3.6) and would link libqpdf, but shelling out keeps qpdf optional and avoids a C++ build dependency, which matters more than the ergonomics of a Rust API for two operations.

`pdfium-render` was considered as a self-contained rasterizing fallback for pages that cannot be fixed vectorially, and rejected for this change: it makes flattening lossy in a way the user cannot see, and adds a large native dependency to serve a case Lulu's own normalizer usually handles.

The pipeline is a fixed ordered list of stages — repair, spread split, geometry, gutter, padding, sanitation, then optional flatten and colour convert — with delegated stages last, so nothing external can change which rectangle is the trim. Every stage reports what it did; unavailable stages report what they would have fixed.

### Native ICC conversion for images only, via `lcms2`

`lcms2` 6.2 (Little CMS bindings, MIT) handles image-only colour conversion for the case where a caller wants CMYK images without letting Ghostscript rewrite the whole document. This stage must state in the report that vector colour was left alone — a half-converted document presented as converted is worse than no conversion.

### Findings are a typed enum with stable string codes

Every check produces a `Finding { code, severity, message, pages, observed, expected, fixable }`. Codes are stable strings (`geometry.page-size-mismatch`, `fonts.not-embedded`, `structure.encrypted`) so reports diff cleanly and users can suppress by code. Three severities only: `blocking` for what Lulu rejects, `warning` for what degrades quality or leans on Lulu's normalizer, `info` for observations. More severities would invite argument over placement without changing any decision.

Normalization re-runs preflight on its own output and folds the result into its report, which makes "did this actually work" a property of the run rather than a separate step the user has to remember. It also makes idempotence directly testable.

### Reports are serde structs; text rendering derives from JSON

One `Report` struct serialized with `serde_json` for the machine form, and a renderer producing the text form from the same value. The text output can therefore never claim something the JSON does not. The JSON carries a schema version, the input digest, the resolved product, the catalog fetch date, and the tool version — enough to reconstruct why a verdict was reached months later.

### The Print API sits behind an off-by-default feature

`reqwest` and the OAuth client-credentials flow live behind a `lulu-api` Cargo feature that is off by default, so the default build has no HTTP client and cannot make a network call. Credentials come from the environment or a config file, never from argv, and never appear in reports or errors.

The API is used for exactly three things: `cover-dimensions` as an authoritative cross-check (and as the source for hardcover geometry), `validate-interior`, and `validate-cover`. Both validations need a publicly downloadable URL, which the tool cannot provide — so when no URL is supplied it skips file validation, says why, and still does the dimension check, which needs no URL. Lulu's asynchronous statuses (`NULL`, `VALIDATING`, `VALIDATED`, `NORMALIZING`, `NORMALIZED`, `ERROR`) are polled with a bounded timeout, and on timeout the job identifier is reported rather than a fabricated verdict.

Where Lulu's error overlaps a local finding — mismatched page sizes, unembedded fonts, under two pages, page size not matching the SKU — the report links the two. That overlap is the tool's own scoreboard: the local checks exist precisely to predict those four.

### Testing strategy

Four layers. Unit tests on geometry against Lulu's published numbers, including boundary page counts for every gutter and spine band. Property tests asserting that normalized output always has uniform pages at the required size, a conformant page count, and correct boxes, over generated inputs of varying size, rotation, and count. Golden-file tests over a small corpus of committed fixture PDFs — no bleed, has bleed, mixed sizes, rotated, encrypted with empty password, unembedded font, low-resolution image — snapshotted with `insta` on the JSON report with volatile fields excluded. And an idempotence test asserting normalization of normalized output is a no-op, which is the cheapest guard against the whole class of double-application bugs.

Fixtures are generated by a committed script rather than committed as opaque binaries where possible, so a reviewer can see what a fixture is meant to contain.

## Risks / Trade-offs

**Lulu changes its specifications, silently invalidating vendored data and formulas** → Provenance is recorded in the catalog and echoed in every report, so a stale result is identifiable rather than merely wrong. The optional `cover-dimensions` cross-check catches formula drift against Lulu's live answer, and is worth running before any large print order.

**The dotted-SKU migration lands mid-project; legacy support ends 2027-02-01** → Both forms parse to one internal descriptor from the start, and legacy input carries a deprecation notice naming its dotted equivalent. The catalog CSV keeps both columns, so neither form needs a lookup table built at runtime.

**Hardcover geometry cannot be derived from published figures** → Refuse rather than guess: unverified estimates are labelled and cannot produce a final file. The cost is that hardcover output needs either the template table or API access; the alternative cost is unusable printed books.

**`center` fit leaves an unprinted 0.125 in border when the source has no bleed** → This is inherent, not a bug: content that was never drawn cannot be invented. `center` is the default because it never moves content relative to the trim edge; `scale-to-bleed` and `stretch-margins` are offered for callers who prefer enlargement or edge extension, and the report always states which was used and what it cost.

**The gutter shift double-applies on a source that already has a gutter** → Off by default, and the report states plainly when it was not applied. Enabling it warns when the shift pushes content past the trim.

**The image-resolution CTM walker is the most error-prone code here** → It is read-only and confined to preflight, so its failures degrade a warning rather than corrupt a file. Nested form XObjects get explicit fixtures, and the check reports the minimum effective resolution found so a wrong answer is visible in one line.

**Ghostscript's AGPL licence could contaminate distribution** → Subprocess-only invocation, never linked, always optional, and documented as a user-installed dependency. Nothing in the crate's dependency graph is AGPL.

**Ghostscript may alter page geometry as a side effect** → A post-stage assertion checks that every page's `MediaBox` and `TrimBox` are exactly the values normalization set and the page count is unchanged, failing the run if not. Delegated stages run last for this reason.

**A generated cover template could be submitted to Lulu with its guides visible** → Guides live in one named optional content group, and the template says in both its legend and its metadata that it is a design aid, not a submittable file.

**Silent misprediction — the tool passes a file that Lulu then rejects** → The four overlap cases are tested directly, and the report links Lulu's errors to local finding codes so any miss is visible. Each such miss should become a fixture.

## Migration Plan

Not applicable: greenfield project, no existing users, no data to migrate. The one forward-looking concern is the SKU format transition, handled above by accepting both forms from the start.

## Open Questions

- **Hardcover template table coverage — resolved for case wrap, open for linen wrap.** Live probes against Lulu's production `cover-dimensions` endpoint (2026-09-03) across two trim sizes (6x9in, A4) and five page counts (24, 100, 212, 400, 800) confirm case wrap's composition is `canvas_width = 2*trim_width + spine_width + 2*0.875in`, `canvas_height = trim_height + 2*0.875in`, with a 0.25in hinge either side of the spine — 7 of 8 probes matched to the point, the eighth within 0.5 pt (a rounding artifact, not a formula error). This is now a Lulu-confirmed formula in `crates/lulu-prep/src/cover.rs` (`case_wrap_geometry`), not a locally inferred one, and the empty `HARDCOVER_TEMPLATE_TABLE` is no longer needed for case wrap — its transcribed-entry count is **zero**, recorded directly on the constant in `cover.rs`, and it exists today only as the (still unimplemented) landing spot for linen wrap's eventual per-page-count data. Linen wrap is a **dust jacket** — a probe of `0600X0900.BW.STD.LW.060UC444.GBB` at 100 pages returned 1458 x 702 pt, nothing like case wrap's ~1026 x 774 pt at the same count, consistent with a flap-based panel layout (front flap, front, spine, back, back flap) rather than the simple 3-panel model this crate has. Designing that panel model and transcribing enough dust-jacket data points to trust it remains open.
- **Total-area-coverage estimation for images — decided: not sampled in this version.** Checking TAC on vector fills (`k`/`K` fill/stroke operators) is cheap and implemented in `check_colour_and_ink`. Sampling every embedded image's decoded pixels is expensive (requires decoding each image's filter chain) and was decided against for this version: the check exists to catch the common case before upload, not to replace Lulu's own normalizer, which performs the authoritative full check regardless. No uncertainty is reported because no sampling happens — image content simply is not inspected for TAC. Revisit if real-world misses show this gap matters in practice (see the "silent misprediction" risk below).
- **Default for the gutter shift on thin books — decided: stays warning-only.** Lulu's band table gives a 0.000 in gutter under 60 pages, while its PDF creation settings advise a 0.200 in minimum. `normalize_interior` never applies the 0.200 in floor itself — it always uses the banded table's value (0.000 in under 60 pages) — but now reports a `gutter.below-advisory-floor` warning finding whenever the banded value falls under the floor, naming both numbers, so the conflict is visible in every report rather than only in `geometry::GutterAllowance::below_advisory_floor`'s (previously unsurfaced) value.
- **Whether `book` should emit a combined manifest — decided: no, not in this version.** Each of `book`'s two `Report`s already carries `product_sku` and `page_count`, and the CLI prints both output paths. A combined manifest (one JSON naming both files, their digests, and the shared product/page count) would help automation that wants a single artifact to consume, but nothing in the current CLI surface forces that shape, and inventing a new schema without a concrete consumer risks guessing wrong. Revisit if a real automation use case asks for it specifically.
