# interior-normalization Specification

## Purpose
TBD - created by archiving change prepare-pdf-for-lulu. Update Purpose after archive.
## Requirements
### Requirement: Interior normalization produces a conformant file

Given an input PDF and a target `pod_package_id`, normalization SHALL write a new interior PDF in which every page is the product's required size with bleed, the page count satisfies the product's minimum and divisibility rule, and none of the structures Lulu prohibits remain.

Normalization SHALL never modify the input file in place, and SHALL refuse to overwrite an existing output path unless the caller explicitly allows it.

#### Scenario: A plain trim-size PDF is made print-ready

- **WHEN** a 200-page PDF whose pages measure exactly 6.000 × 9.000 in is normalized for a 6 × 9 in perfect-bound product
- **THEN** the output has 200 pages, each 6.250 × 9.250 in, with the original page content positioned according to the configured fit mode

#### Scenario: Re-running normalization is stable

- **WHEN** normalization runs on its own output with the same product and options
- **THEN** the second run reports no geometry or page-count changes, and the resulting page sizes and page count are identical

#### Scenario: Existing output is protected

- **WHEN** the output path already exists and the caller has not passed the overwrite option
- **THEN** normalization fails with an error naming the path, and the existing file is untouched

### Requirement: Page geometry transformation

Normalization SHALL place each source page's content onto a new page of the required size with bleed, without resampling raster images or rasterizing vector content, by embedding the source page as a form XObject under an affine transform.

Three fit modes SHALL be supported. `center` SHALL centre the source content at its original scale, which yields an unprinted 0.125 in border where the source has no bleed. `scale-to-bleed` SHALL scale the source uniformly so it covers the full bleed area, cropping equally on all sides. `stretch-margins` SHALL keep the source at original scale and extend the outermost edge pixels or fill colour into the bleed area. The default SHALL be `center`, because it never alters the position of content relative to the trim edge.

The output SHALL set `MediaBox`, `CropBox`, and `BleedBox` to the full page including bleed, and `TrimBox` and `ArtBox` to the trim rectangle inset 0.125 in on each side, so downstream tools know where the trim falls.

#### Scenario: Centre fit preserves scale and trim position

- **WHEN** a 6.000 × 9.000 in page is normalized with fit mode `center` for a 6 × 9 in product
- **THEN** the content is offset by 9 pt in x and 9 pt in y, its scale is exactly 1.0, and the output `TrimBox` coincides with the original page rectangle

#### Scenario: Scale-to-bleed covers the bleed area

- **WHEN** a 6.000 × 9.000 in page is normalized with fit mode `scale-to-bleed`
- **THEN** the content is scaled uniformly by 6.250 / 6.000 and centred, so no unprinted border remains, and the report notes the 4.2 percent enlargement

#### Scenario: A source that already has bleed is not rescaled

- **WHEN** a page already measures 6.250 × 9.250 in for a 6 × 9 in product
- **THEN** the page is passed through with scale 1.0 and zero offset, and only its page boxes are corrected

#### Scenario: Rotation is baked in

- **WHEN** a source page carries `/Rotate 90`
- **THEN** the rotation is applied within the transform, the output page carries no `/Rotate` entry, and the visible content orientation is unchanged

#### Scenario: Boxes are set for the trim

- **WHEN** any page is normalized for a 6 × 9 in product
- **THEN** the output page's `MediaBox`, `CropBox`, and `BleedBox` are `[0 0 450 666]` and its `TrimBox` and `ArtBox` are `[9 9 441 657]`

### Requirement: Gutter shift

When enabled, normalization SHALL shift page content away from the bound edge by the gutter allowance for the final page count, moving odd pages toward the right and even pages toward the left, so that inner-edge content clears the binding.

The gutter shift SHALL be off by default, because a source laid out with its own gutter would be double-shifted. When enabled, normalization SHALL report the applied offset and warn if any content is thereby pushed outside the trim area.

#### Scenario: Odd and even pages shift in opposite directions

- **WHEN** the gutter shift is enabled for a 210-page interior, whose gutter allowance is 0.500 in
- **THEN** odd-numbered pages are shifted 36 pt toward increasing x and even-numbered pages 36 pt toward decreasing x

