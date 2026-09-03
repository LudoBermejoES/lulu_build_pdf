## 1. Workspace setup

- [x] 1.1 Create the Cargo workspace with `crates/lulu-prep` (library) and `crates/lulu-prep-cli` (binary named `lulu-prep`), pinning the Rust edition and a `rust-toolchain.toml`
- [x] 1.2 Add core dependencies to the library: `lopdf`, `pdf-writer`, `serde`, `serde_json`, `thiserror`, `camino`, `sha2`
- [x] 1.3 Declare the off-by-default `lulu-api` feature and gate `reqwest` behind it; declare an `lcms2` feature for native image colour conversion
- [x] 1.4 Add dev dependencies: `insta`, `proptest`, and set up `cargo fmt` and `cargo clippy -- -D warnings` in CI
- [x] 1.5 Write the README skeleton stating that Ghostscript and qpdf are optional user-installed dependencies and that Ghostscript is AGPL and only ever invoked as a subprocess

## 2. Units and geometry primitives

- [x] 2.1 Implement the `Length` newtype stored in points, with `from_inches`/`from_mm`/`from_points` constructors and `as_*` accessors, and no bare `f64` in public signatures
- [x] 2.2 Implement `Rect` and `Size` over `Length`, plus inset/outset, centring, and PDF-array conversion helpers
- [x] 2.3 Implement an affine `Matrix` type with translate, scale, rotate, concatenation, and emission as a PDF `cm` operand list
- [x] 2.4 Unit-test that 6 × 9 in equals 432 × 648 pt exactly and that inch/mm/point round-trips stay within 0.001 pt

## 3. Product catalog

- [x] 3.1 Write the regeneration script that downloads Lulu's spec sheet from `https://assets.lulu.com/media/specs/lulu-print-api-spec-sheet.xlsx` and emits `crates/lulu-prep/data/pod-packages.csv` with a header comment carrying the source URL and fetch date
- [x] 3.2 Commit the generated CSV covering all 3,277 products with both SKU forms, book type, min/max page, trim size in inches and mm, size with bleed in inches and mm, ink, quality, binding, paper type, interior PPI, lamination, linen colour, and foil colour
- [x] 3.3 Implement the catalog loader: `include_str!` the CSV, parse once behind a `OnceLock`, expose lookup by either SKU form and a filtered search
- [x] 3.4 Expose catalog metadata — source URL, fetch date, product count — for report headers
- [x] 3.5 Implement `pod_package_id` parsing for the dotted form `[Trim].[Ink].[Quality].[Binding].[Paper].[Finish]` into a structured descriptor
- [x] 3.6 Implement legacy 27-character parsing that yields the same descriptor, plus a deprecation notice naming the dotted equivalent and the 2027-02-01 end-of-support date
- [x] 3.7 Return errors for malformed IDs that name the offending segment and its position, with no partial descriptor
- [x] 3.8 Test dotted and legacy parses of `0600X0900.BW.STD.PB.060UW444.MXX` produce identical descriptors, and that unknown-but-well-formed SKUs fail with a message naming the catalog fetch date

## 4. Lulu-published geometry rules

- [x] 4.1 Implement bleed geometry: 0.125 in per side, required page size = trim + 0.250 in per dimension; test that 6 × 9 in yields 450 × 666 pt
- [x] 4.2 Add the cross-check test asserting derived size-with-bleed agrees with the catalog's own bleed columns for every product within 0.01 in
- [x] 4.3 Implement safety margins: 0.500 in interior, 0.250 in cover, 0.750 in hardcover case wrap
- [x] 4.4 Implement the gutter band table as a total function over page counts ≥ 1, with the 0.200 in advisory-floor flag, and test the 60/61, 150/151, 400/401, 600/601 boundaries
- [x] 4.5 Implement page-count rules: catalog min/max, multiple of 2 for coil and Wire-O, multiple of 4 otherwise, the API's 2-page floor, and `next_conformant_count`
- [x] 4.6 Test page-count rules for 205→208 perfect-bound, 18→32 against a 32-page minimum, 30→32 saddle stitch, and 812 refused against an 800-page maximum
- [x] 4.7 Implement perfect-bound spine width as `pages / interior_ppi + 0.06 in`, taking PPI from the SKU, and test 210 pages at 444 ppi → 0.533 in and at 460 ppi → 0.517 in
- [x] 4.8 Transcribe the 28-row hardcover spine table (0.250 in at 24–84 pages through 2.125 in at 800) and test 210 pages case wrap → 0.750 in and 20 pages → no defined spine
- [x] 4.9 Return zero spine and a no-printable-spine marker for saddle stitch, coil, and Wire-O
- [x] 4.10 Implement perfect-bound cover canvas composition and test that `0600X0900.BW.STD.PB.060UW444.MXX` at 210 pages yields 920 × 666 pt, matching Lulu's published `cover-dimensions` example
- [x] 4.11 Implement the too-narrow-for-spine-text report for spines under 0.125 in

