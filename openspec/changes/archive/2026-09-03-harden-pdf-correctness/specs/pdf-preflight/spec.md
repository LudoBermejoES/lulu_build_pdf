# pdf-preflight (delta)

## ADDED Requirements

### Requirement: PDF object access follows the format's legal shapes

Every read of a PDF object SHALL resolve indirect references, and SHALL resolve inheritable page attributes through the page's `/Parent` chain. A read that cannot be resolved SHALL produce a finding naming the entry, and SHALL NOT be silently substituted with a default value such as an empty dictionary or a zero.

This applies at minimum to a page's `/Resources`, its box entries and the elements of those arrays, its `/Rotate`, a form XObject's own `/Resources`, and the document catalog's `/Names` tree.

#### Scenario: Indirect resource dictionary is resolved

- **WHEN** a page carries `/Resources 4 0 R`, where object 4 holds the font used by the page's content stream
- **THEN** the font is found and checked exactly as if `/Resources` had been written as a direct dictionary

#### Scenario: Inherited resource dictionary is resolved

- **WHEN** a page has no `/Resources` of its own and inherits one from its `Pages` node
- **THEN** the inherited resources are found and checked

#### Scenario: A page's own resources take precedence over inherited ones

- **WHEN** both a page and its `Pages` ancestor define `/Font` entries under the same name
- **THEN** the page's own entry is the one used

#### Scenario: Unreadable rotation is reported, not assumed to be zero

- **WHEN** a page's `/Rotate` is an indirect reference, a real number such as `90.0`, or a value that is not a multiple of 90
- **THEN** the rotation is resolved where it legally can be, and where it cannot be resolved or is not a multiple of 90, preflight reports a finding rather than treating the page as unrotated

#### Scenario: Unreadable page geometry is blocking rather than skipped

- **WHEN** a page's box entry is an indirect reference that cannot be resolved, or the page has no resolvable box at all
- **THEN** preflight reports a blocking finding naming that page, and the page is not silently omitted from the geometry and mixed-size checks

### Requirement: Content that names unresolvable resources is blocking

A page or form XObject whose content stream names a resource — a font, XObject, `ExtGState`, colour space, pattern, or shading — that cannot be resolved in its effective resource dictionary SHALL be reported as a blocking finding identifying the page and the missing resource name.

An empty resource dictionary SHALL NOT by itself be a finding, because a page that draws nothing legitimately has no resources.

#### Scenario: Content references a font that cannot be resolved

- **WHEN** a page's content stream contains `/F1 Tf` and no `/F1` can be resolved in the page's effective resources
- **THEN** preflight reports a blocking finding naming the page and `/F1`, because that page will print without its text

#### Scenario: A genuinely blank page is not flagged

- **WHEN** a page has an empty content stream and no resources
- **THEN** preflight emits no unresolvable-resource finding

### Requirement: Traversal of untrusted structure is bounded

Every walk over structure taken from the input file SHALL be bounded, and exhausting a bound SHALL produce a blocking finding rather than a hang, a stack overflow, or a silently truncated result.

Bounds SHALL cover at minimum: cycles in the `/Parent` inheritance chain, the depth of a chain of indirect references followed while copying objects, and the total number of content-stream operations examined while walking nested form XObjects.

#### Scenario: Parent cycle terminates

- **WHEN** a page's `/Parent` chain contains a cycle and the attribute being resolved appears nowhere in it
- **THEN** the walk terminates and preflight reports the file as unreadable, rather than looping indefinitely

#### Scenario: Operation budget is reported, not silently applied

- **WHEN** a file's nested form XObjects would require more content-stream operations to examine than the budget allows
- **THEN** preflight reports a blocking finding stating that the file's structure exceeded the traversal budget and that its checks are therefore incomplete

## MODIFIED Requirements

### Requirement: Page geometry checks

Preflight SHALL verify that every page's effective print size equals the product's required size with bleed, and that all pages share one size.

The effective size SHALL be computed from the page's `TrimBox`, `BleedBox`, `CropBox`, and `MediaBox` with PDF's defined fallback order, resolving indirect references for both the box entry and the numbers within it, and SHALL account for the page's `/Rotate` entry including when that entry is an indirect reference or a real number. Lulu's own validation rejects interiors whose pages differ in size, so a mixed-size interior SHALL be blocking.

A page whose geometry cannot be resolved SHALL be reported as a blocking finding naming that page, and SHALL NOT be excluded from the size and mixed-size checks without a finding.

#### Scenario: Uniform pages at the wrong size

- **WHEN** every page of a 6 × 9 in product's interior measures exactly 6.000 × 9.000 in
- **THEN** preflight reports a blocking finding that the file lacks bleed, states that 0.125 in per side is required, and marks it fixable

#### Scenario: Mixed page sizes

