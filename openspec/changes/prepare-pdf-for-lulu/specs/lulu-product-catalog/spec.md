## ADDED Requirements

### Requirement: POD package ID parsing

The library SHALL parse a Lulu `pod_package_id` in both published forms into a structured product descriptor exposing trim size, ink, print quality, binding, paper, interior PPI, cover lamination, linen colour, and foil colour.

The two forms are the current dotted form `[Trim].[Ink].[Quality].[Binding].[Paper].[Finish]` (for example `0600X0900.BW.STD.PB.060UW444.MXX`) and the legacy 27-character undotted form (for example `0600X0900BWSTDPB060UW444MXX`), which Lulu retires on 2027-02-01.

#### Scenario: Dotted ID is parsed

- **WHEN** the caller parses `0600X0900.FC.STD.PB.080CW444.GXX`
- **THEN** the descriptor reports trim size 6.000 × 9.000 in, ink full colour, quality standard, binding perfect, paper 80# coated white, interior PPI 444, lamination gloss, no linen, and no foil

#### Scenario: Legacy ID is parsed and flagged

- **WHEN** the caller parses `0600X0900BWSTDPB060UW444MXX`
- **THEN** the descriptor is equivalent to the one parsed from `0600X0900.BW.STD.PB.060UW444.MXX`
- **AND** the result carries a deprecation notice naming the dotted equivalent and the 2027-02-01 end-of-support date

#### Scenario: Malformed ID is rejected

- **WHEN** the caller parses a string whose trim segment, segment count, or component code is not recognised
- **THEN** parsing fails with an error naming the offending segment and its position, and no partial descriptor is returned

### Requirement: Embedded product catalog

The library SHALL embed Lulu's published product catalog and resolve a `pod_package_id` to that product's authoritative specification without network access.

For each product the catalog SHALL provide: both SKU forms, book type, minimum and maximum interior page count, trim width and height, width and height with bleed, interior colour, print quality, binding, paper type, and interior PPI. The catalog SHALL record the source URL and the date it was fetched, and SHALL be regenerable by a checked-in script.

#### Scenario: Known product is resolved offline

- **WHEN** the caller looks up `0600X0900.BW.STD.PB.060UW444.MXX` with no network available
- **THEN** the catalog returns trim 6.000 × 9.000 in, size with bleed 6.250 × 9.250 in, binding perfect, interior PPI 444, minimum 32 pages, and maximum 800 pages

#### Scenario: Unknown but well-formed product

- **WHEN** the caller looks up a syntactically valid `pod_package_id` that is absent from the embedded catalog
- **THEN** lookup fails with an error stating that the SKU is not in the catalog, naming the catalog's fetch date, and pointing to the regeneration script

#### Scenario: Catalog provenance is reportable

- **WHEN** the caller requests catalog metadata
- **THEN** the library returns the source URL, the fetch date, and the number of products, so a report can state which catalog revision a decision was based on

### Requirement: Bleed and safety geometry

The library SHALL derive page geometry from the product's trim size using Lulu's published allowances: bleed is 0.125 in (9 pt) beyond the trim edge on all four sides, so the required page size is the trim size plus 0.250 in in each dimension.

The interior safety margin SHALL be 0.500 in inside the trim edge. The cover safety margin SHALL be 0.250 in inside the trim edge, except hardcover casewrap, which SHALL be 0.750 in.

#### Scenario: Page size with bleed is derived

- **WHEN** the caller requests the required interior page size for a 6 × 9 in product
- **THEN** the library returns 6.250 × 9.250 in (450.0 × 666.0 pt)

#### Scenario: Derived geometry agrees with the catalog

- **WHEN** the derived size with bleed is compared against the catalog's own `Width w/ Bleed` and `Height w/ Bleed` columns for every product
- **THEN** the two agree for every product in the catalog, within 0.01 in

### Requirement: Gutter allowance by page count

The library SHALL return the inner-edge gutter allowance and total interior margin for a given interior page count, using Lulu's published stepped table: up to 60 pages, 0.000 in gutter and 0.500 in margin; 61–150 pages, 0.125 in and 0.625 in; 151–400 pages, 0.500 in and 1.000 in; 401–600 pages, 0.625 in and 1.125 in; 601 pages and above, 0.750 in and 1.250 in.

The function SHALL be total over all page counts of one or more. Because Lulu's PDF creation settings separately advise a minimum 0.200 in gutter, the library SHALL flag — but not silently override — any table result below 0.200 in.

#### Scenario: Gutter for a mid-size book

- **WHEN** the caller requests the gutter allowance for 210 interior pages
- **THEN** the library returns a 0.500 in gutter and a 1.000 in total interior margin

#### Scenario: Thin book falls below Lulu's advisory floor

- **WHEN** the caller requests the gutter allowance for 40 interior pages
- **THEN** the library returns a 0.000 in gutter
- **AND** the result is flagged as below Lulu's separately published 0.200 in advisory minimum

#### Scenario: Boundary page counts resolve unambiguously