#### Scenario: Gutter shift pushes content off the trim

- **WHEN** the gutter shift would move existing content outside the trim rectangle
- **THEN** the operation still completes and the report carries a warning naming the affected pages

#### Scenario: Gutter shift is off by default

- **WHEN** normalization runs without the gutter option
- **THEN** no page is shifted and the report states that gutter compensation was not applied

### Requirement: Page count padding

Normalization SHALL append blank pages of the required size with bleed until the page count reaches the product's minimum and satisfies its divisibility rule, and SHALL refuse the job when the count exceeds the product's maximum.

Blank pages SHALL be appended at the end, matching Lulu's own behaviour of adding white pages to the back of the book. Blank pages SHALL carry no content stream and SHALL be reported individually in the run report.

#### Scenario: Padding to the divisibility rule

- **WHEN** a 205-page interior is normalized for a perfect-bound product
- **THEN** the output has 208 pages, pages 206 through 208 are blank, and the report names them

#### Scenario: Padding to the product minimum

- **WHEN** an 18-page interior is normalized for a product with a 32-page minimum
- **THEN** the output has 32 pages and the report states that 14 blank pages were appended to reach the minimum

#### Scenario: Over the maximum is refused

- **WHEN** an 812-page interior is normalized for a product with an 800-page maximum
- **THEN** normalization fails with an error naming the observed count and the maximum, and writes no output file

#### Scenario: Saddle stitch padding

- **WHEN** a 30-page interior is normalized for a saddle-stitch product
- **THEN** the output has 32 pages

### Requirement: Structural sanitation

Normalization SHALL remove from the output every structure Lulu prohibits or that carries no print meaning: encryption, all annotations, all form fields and the `AcroForm` dictionary, document-level and annotation-level JavaScript, embedded files, multimedia and 3D artwork, and any `PageLayout` requesting spreads.

Where a source PDF is encrypted with an empty user password, normalization SHALL decrypt it. Where it is encrypted with a real user password, normalization SHALL fail with an error asking for the password rather than producing a partially readable file.

#### Scenario: Empty-password encryption is removed

- **WHEN** an input PDF is encrypted with an empty user password and an owner password
- **THEN** the output is unencrypted and the report records that encryption was removed

#### Scenario: Password-protected input is refused

- **WHEN** an input PDF requires a user password that the caller has not supplied
- **THEN** normalization fails with an error stating that the password is required, and writes no output

#### Scenario: Annotations and scripts are stripped

- **WHEN** an input PDF carries link annotations, a form field, and document-level JavaScript
- **THEN** none of them appear in the output, and the report lists each category removed with its count

#### Scenario: Single-page layout is enforced

- **WHEN** an input declares a two-page spread layout
- **THEN** the output declares `/PageLayout /SinglePage`

### Requirement: Spread splitting

When the caller declares that the source is imposed as two-up spreads, normalization SHALL split each spread page down its vertical centre into two single pages, ordered left page first, before applying page geometry.

Spread splitting SHALL be opt-in and SHALL NOT be inferred from aspect ratio alone, because a legitimately landscape product is indistinguishable from a spread by geometry. The tool MAY report a landscape-page observation suggesting the option.

#### Scenario: Spreads are split in reading order

- **WHEN** a 50-page file of 12 × 9 in spreads is normalized with spread splitting enabled for a 6 × 9 in product
- **THEN** the output has 100 pages, and the left half of source page 1 becomes output page 1

#### Scenario: Splitting is never automatic

- **WHEN** a file of landscape pages is normalized without the spread option
- **THEN** no page is split, and the report carries an informational finding that the pages are landscape and that spread splitting exists

### Requirement: Run report

Normalization SHALL emit a report describing every change it made, in the same human and JSON forms as preflight, and SHALL re-run the preflight checks against its own output so the report states the resulting file's conformance.

#### Scenario: Report enumerates the changes

- **WHEN** normalization rescales pages, appends blank pages, and strips annotations
- **THEN** the report lists each of those actions with the affected page numbers or counts

#### Scenario: Output is preflighted

- **WHEN** normalization completes
- **THEN** the report includes a preflight verdict for the output file, and any finding that normalization could not fix is repeated there with its severity

