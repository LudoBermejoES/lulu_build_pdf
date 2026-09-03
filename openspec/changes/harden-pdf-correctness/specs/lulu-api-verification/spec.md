# lulu-api-verification (delta)

## MODIFIED Requirements

### Requirement: Cover dimension verification

When verification is enabled, the tool SHALL call the `cover-dimensions` endpoint with the product and final interior page count, and SHALL compare Lulu's answer against its own computed canvas.

A disagreement beyond 1 pt in either dimension SHALL be a blocking finding, since it means the local formula or the catalog is wrong for that product. For hardcover products, Lulu's answer SHALL be treated as authoritative and used in place of any local estimate.

Lulu's canvas dimensions SHALL be used to build panel geometry only for bindings whose panel model this tool implements. An authoritative canvas SHALL NOT be used to infer a panel layout the tool has declared it cannot model: a linen-wrap dust jacket has front and back flaps, so dividing its canvas into three panels around a centred spine produces wrong fold positions and wrong panel widths, and the API path SHALL refuse it exactly as the local path does.

The binding-to-geometry decision SHALL be made in one place shared by the local and API paths, so the two cannot disagree about which bindings are supported. An unexpected binding SHALL be an error rather than being treated as spineless.

#### Scenario: Local and remote geometry agree

- **WHEN** verification runs for a 6 × 9 in perfect-bound product with 210 pages and Lulu returns 920 × 666 pt
- **THEN** the report records agreement with the locally computed canvas

#### Scenario: Disagreement is blocking

- **WHEN** Lulu returns a canvas differing from the local computation by more than 1 pt
- **THEN** a blocking finding names both values, the product, and the page count, and the run's exit status reflects a blocking finding

#### Scenario: Hardcover dimensions come from the API

- **WHEN** verification runs for a case wrap product whose geometry is absent from the local template table
- **THEN** Lulu's returned dimensions are used to build the cover, and the report records that the geometry came from the API

#### Scenario: Linen wrap is refused even with an authoritative canvas

- **WHEN** verification returns a canvas for a linen wrap product
- **THEN** the tool refuses to build three-panel geometry from it, reporting that the dust-jacket panel model is not implemented, rather than centring a spine in the canvas

#### Scenario: An unsupported binding is an error

- **WHEN** the API geometry path is given a binding for which no spine rule exists
- **THEN** it returns an error naming the binding, rather than producing a spineless three-panel geometry
