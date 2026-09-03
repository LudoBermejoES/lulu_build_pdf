# lulu-prep

Prepares an arbitrary PDF for print submission to [Lulu](https://www.lulu.com):
normalizes an interior to Lulu's bleed, size, and page-count rules, and builds
a matching cover (spine width included) from a `pod_package_id` and a page
count. Works entirely offline against an embedded copy of Lulu's product
catalog; Lulu API access is opt-in and used only for read-only cross-checks.

```
lulu-prep book manuscript.pdf --sku 0600X0900.BW.STD.PB.060UW444.MXX
```

## Install / build

```
cargo build --release -p lulu-prep-cli
./target/release/lulu-prep --help
```

The binary is `lulu-prep`; the library it's built on (`lulu-prep`, the
`lulu-prep` crate) can also be used directly from other Rust code.

## Optional external tools

Two external tools are used, both strictly as subprocesses (never linked in,
never required to build or run the core tool):

- **qpdf** — repairs a malformed input PDF before normalizing it, when the
  native parser can't read it as-is. Detected on `PATH` (or via `--qpdf-path`);
  if it isn't installed, repair is simply skipped and the original parse error
  is reported.
- **Ghostscript** — an opt-in flattening stage (`--flatten`) for live
  transparency, spot colours, or other content this tool can detect but not
  itself rewrite. Detected on `PATH` (or via `--gs-path`); if you never pass
  `--flatten`, Ghostscript is never invoked and its absence is not an error.
  **Ghostscript is AGPL-licensed** — this project does not vendor or link it,
  and invoking it as a separate process does not place this project under the
  AGPL, but you are responsible for your own Ghostscript license terms if you
  install and use it (in particular, note the AGPL's implications if you
  offer this tool's flattening feature as a network service).

Neither tool is required for `check`, `interior` (without `--flatten`),
`cover`, `products`, or `spine`.

## First run

Pick a product from Lulu's catalog, either by its `pod_package_id`:

```
lulu-prep check manuscript.pdf --sku 0600X0900.BW.STD.PB.060UW444.MXX
```

or by describing it — trim size, binding, ink, quality, paper, lamination —
and letting the tool resolve it:

```
lulu-prep products --trim 6x9 --binding perfect --ink bw
```

`check` never writes a file; it only reports what would need to change.
`interior`, `cover`, and `book` do the actual work:

```
lulu-prep book manuscript.pdf --sku 0600X0900.BW.STD.PB.060UW444.MXX --output-dir out/
```

`book` normalizes the interior first, then builds the cover from the
*normalized* interior's final page count (after any blank-page padding) —
never from the page count you started with, so the pair can never drift out
of sync. Its `--json` output is a single document, `{"interior": {...},
"cover": {...}}`, rather than the interior's and cover's reports printed one
after another — the whole point is that it's one parseable thing, and
`--report-out` writes it once rather than being overwritten by the second of
two separate writes.

## Product selection

A product is either a `pod_package_id` (`--sku`, dotted or legacy 27-character
form) or a set of component flags: `--trim WxH` (inches), `--binding`, `--ink`
(`bw`/`fc`), `--quality`, `--paper`, `--lamination`. Component flags are
substring/exact matches against the catalog; if more than one product matches,
the tool lists every candidate and exits without acting rather than guessing —
narrow the selection instead of picking blind. `lulu-prep products` runs the
same resolution as a search, listing every match's SKU, book type, trim size,
size with bleed, binding, paper, and page-count range.

## Fit modes

`--fit-mode` controls how a page's original content is placed onto the
required (trim + bleed) canvas:

- **`center`** (default) — content stays at its original scale, centered.
  Safest choice: never rescales or crops content, but leaves an unprinted
  border if the source has no bleed of its own.
- **`scale-to-bleed`** — scales content up uniformly until it fully covers
  the bleed area, cropping equally on all sides. Use when the source has no
  bleed and a small uniform crop is acceptable; never distorts, but you lose
  a fixed margin all around.
- **`stretch-margins`** — documented as filling the surrounding bleed area
  with a flat colour, as a stand-in for Lulu's "extend the outermost edge
  pixels or fill colour" allowance. **Not implemented**: selecting it fails
  the run explicitly rather than silently behaving like `center`. True edge
  extension would require decoding and resampling raster content, which this
  tool does not do; a flat-colour fill remains open for a future change.

## Optional stages

- `--gutter` — applies the inner-margin (gutter) shift for the page count's
  band. Off by default: a source already laid out with its own gutter would
  otherwise be double-shifted.
- `--split-spreads` — splits each page down its vertical centre into two
  pages (left then right) before geometry, for a source imposed as two-up
  spreads. Off by default and never inferred from aspect ratio — a
  legitimately landscape product looks identical to an unsplit spread by
  geometry alone, so a landscape source without this flag gets an
  informational finding instead of an automatic split.
- `--flatten` — runs the Ghostscript stage (see above) after normalizing.
- ICC-based image colour conversion is available as a library feature
  (`icc`, off by default) for callers converting images to a specific CMYK
  destination profile; not currently exposed as a CLI flag.
- Lulu API verification (`lulu-api` feature) — read-only `cover-dimensions`,
  `validate-interior`, and `validate-cover` cross-checks against Lulu's own
  API. Opt-in at build time and requires API credentials; never required for
  normal use.

## Catalog refresh

The embedded catalog (`crates/lulu-prep/data/pod-packages.csv`) is a snapshot
of Lulu's published product spec sheet, not a live lookup — regenerate it with:

```
python3 crates/lulu-prep/data/regenerate.py
```

which re-downloads Lulu's spec sheet and rewrites the CSV, including a header
comment recording the source URL and fetch date. Every report includes
`catalog_fetch_date`; compare it against Lulu's own current spec sheet date to
tell whether a report was produced against a stale catalog. A catalog refresh
is a plain diff of a committed CSV file, so it shows up as a reviewable change
rather than a silent, unauditable update.

## What this predicts about Lulu's own rejections

Lulu's file validation, run after upload, rejects a file for several reasons.
Four of them overlap exactly with checks this tool runs locally, before you
ever upload anything:

- **Mismatched page sizes** — every interior page must be the same size
  (`geometry.mixed-page-sizes`).
- **Unembedded fonts** — every font, including the standard 14, must be
  embedded (`fonts.not-embedded`).
- **Too few pages** — an interior must meet the product's page-count minimum
  (`page-count.below-minimum`).
- **Page size not matching the target product** — the page size (trim plus
  0.125in bleed per side) must match the SKU you're submitting against
  (`geometry.page-size-mismatch`).

These are the cases this tool is specifically designed to predict correctly,
and its own report links a local finding to the Lulu error it corresponds to
where the two overlap. If you ever see Lulu reject a file for one of these
four reasons on a file this tool reported as print-ready, that is a bug in
this tool — please turn the offending file into a new test fixture
(`crates/lulu-prep/examples/generate_fixtures.rs`) rather than working around
it by hand next time.

`check` and `interior` are guaranteed to agree about the same input: if
`check` reports a blocking finding, `interior` on that same file will too
(and exit 1), never silently produce a `print-ready` file that still carries
the problem. In particular, font embedding and colour/ink checks now see
through the form XObjects normalization nests page content into, so an
unembedded font or a colour problem inside already-normalized content is
still caught rather than becoming invisible to the checks that exist to
catch it.