- **WHEN** the caller requests the gutter allowance for 60, 61, 150, 151, 400, 401, 600, and 601 pages in turn
- **THEN** each call returns exactly one band, and no page count maps to two bands or to none

### Requirement: Page count rules

The library SHALL report, for a given product, the minimum and maximum interior page count from the catalog and the divisibility rule its binding imposes, and SHALL compute the smallest conformant page count greater than or equal to a supplied count.

Coil and Wire-O bindings SHALL require a multiple of 2. All other bindings — perfect, saddle stitch, case wrap, and linen wrap — SHALL require a multiple of 4. Lulu's file validation additionally rejects interiors of fewer than 2 pages regardless of product.

#### Scenario: Count is padded to the binding's multiple

- **WHEN** the caller asks for the conformant page count for 205 pages on a perfect-bound product
- **THEN** the library returns 208, being the smallest multiple of 4 at or above 205

#### Scenario: Count is raised to the product minimum

- **WHEN** the caller asks for the conformant page count for 18 pages on a perfect-bound product whose catalog minimum is 32
- **THEN** the library returns 32

#### Scenario: Count exceeds the product maximum

- **WHEN** the caller asks for the conformant page count for 812 pages on a product whose catalog maximum is 800
- **THEN** the library reports that no conformant count exists and names the maximum, rather than returning a truncated count

#### Scenario: Saddle stitch requires a multiple of four

- **WHEN** the caller asks for the conformant page count for 30 pages on a saddle-stitch product with a 4–48 page range
- **THEN** the library returns 32

### Requirement: Spine width

The library SHALL compute spine width from the product and the final interior page count, using Lulu's published rules.

For perfect binding, spine width SHALL be `page_count / interior_ppi + 0.06 in`, where `interior_ppi` is the product's pages-per-inch bulk from the SKU — 444 for standard papers and 460 for the magazine and comic papers. For hardcover case wrap and linen wrap, spine width SHALL come from Lulu's stepped lookup table, which starts at 0.250 in for 24–84 pages and ends at 2.125 in at 800 pages, and which returns no spine below 24 pages. For saddle stitch, coil, and Wire-O, spine width SHALL be zero, as these bindings have no spine.

#### Scenario: Perfect-bound spine on 444 ppi paper

- **WHEN** the caller computes the spine width for 210 pages on a perfect-bound 444 ppi product
- **THEN** the library returns 0.533 in, within 0.001 in

#### Scenario: Perfect-bound spine on 460 ppi paper

- **WHEN** the caller computes the spine width for 210 pages on a perfect-bound 460 ppi magazine or comic product
- **THEN** the library returns 0.517 in, within 0.001 in

#### Scenario: Hardcover spine comes from the table

- **WHEN** the caller computes the spine width for 210 pages on a case wrap product
- **THEN** the library returns 0.750 in, the tabulated width for the 195–222 page band, and does not apply the perfect-bound formula

#### Scenario: Hardcover below the table's floor

- **WHEN** the caller computes the spine width for 20 pages on a case wrap product
- **THEN** the library reports that no spine width is defined below 24 pages, consistent with the product's 24-page minimum

#### Scenario: Spineless bindings

- **WHEN** the caller computes the spine width for any page count on a saddle-stitch, coil, or Wire-O product
- **THEN** the library returns zero and marks the product as having no printable spine

### Requirement: Cover wrap dimensions

The library SHALL compute the full cover canvas for a product and final page count.

For perfect binding, the canvas width SHALL be `2 × trim_width + spine_width + 2 × 0.125 in` and the canvas height SHALL be `trim_height + 2 × 0.125 in`, rounded to the nearest whole point when reported in points.

For hardcover case wrap and linen wrap the geometry additionally involves board overhang, a 0.750 in wrap allowance, and a 0.250 in hinge either side of the spine. The library SHALL NOT derive these dimensions from an inferred formula: it SHALL take them from a checked-in table transcribed from Lulu's own downloadable cover templates, or from the Print API `cover-dimensions` endpoint. Any locally estimated hardcover dimension SHALL be labelled as an estimate and SHALL NOT be used to produce a final cover file.

#### Scenario: Perfect-bound cover matches Lulu's own figure

- **WHEN** the caller computes cover dimensions for `0600X0900.BW.STD.PB.060UW444.MXX` with 210 interior pages, in points
- **THEN** the library returns 920 × 666 pt, matching the worked example published for Lulu's `cover-dimensions` endpoint

#### Scenario: Hardcover dimensions are never guessed

- **WHEN** the caller computes cover dimensions for a case wrap product and no template table entry and no API result are available
- **THEN** the library returns an estimate explicitly marked as unverified
- **AND** any attempt to write a final cover file from that estimate is refused with an error directing the caller to Lulu's template download or the `cover-dimensions` endpoint

#### Scenario: Spine text placement guidance

- **WHEN** the caller requests spine geometry for a product whose spine width is under 0.125 in
- **THEN** the library reports that the spine is too narrow to carry text reliably, so a caller generating a template can omit the spine text zone