## 5. Findings and reporting

- [x] 5.1 Define `Finding { code, severity, message, pages, observed, expected, fixable }` with the three severities `blocking`, `warning`, `info`, and a stable-string code registry
- [x] 5.2 Define the `Report` serde struct carrying schema version, input digest, resolved product, catalog fetch date, tool version, detected external tools, stage log, and findings
- [x] 5.3 Implement the text renderer derived from the same `Report` value, leading with a one-line verdict, grouped by severity, readable without colour
- [x] 5.4 Test that the verdict line reports both counts for a file with two blocking findings and five warnings, and reports readiness with the product and page count when nothing is blocking
- [x] 5.5 Test that JSON output parses, carries the schema version, and that two runs differ only in timestamps, durations, and tool versions

## 6. PDF reading and preflight

- [x] 6.1 Implement document loading via `lopdf`, including empty-user-password decryption, and a load failure path that still emits a blocking parse finding rather than aborting without a report
- [x] 6.2 Implement effective page size resolution from `TrimBox`/`BleedBox`/`CropBox`/`MediaBox` with PDF fallback order and `/Rotate` applied
- [x] 6.3 Implement the page geometry checks: wrong uniform size (blocking, fixable), mixed sizes (blocking, listing each size and its pages), rotated orientation, and a 0.5 pt tolerance
- [x] 6.4 Implement the font embedding check over `FontFile`/`FontFile2`/`FontFile3`, descending into Type0 descendant fonts, treating the standard 14 as not embedded, and accepting subset-tagged names
- [x] 6.5 Implement the read-only content-stream walker tracking `cm`/`q`/`Q` and descending into form XObjects to produce the CTM at each image draw site
- [x] 6.6 Implement the image resolution check from pixel dimensions and CTM: warn below 300 ppi and above 600 ppi, report the minimum found, emit nothing for vector-only pages
- [x] 6.7 Implement the colour and ink checks: non-DeviceGray/sRGB/CMYK spaces, Separation and DeviceN, CMYK total area coverage above 270 percent, tints below 20 percent, soft masks and non-Normal blend modes, and `/OCProperties`
- [x] 6.8 Implement the structural checks: encryption (blocking, fixable), annotations, `AcroForm` and form fields, document- and annotation-level JavaScript, embedded files, multimedia, and spread `PageLayout`
- [x] 6.9 Implement the page-count check against the product, stating the padding that would be applied, or refusing above the maximum as blocking and unfixable
- [x] 6.10 Assert in tests that preflight leaves the input file's bytes and modification time unchanged

## 7. Interior normalization

- [x] 7.1 Implement page nesting: embed each source page as a form XObject on a fresh page of the required size under a computed matrix, without resampling or rasterizing
- [x] 7.2 Implement the three fit modes — `center` (default), `scale-to-bleed`, `stretch-margins` — and pass through unscaled any page already at the required size
- [x] 7.3 Bake `/Rotate` into the transform and emit no `/Rotate` on output pages
- [x] 7.4 Set output page boxes: `MediaBox`/`CropBox`/`BleedBox` to the full bleed page, `TrimBox`/`ArtBox` inset 0.125 in; test `[0 0 450 666]` and `[9 9 441 657]` for a 6 × 9 in product
- [x] 7.5 Test that `center` on a 6 × 9 in source gives a 9 pt offset at scale 1.0, and that `scale-to-bleed` scales by 6.25/6.0 and reports the 4.2 percent enlargement
- [x] 7.6 Implement the opt-in gutter shift moving odd pages toward increasing x and even pages toward decreasing x by the band's allowance, warning when content is pushed past the trim
- [x] 7.7 Implement blank-page padding to the product minimum and divisibility rule, appended at the end with no content stream, each reported individually; refuse above the maximum without writing output
- [x] 7.8 Implement structural sanitation: remove encryption, all annotations, `AcroForm` and fields, JavaScript, embedded files, multimedia and 3D, and force `/PageLayout /SinglePage`
- [x] 7.9 Fail with a password-required error, writing no output, when the input needs a real user password
- [x] 7.10 Implement opt-in spread splitting that halves each page vertically into left-then-right pages before geometry, never inferred from aspect ratio, with an informational finding on landscape pages
- [x] 7.11 Refuse to overwrite an existing output path without the explicit overwrite option, leaving the existing file untouched
- [x] 7.12 Re-run preflight on the output and fold the verdict into the run report, repeating every unfixed finding
- [x] 7.13 Test idempotence: normalizing normalized output reports no geometry or page-count change and yields identical page sizes and count

