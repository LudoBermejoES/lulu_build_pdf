# pdf-preflight Specification

## Purpose
TBD - created by archiving change prepare-pdf-for-lulu. Update Purpose after archive.
## Requirements
### Requirement: Preflight is read-only

Preflight SHALL inspect a PDF and report findings without modifying the input file or writing any output PDF. It SHALL accept a target `pod_package_id` and an optional role (interior or cover), and SHALL produce a findings report even when the file cannot be fully parsed.

#### Scenario: Input is left untouched

- **WHEN** preflight runs against a PDF
- **THEN** the input file's bytes and modification time are unchanged after the run

#### Scenario: Damaged file still yields a report

- **WHEN** preflight runs against a PDF whose cross-reference table is broken
- **THEN** the report contains a blocking finding describing the parse failure and names structural repair as the remedy, rather than the process aborting without a report

### Requirement: Findings model

Every finding SHALL carry a stable machine-readable code, a severity, a human-readable message, the affected page numbers where applicable, the observed value, the expected value, and whether the tool can fix it automatically.

Severities SHALL be exactly three: `blocking` for conditions Lulu rejects, `warning` for conditions Lulu accepts but that degrade print quality or rely on Lulu's own normalizer, and `info` for observations that need no action.

#### Scenario: Finding carries observed and expected values

- **WHEN** a page measures 6.000 × 9.000 in against a product requiring 6.250 × 9.250 in
- **THEN** the finding names the page, the observed size, the required size, its severity, and that the tool can fix it

#### Scenario: Codes are stable

- **WHEN** the same defect is detected in two different runs or two different files
- **THEN** the finding code is identical, so reports can be diffed and findings can be suppressed by code

### Requirement: Page geometry checks

Preflight SHALL verify that every page's effective print size equals the product's required size with bleed, and that all pages share one size.

The effective size SHALL be computed from the page's `TrimBox`, `BleedBox`, `CropBox`, and `MediaBox` with PDF's defined fallback order, and SHALL account for the page's `/Rotate` entry. Lulu's own validation rejects interiors whose pages differ in size, so a mixed-size interior SHALL be blocking.

#### Scenario: Uniform pages at the wrong size

- **WHEN** every page of a 6 × 9 in product's interior measures exactly 6.000 × 9.000 in
- **THEN** preflight reports a blocking finding that the file lacks bleed, states that 0.125 in per side is required, and marks it fixable

#### Scenario: Mixed page sizes

- **WHEN** an interior contains pages of two different sizes
- **THEN** preflight reports a blocking finding listing each distinct size and the pages that use it

#### Scenario: Rotated page

- **WHEN** a page carries `/Rotate 90` such that its visible size is 9.25 × 6.25 in for a 6 × 9 in product
- **THEN** preflight reports the visible size, not the unrotated box size, and reports a blocking orientation finding

#### Scenario: Page size within tolerance

- **WHEN** a page measures 450.02 × 665.98 pt against a required 450 × 666 pt
- **THEN** preflight accepts the size, applying a tolerance of 0.5 pt, and emits no size finding

### Requirement: Font embedding check

Preflight SHALL report every font referenced by the document that is not fully embedded, since Lulu's file validation rejects interiors with unembedded fonts.

A font SHALL be treated as embedded when its descriptor carries `FontFile`, `FontFile2`, or `FontFile3`. Composite fonts SHALL be checked through their descendant fonts. The standard 14 base fonts SHALL be reported as not embedded, because Lulu requires embedding rather than relying on viewer substitution.

#### Scenario: Unembedded font is blocking

- **WHEN** a page references Helvetica with no embedded font file
- **THEN** preflight reports a blocking finding naming the font, the pages that use it, and that the tool cannot fix it without an external tool

#### Scenario: Subset-embedded font passes

- **WHEN** every referenced font has an embedded font file, including subset-tagged names such as `ABCDEF+Minion-Regular`
- **THEN** preflight emits no font embedding finding

#### Scenario: Composite font descendants are checked

- **WHEN** a Type0 font's descendant CIDFont has no embedded font file
- **THEN** preflight reports the Type0 font as not embedded

### Requirement: Image resolution check

Preflight SHALL compute the effective resolution of each raster image as placed on the page, by combining the image XObject's pixel dimensions with the current transformation matrix at its draw site, and SHALL report images outside Lulu's stated range.

Effective resolution below 300 ppi SHALL be a warning naming the image and page. Effective resolution above 600 ppi SHALL be a warning, since Lulu states 600 ppi as the maximum useful resolution. The check SHALL report the lowest effective resolution found, so a report can be summarised in one line.

#### Scenario: Low-resolution image

