# cover-preparation Specification

## Purpose
TBD - created by archiving change prepare-pdf-for-lulu. Update Purpose after archive.
## Requirements
### Requirement: Cover geometry is derived from the final interior

Cover preparation SHALL take the product and the *final* interior page count — the count after normalization padding — and derive the cover canvas, spine rectangle, front and back panel rectangles, fold positions, and safety rectangles from it.

Because the spine width changes with every page-count change, cover preparation SHALL refuse to run against a page count that does not satisfy the product's page rules, and SHALL record in its output which page count the geometry was built for.

#### Scenario: Geometry is reported in full

- **WHEN** cover geometry is requested for a 6 × 9 in perfect-bound product with 212 final interior pages (the nearest multiple of 4 to Lulu's own 210-page `cover-dimensions` worked example, since 210 is not itself a conformant final count for this binding)
- **THEN** the result gives approximately a 920 × 666 pt canvas (920.7 × 666, within a point of Lulu's published example), a 38.7 pt wide spine centred horizontally, back and front panels of 441 × 666 pt (trim width plus one bleed, by the full canvas height), the two fold x-positions, and the page count the geometry was built for

#### Scenario: Non-conformant page count is refused

- **WHEN** cover geometry is requested for 205 pages on a perfect-bound product, which requires a multiple of 4
- **THEN** the request fails with an error stating that 205 is not a conformant count and naming 208 as the next valid one

#### Scenario: Interior and cover stay in step

- **WHEN** a cover is prepared from a normalized interior file
- **THEN** the page count used is read from that interior file rather than supplied separately, so the spine cannot silently disagree with the book

### Requirement: Cover template generation

Cover preparation SHALL be able to write a blank cover template PDF at the exact canvas size, marked up with non-printing guides for the trim edges, the bleed edges, the spine folds, and the safety margins, plus a legend stating the product, page count, spine width, and canvas size.

The trim guide SHALL be drawn at the geometry's trim rectangle, and the safety guides SHALL be inset from that trim rectangle rather than from the panel rectangles, so that a designer trusting the guides places content where it will actually survive trimming and, for hardcover, wrapping around the board.

The guides SHALL be placed in an optional content group named so that a designer can hide or delete them, and the template SHALL be labelled as a working file that must not itself be submitted to Lulu.

#### Scenario: Template carries the right canvas and guides

- **WHEN** a template is generated for a 6 × 9 in perfect-bound product with 210 pages
- **THEN** the PDF is a single 920 × 666 pt page containing labelled guides for trim, bleed, both spine folds, and safety margins

#### Scenario: Case wrap guides sit at the board overhang

- **WHEN** a template is generated for a 6 × 9 in case wrap product at 212 pages
- **THEN** the trim guide is 63 pt from each canvas edge and the outer safety guides are a further 54 pt (0.750 in) inside that, not 9 pt and 54 pt from the canvas edge

#### Scenario: Guides are removable

- **WHEN** the generated template is opened in a PDF editor
- **THEN** the guides sit in one clearly named optional content group, separate from any artwork

#### Scenario: Template is marked not for submission

- **WHEN** a template is generated
- **THEN** its legend and its document metadata state that it is a design aid and not a submittable cover

### Requirement: Fitting supplied cover artwork

Cover preparation SHALL be able to place a supplied single-page cover PDF onto the correct canvas, reporting how well the supplied artwork matches the required geometry.

The supplied artwork's size SHALL be measured as its effective (rotation-applied) size, and SHALL be compared against the required canvas in **both** dimensions, so that artwork of the correct width but the wrong height is reported rather than silently scaled.

A supplied file that is not a single-page PDF SHALL be reported as an error naming the problem, and SHALL NOT cause a panic.

When the artwork's size already equals the required canvas within 0.5 pt, it SHALL be passed through with its boxes corrected and no scaling. When it differs, the caller's fit mode SHALL decide, using the same `center`, `scale-to-bleed`, and `stretch-margins` modes as the interior. When the artwork is supplied as three separate files for back, spine, and front, they SHALL be assembled in that order at the computed panel positions, each clipped to its destination panel and aligned to the canvas edge on its outer side rather than centred within the panel.

#### Scenario: Correctly sized artwork passes through

- **WHEN** a 920 × 666 pt cover is supplied for a geometry requiring 920 × 666 pt
- **THEN** the output is unscaled and only the page boxes are corrected

#### Scenario: Wrong-spine artwork is caught

- **WHEN** a supplied cover is 900 × 666 pt against a required 920 × 666 pt, a 20 pt shortfall consistent with a spine computed for the wrong page count
- **THEN** cover preparation reports a blocking finding naming the shortfall, stating the page count the required spine implies, and does not silently stretch the artwork

#### Scenario: Wrong height is caught

- **WHEN** a supplied cover is 920 × 648 pt against a required 920 × 666 pt
- **THEN** cover preparation reports a finding naming the height mismatch rather than silently scaling or centring the artwork

#### Scenario: Rotated artwork is measured as displayed

- **WHEN** supplied cover artwork carries `/Rotate 90` such that its displayed size matches the required canvas
- **THEN** it is treated as correctly sized, and no spurious mismatch finding is reported

#### Scenario: A supplied file with no pages is an error

- **WHEN** the supplied cover file is a structurally valid PDF whose page tree contains no pages
- **THEN** cover preparation fails with an error naming the problem and the process does not panic

#### Scenario: Panels are assembled from separate files

- **WHEN** back, spine, and front artwork are supplied as three files
- **THEN** the output places them at the computed back panel, spine, and front panel rectangles, in that left-to-right order

#### Scenario: Oversized panel artwork cannot spill across a fold

- **WHEN** supplied back-cover artwork is wider than its destination panel
- **THEN** it is clipped to that panel, so no part of it appears across the spine fold, and a finding reports the mismatch

#### Scenario: Cover structural rules are applied

- **WHEN** any cover is written
- **THEN** it is a single-page PDF with no annotations, no encryption, no trim or bleed marks, and a `TrimBox` set to the geometry's trim rectangle for that binding

### Requirement: Cover safety margin checks

Cover preparation SHALL report artwork placed inside the safety margins or crossing the spine folds, using the product's cover safety margin: 0.250 in inside the trim edge, or 0.750 in for hardcover case wrap.

The safety rectangle SHALL be computed by insetting from the trim rectangle, not from the panel rectangle, so that the reported safe area does not include the bleed or board overhang.

The narrow-spine warning SHALL be emitted by the cover preparation path, not merely be available as an unused helper.

#### Scenario: Text too close to the trim

- **WHEN** supplied cover artwork places text 0.100 in from the trim edge on a paperback
- **THEN** a warning names the region, the observed clearance, and the 0.250 in requirement

#### Scenario: Spine text on a narrow spine

- **WHEN** a cover is prepared for a product whose spine is narrower than 0.125 in
- **THEN** the report carries a warning that the spine is too narrow to hold text reliably given Lulu's binding variance

#### Scenario: Casewrap uses the wider margin

- **WHEN** the product is hardcover case wrap
- **THEN** the safety margin applied is 0.750 in, measured inward from the trim rectangle, so the safe area begins 0.875 + 0.750 in from the canvas edge

### Requirement: Hardcover geometry is not invented

Cover preparation SHALL NOT write a final cover file from a locally inferred formula that has not been confirmed against Lulu's own data.

For hardcover case wrap, the canvas composition is Lulu-confirmed: `canvas_width = 2 * trim_width + spine_width + 2 * 0.875 in`, `canvas_height = trim_height + 2 * 0.875 in`, verified live against the Print API's `cover-dimensions` endpoint across two trim sizes and five page counts spanning the full 24–800 page range, with a 0.250 in hinge on each side of the spine. This is computed directly, the same as perfect binding's formula, rather than looked up.

For hardcover linen wrap, cover preparation SHALL obtain the canvas dimensions from the checked-in template table or from the Print API `cover-dimensions` endpoint, and SHALL refuse rather than guess when neither is available. Linen wrap ships with a dust jacket — a probe of a real linen wrap product returned a canvas nothing like case wrap's at the same page count, consistent with a flap-based panel layout (front flap, front, spine, back, back flap) rather than the plain back/spine/front model case wrap and perfect binding share. Reconstructing that layout from a locally inferred formula is exactly the guess this requirement exists to prevent.

#### Scenario: Case wrap geometry is computed, not looked up

- **WHEN** a case wrap cover is prepared for a product and a conformant page count
- **THEN** the canvas, fold positions, and hinge zone are computed directly from the verified formula, with no template table or API call required

#### Scenario: Unverified linen wrap geometry blocks output

- **WHEN** a linen wrap cover is prepared and neither the template table nor the API can supply dimensions
- **THEN** preparation fails with an error directing the caller to download Lulu's template or enable API verification
- **AND** any advisory estimate shown in the report is explicitly labelled unverified

#### Scenario: Hinge zone is reported

- **WHEN** hardcover geometry is available (case wrap always; linen wrap when the table or API supplies it)
- **THEN** the reported rectangles include the 0.250 in hinge zone on each side of the spine, so a designer can keep artwork out of the fold

### Requirement: Combined preview PDF

Cover preparation SHALL be able to produce a combined preview PDF, for human review only, splitting the prepared cover into a front-cover page placed first, the normalized interior's pages in order, and a back-cover page placed last.

The preview SHALL be built by splitting the cover canvas at its two fold positions into separate front and back pages sized to the product's trim (not the full wrap-with-spine width), never by including the spine panel as its own page. The preview is derived output: it SHALL carry a legend and document metadata stating it is a proof, not a submittable file, and producing it SHALL NOT alter, replace, or become a substitute for the separate interior and cover files Lulu's Print API requires.

#### Scenario: Preview page order

- **WHEN** a preview is built from a prepared cover and a 32-page normalized interior
- **THEN** the output has 34 pages: the front cover panel, then the 32 interior pages in order, then the back cover panel

#### Scenario: Preview pages are trim-sized, not wrap-sized

- **WHEN** a preview is built for a 6 × 9 in product
- **THEN** the front-cover and back-cover pages are each sized to the product's page-with-bleed geometry, not the full cover canvas width that includes the spine

#### Scenario: Preview is marked as non-submittable

- **WHEN** a preview PDF is generated
- **THEN** its legend and document metadata state it is a proof for review, not a file to upload to Lulu

#### Scenario: Preview generation does not affect the real outputs

- **WHEN** a preview is generated alongside a normal `book` run
- **THEN** the separate interior file and the wrap-format cover file are produced exactly as they would be without the preview, unchanged in content or geometry

### Requirement: The trim rectangle is derived from the product, not from a bleed constant

Cover geometry SHALL carry its trim rectangle explicitly, computed by whichever geometry rule produced the canvas, because the distance from the canvas edge to the trim edge differs by binding: 0.125 in of bleed for a perfect-bound cover, but 0.875 in of board overhang for hardcover case wrap.

Every consumer of that trim rectangle — the `TrimBox` and `ArtBox` written onto a cover page, the trim guide drawn on a template, and the origin from which safety margins are inset — SHALL read it from the geometry rather than recomputing it from a bleed constant.

#### Scenario: Case wrap trim reflects the board overhang

- **WHEN** a cover is prepared for a 6 × 9 in hardcover case wrap product at 212 pages, whose canvas is 1044 × 774 pt
- **THEN** the trim rectangle is inset 63 pt (0.875 in) from each canvas edge, and the page's `TrimBox` records that rectangle rather than a 9 pt inset

#### Scenario: Perfect-bound trim reflects the bleed

- **WHEN** a cover is prepared for a 6 × 9 in perfect-bound product
- **THEN** the trim rectangle is inset 9 pt (0.125 in) from each canvas edge

### Requirement: Degenerate rectangles are never drawn or written

A rectangle inset by more than half its own width or height SHALL NOT be produced with inverted edges. An inset that would invert a rectangle SHALL be reported rather than drawn, so that a template never displays a mirrored guide box straddling adjacent panels.

#### Scenario: A spine narrower than twice its safety margin

- **WHEN** the safety margin is inset from a spine panel narrower than twice that margin
- **THEN** no inverted rectangle is drawn, and the template instead reports that the spine has no usable safe area