## 8. Cover preparation

- [x] 8.1 Implement cover geometry from product and final page count, returning canvas, spine rectangle, back and front panel rectangles, fold x-positions, hinge zones, safety rectangles, and the page count it was built for
- [x] 8.2 Refuse non-conformant page counts, naming the next valid count
- [x] 8.3 Read the final page count from a normalized interior file rather than accepting it separately, so spine and book cannot disagree
- [x] 8.4 Test that a 6 × 9 in perfect-bound product at 210 pages gives a 920 × 666 pt canvas, a 38.4 pt spine, and 432 × 648 pt panels
- [x] 8.5 Case wrap: composition rule derived and verified live against Lulu's production `cover-dimensions` endpoint (2 trim sizes x 5 page counts, 2026-09-03) — see `case_wrap_geometry` in cover.rs and design.md's Open Questions. Linen wrap (dust jacket, a different panel layout) remains untranscribed — `HARDCOVER_TEMPLATE_TABLE` stays empty pending a dust-jacket panel model
- [x] 8.6 Implement hardcover geometry lookup from that table or the API, labelling any local estimate unverified and refusing to write a final cover from it
- [x] 8.7 Implement cover template generation: a single page at the canvas size with trim, bleed, fold, hinge, and safety guides in one named optional content group, plus a legend giving product, page count, spine width, and canvas size
- [x] 8.8 Mark the template as a design aid and not submittable, in both the legend and the document metadata
- [x] 8.9 Implement fitting supplied single-page cover artwork: pass through unscaled within 0.5 pt, otherwise apply the caller's fit mode
- [x] 8.10 Implement the wrong-spine check that turns a canvas-width shortfall into a blocking finding naming the shortfall and the page count the required spine implies, without stretching the artwork
- [x] 8.11 Implement three-file assembly placing back, spine, and front artwork at their computed rectangles in left-to-right order
- [x] 8.12 Apply cover structural rules to every written cover: single page, no annotations, no encryption, no marks, `TrimBox` inset 0.125 in
- [x] 8.13 Implement cover safety-margin checks using 0.250 in, or 0.750 in for case wrap, and the narrow-spine text warning
- [x] 8.14 Implement the combined preview PDF: split the cover at its fold positions into trim-sized front/back pages, assemble front-cover + normalized interior pages + back-cover in order, and mark it as a non-submittable proof in its legend and metadata

## 9. External tool pipeline

- [x] 9.1 Implement capability detection resolving each binary on `PATH` or from a configured path, probing its version under a timeout, treating unresponsive or below-minimum binaries as unavailable with a stated reason
- [x] 9.2 Record detected tools with name, resolved path, and version in the report; honour explicit configured paths over `PATH`
- [x] 9.3 Implement the fixed stage pipeline — repair, spread split, geometry, gutter, padding, sanitation, flatten, colour convert — with a stage log carrying order and durations
- [x] 9.4 Implement the qpdf repair stage rebuilding the xref, removing encryption, and optionally linearizing; attempt it automatically when native parsing fails, then re-parse and continue
- [x] 9.5 Fail with a blocking parse finding naming qpdf as the remedy when repair is needed and unavailable, and surface qpdf's own diagnostics when repair itself fails
- [x] 9.6 Implement the Ghostscript flatten and colour-convert stage, off by default, preserving geometry, embedding fonts, and never downsampling below 300 ppi
- [x] 9.7 Require an explicitly supplied ICC profile for CMYK conversion, failing rather than defaulting, and record the profile path and digest
- [x] 9.8 Record the full Ghostscript argument list, exit status, and stderr in the report
- [x] 9.9 Assert after the Ghostscript stage that every page's `MediaBox` and `TrimBox` still equal the values normalization set and the page count is unchanged, failing the run otherwise
- [x] 9.10 Ensure a non-zero Ghostscript exit never replaces the pre-stage file with a partial output, and that the pre-stage file is either kept at a stated path or removed cleanly
- [x] 9.11 Fail with an explicit missing-binary error naming the stage and installation hint when a stage is requested and its binary is absent
- [x] 9.12 Verify by test that preflight and normalization both complete with neither binary installed, listing skipped stages and what each would have fixed
- [x] 9.13 Implement native image-only ICC conversion via `lcms2`, leaving vector colour untouched and saying so explicitly in the report, and warning on rather than failing over undecodable image encodings

## 10. Lulu Print API verification

