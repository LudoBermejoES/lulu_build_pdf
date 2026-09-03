# cover-preparation (delta)

## ADDED Requirements

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

## MODIFIED Requirements

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

#### Scenario: Casewrap uses the wider margin measured from the trim

- **WHEN** the product is hardcover case wrap
- **THEN** the safety margin applied is 0.750 in, measured inward from the trim rectangle, so the safe area begins 0.875 + 0.750 in from the canvas edge