- **WHEN** a 600 × 400 pixel image is placed across a 6 in wide area, giving 100 ppi
- **THEN** preflight reports a warning naming the page, the image, its 100 ppi effective resolution, and the 300 ppi target

#### Scenario: Excessive resolution

- **WHEN** an image is placed at an effective 1200 ppi
- **THEN** preflight reports a warning that the resolution exceeds Lulu's 600 ppi maximum and adds file size without improving print quality

#### Scenario: Image drawn under nested transforms

- **WHEN** an image is drawn inside a form XObject that is itself scaled by the page content stream
- **THEN** the effective resolution accounts for the concatenated transformation, not just the innermost one

#### Scenario: Vector-only page

- **WHEN** a page contains no raster images
- **THEN** preflight emits no image resolution finding for that page

### Requirement: Colour and ink checks

Preflight SHALL report the colour spaces in use and the conditions Lulu calls out as print risks.

Colour spaces other than DeviceGray, DeviceRGB with an sRGB or calibrated profile, and DeviceCMYK SHALL be reported as warnings. For CMYK content, total area coverage above 270 percent SHALL be a warning, since Lulu states 270 percent as the ceiling. Tints below 20 percent SHALL be reported as a warning, since Lulu advises against them. Live transparency and optional content groups SHALL be reported as warnings, naming flattening as the remedy.

#### Scenario: Total ink coverage too high

- **WHEN** a CMYK fill on a page sums to 300 percent coverage
- **THEN** preflight reports a warning naming the page, the observed 300 percent, and Lulu's 270 percent ceiling

#### Scenario: Spot colour is reported

- **WHEN** a page uses a Separation or DeviceN colour space
- **THEN** preflight reports a warning naming the colour space and the pages that use it

#### Scenario: Live transparency is reported

- **WHEN** a page's resources declare a soft mask or a non-Normal blend mode
- **THEN** preflight reports a warning that transparency is unflattened and names flattening as the remedy

#### Scenario: Optional content is reported

- **WHEN** the document declares an `/OCProperties` dictionary
- **THEN** preflight reports a warning that layers are present and must be flattened

### Requirement: Structural checks

Preflight SHALL report document features Lulu prohibits or that have no meaning in print: encryption or password protection, annotations, form fields, embedded JavaScript, embedded files, multimedia, and a page layout requesting two-page spreads.

#### Scenario: Encrypted file is blocking

- **WHEN** the PDF carries an encryption dictionary, even with an empty user password
- **THEN** preflight reports a blocking finding, since Lulu prohibits security and password protection, and marks it fixable

#### Scenario: Annotations are reported

- **WHEN** pages carry link or text annotations
- **THEN** preflight reports a warning listing the annotation subtypes and affected pages, and marks them fixable by removal

#### Scenario: Spread layout is reported

- **WHEN** the catalog declares `/PageLayout /TwoPageLeft` or similar
- **THEN** preflight reports a warning that Lulu requires a single-page layout, and marks it fixable

### Requirement: Page count check against the product

Preflight SHALL compare the document's page count against the product's minimum, maximum, and divisibility rule, and SHALL state the padding the tool would apply.

#### Scenario: Count below the minimum

- **WHEN** an interior of 18 pages targets a product with a 32-page minimum
- **THEN** preflight reports a blocking finding naming the minimum and stating that 14 blank pages would be appended

#### Scenario: Count not divisible

- **WHEN** an interior of 205 pages targets a perfect-bound product
- **THEN** preflight reports a fixable finding stating that 3 blank pages would be appended to reach 208

#### Scenario: Count above the maximum

- **WHEN** an interior of 812 pages targets a product with an 800-page maximum
- **THEN** preflight reports a blocking, unfixable finding naming the maximum and stating that the content must be split or the product changed

### Requirement: Report output

Preflight SHALL emit its report as human-readable text and as JSON, with the JSON carrying a schema version, the input file digest, the resolved product, the catalog fetch date, the tool version, and the full findings list.

The human-readable report SHALL group findings by severity, lead with a one-line verdict, and be readable without colour.

#### Scenario: JSON report is machine-consumable

- **WHEN** the caller requests JSON output
- **THEN** the output parses as JSON, carries a schema version, and lists every finding with its code, severity, pages, observed value, and expected value

#### Scenario: Verdict line summarises the run

- **WHEN** a file has two blocking findings and five warnings
- **THEN** the human-readable report's first line states that the file is not print-ready and gives both counts

#### Scenario: Clean file reports readiness

- **WHEN** a file has no blocking findings
- **THEN** the verdict line states the file is print-ready for the named product and page count, listing any remaining warnings below