- [x] 10.1 Implement the OAuth client-credentials token flow behind the `lulu-api` feature, reading the client key and secret from environment or config file and never from argv
- [x] 10.2 Implement sandbox and production host selection, naming the environment in the report
- [x] 10.3 Ensure credentials and tokens never appear in reports, logs, or error messages, and test that an auth failure message contains neither
- [x] 10.4 Verify that a default-feature build contains no HTTP client, attempts no connection, and reads no credential under any invocation
- [x] 10.5 Implement the `cover-dimensions` call and the comparison against locally computed geometry, treating a disagreement beyond 1 pt as blocking and naming both values
- [x] 10.6 Use API-returned dimensions as the authoritative source for hardcover products absent from the template table
- [x] 10.7 Implement `validate-interior` and `validate-cover` submission and status polling across `NULL`, `VALIDATING`, `VALIDATED`, `NORMALIZING`, `NORMALIZED`, and `ERROR`
- [x] 10.8 Treat `VALIDATED` and `NORMALIZED` as success, recording Lulu's normalized page count; turn each entry of Lulu's `errors` field into a blocking finding reproduced verbatim and attributed to Lulu
- [x] 10.9 Skip file validation with a clear explanation when no publicly reachable URL is supplied, while still performing the dimension check
- [x] 10.10 Bound polling by timeout, reporting the last status and the validation job identifier instead of a verdict on timeout
- [x] 10.11 Link Lulu errors to their corresponding local finding codes for the four overlapping cases — mismatched page sizes, unembedded fonts, under two pages, and page size not matching the SKU
- [x] 10.12 Apply request timeouts and bounded exponential backoff retrying only idempotent requests on transport errors and 5xx, never on 4xx, and leave prepared files on disk when verification cannot complete

## 11. CLI

- [x] 11.1 Implement the `check`, `interior`, `cover`, `book`, `products`, and `spine` subcommands with `--help` stating every option's default and unit
- [x] 11.2 Implement product selection by `--sku` in either form, or by component flags, listing candidates and exiting non-zero without writing when ambiguous
- [x] 11.3 Implement `products` search output listing SKU, book type, trim size, size with bleed, binding, paper, and page-count range
- [x] 11.4 Implement `spine` to print spine width and cover canvas from a product and page count with no PDF input
- [x] 11.5 Implement `book` so the cover's page count comes from the normalized interior rather than the user
- [x] 11.6 Implement configuration precedence — flags, environment, project config, user config, defaults — and a printable effective configuration naming each value's source
- [x] 11.7 Implement the exit codes: 0 clean, 1 blocking findings remain, 2 invalid usage or unresolvable product, 3 I/O or parse failure, 4 missing external tool or credential; add the strict option promoting warnings to exit 1
- [x] 11.8 Implement default output paths with `-interior` and `-cover` suffixes in a chosen directory, printing the path, and refusing to overwrite without `--force`
- [x] 11.9 Implement dry-run mode printing the full report and intended output paths while creating and modifying nothing
- [x] 11.10 Implement report presentation: human text to stdout by default, JSON to a path or stdout with nothing else mixed in, progress and diagnostics to stderr, colour disabled when stdout is not a terminal or a no-colour setting is present
- [x] 11.11 Support a fixed document identifier and creation date so that two identical runs produce byte-identical output PDFs, and test it

## 12. Test corpus and verification

- [x] 12.1 Write the committed fixture generation script producing: no bleed, correct bleed, mixed page sizes, rotated pages, empty-password encryption, unembedded font, low-resolution image, nested form XObject image, live transparency, optional content groups, and two-up spreads
- [x] 12.2 Add `insta` snapshot tests over the JSON report for each fixture, excluding timestamps, durations, and tool versions
- [x] 12.3 Add property tests asserting normalized output always has uniform pages at the required size, a conformant page count, and correct page boxes, over generated inputs varying in size, rotation, and count
- [x] 12.4 Add the end-to-end `book` test taking a fixture from raw PDF to interior plus cover, asserting the cover canvas matches the interior's final page count
- [x] 12.5 Add tests covering each exit code path
- [x] 12.6 Document in the README the four Lulu rejection reasons the local checks are designed to predict, and add a note that any observed miss should become a new fixture

## 13. Documentation and open questions

- [x] 13.1 Write user documentation covering a first run, product selection, the fit modes and their trade-offs, and when to enable the optional stages
- [x] 13.2 Document the catalog refresh procedure and how to tell from a report whether the catalog is stale
- [x] 13.3 Resolve and record how many hardcover templates were transcribed before trusting the inferred composition rule, in the data file itself
- [x] 13.4 Decide and document the image total-area-coverage sampling rate and how the finding reports its uncertainty
- [x] 13.5 Decide whether the 0.200 in gutter advisory floor stays warning-only, and record the reasoning
- [x] 13.6 Decide whether `book` emits a combined manifest describing both files, their digests, the product, and the final page count; specify it before implementing