- **WHEN** an interior contains pages of two different sizes
- **THEN** preflight reports a blocking finding listing each distinct size and the pages that use it

#### Scenario: Rotated page

- **WHEN** a page carries `/Rotate 90` such that its visible size is 9.25 × 6.25 in for a 6 × 9 in product
- **THEN** preflight reports the visible size, not the unrotated box size, and reports a blocking orientation finding

#### Scenario: Rotation written as a real number

- **WHEN** a page carries `/Rotate 90.0` rather than `/Rotate 90`
- **THEN** preflight reports the same visible size and the same finding as it does for the integer form

#### Scenario: Page size within tolerance

- **WHEN** a page measures 450.02 × 665.98 pt against a required 450 × 666 pt
- **THEN** preflight accepts the size, applying a tolerance of 0.5 pt, and emits no size finding

#### Scenario: Page with an unresolvable box

- **WHEN** a page's `/MediaBox` is an indirect reference that cannot be resolved
- **THEN** preflight reports a blocking finding naming that page rather than omitting it from the geometry checks

### Requirement: Font embedding check

Preflight SHALL report every font referenced by the document that is not fully embedded, since Lulu's file validation rejects interiors with unembedded fonts.

A font SHALL be treated as embedded when its descriptor carries `FontFile`, `FontFile2`, or `FontFile3`. Composite fonts SHALL be checked through their descendant fonts. The standard 14 base fonts SHALL be reported as not embedded, because Lulu requires embedding rather than relying on viewer substitution.

Fonts SHALL be discovered through the page's effective resources — direct, indirect, or inherited — and through the resources of every form XObject the page draws, to whatever depth the traversal budget allows. A font referenced only from inside a form XObject SHALL be reported exactly as one referenced directly by the page.

#### Scenario: Unembedded font is blocking

- **WHEN** a page references Helvetica with no embedded font file
- **THEN** preflight reports a blocking finding naming the font, the pages that use it, and that the tool cannot fix it without an external tool

#### Scenario: Subset-embedded font passes

- **WHEN** every referenced font has an embedded font file, including subset-tagged names such as `ABCDEF+Minion-Regular`
- **THEN** preflight emits no font embedding finding

#### Scenario: Composite font descendants are checked

- **WHEN** a Type0 font's descendant CIDFont has no embedded font file
- **THEN** preflight reports the Type0 font as not embedded

#### Scenario: Font inside a form XObject is found

- **WHEN** a page draws a form XObject whose own resources reference an unembedded font, and the page's own resources reference none
- **THEN** preflight reports the unembedded font, naming the page that draws the form

### Requirement: Colour and ink checks

Preflight SHALL report total area coverage above Lulu's limit, tints below the reproducible minimum, colour spaces Lulu does not support, live transparency, and optional content (layers).

These SHALL be evaluated over the page's own content stream and over the content of every form XObject the page draws, to whatever depth the traversal budget allows, together with the corresponding effective resource dictionaries. Colour set inside a form XObject SHALL NOT be invisible to these checks.

Each distinct condition SHALL carry its own stable finding code, so that a consumer filtering or suppressing by code cannot conflate two unrelated conditions.

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

#### Scenario: Colour inside a form XObject is inspected

- **WHEN** a page draws a form XObject whose content sets a CMYK fill totalling 320%, and the page's own content sets no colour
- **THEN** preflight reports the coverage warning against that page

#### Scenario: Low tint and unsupported colour space are distinguishable

- **WHEN** a file has both a tint below the reproducible minimum and an unsupported colour space
- **THEN** the two findings carry different codes

### Requirement: Structural checks

Preflight SHALL report the structures Lulu prohibits or ignores: encryption of any kind, annotations, form fields and the `AcroForm` dictionary, document-level and annotation-level JavaScript, embedded files, multimedia and 3D artwork, and a page layout requesting spreads.

The document catalog's `/Names` tree SHALL be resolved whether it is written as a direct dictionary or an indirect reference, so that JavaScript and embedded files are found in both encodings.

#### Scenario: Encrypted file is blocking

- **WHEN** the PDF carries an encryption dictionary, even with an empty user password
- **THEN** preflight reports a blocking finding, since Lulu prohibits security and password protection, and marks it fixable

#### Scenario: Annotations are reported

- **WHEN** pages carry link or text annotations
- **THEN** preflight reports a warning listing the annotation subtypes and affected pages, and marks them fixable by removal

#### Scenario: Spread layout is reported

- **WHEN** the catalog declares `/PageLayout /TwoPageLeft` or similar
- **THEN** preflight reports a warning that Lulu requires a single-page layout, and marks it fixable

#### Scenario: JavaScript behind an indirect Names tree is found

- **WHEN** a document's catalog carries `/Names 7 0 R`, where object 7 holds a `/JavaScript` name tree
- **THEN** preflight reports the document-level JavaScript finding exactly as it does for a direct `/Names` dictionary
