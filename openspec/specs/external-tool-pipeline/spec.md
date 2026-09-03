# external-tool-pipeline Specification

## Purpose
TBD - created by archiving change prepare-pdf-for-lulu. Update Purpose after archive.
## Requirements
### Requirement: External tools are optional

The tool SHALL be fully usable with no external binaries installed. Ghostscript and qpdf SHALL be optional stages that add capability when present, and their absence SHALL never abort a run that does not require them.

Ghostscript is AGPL-licensed, so it SHALL only ever be invoked as a subprocess and SHALL NOT be linked into the binary.

#### Scenario: Neither tool is installed

- **WHEN** the tool runs preflight and interior normalization on a machine with no Ghostscript and no qpdf
- **THEN** both complete successfully, and the report lists the stages that were skipped and what each would have fixed

#### Scenario: A requested stage is unavailable

- **WHEN** the caller explicitly requests transparency flattening and Ghostscript is not on the path
- **THEN** the run fails with an error naming the missing binary, the stage it powers, and how to install it, rather than silently producing an unflattened file

### Requirement: Capability detection

The tool SHALL detect each external binary once per run by resolving it on `PATH` or from an explicit configured path, invoking it for its version, and recording the name, path, and version in the report.

Detection SHALL apply a timeout, and SHALL treat a binary that fails to report a version as absent rather than hanging or crashing the run.

#### Scenario: Detected tools are recorded

- **WHEN** Ghostscript 10.03.1 and qpdf 11.9.0 are on the path
- **THEN** the report names both, with their resolved paths and versions

#### Scenario: Explicit path overrides PATH

- **WHEN** the caller configures an explicit Ghostscript path
- **THEN** that binary is used even if a different one is earlier on `PATH`, and the report shows the configured path

#### Scenario: Unresponsive binary is treated as absent

- **WHEN** a binary on the path does not return a version within the detection timeout
- **THEN** it is recorded as unavailable with the reason, and the run proceeds as if it were not installed

#### Scenario: Version below the supported minimum

- **WHEN** a detected binary's version is below the minimum the tool supports
- **THEN** it is recorded as unusable, naming the observed and required versions, and its stages are skipped

### Requirement: Structural repair and decryption via qpdf

When qpdf is available, the tool SHALL be able to run a structural repair stage that rebuilds the cross-reference table, removes encryption, and optionally linearizes the output.

Repair SHALL be attempted automatically when the native parser cannot read the input, and SHALL be available on request otherwise. After a successful repair the pipeline SHALL re-parse the repaired file and continue.

#### Scenario: Broken xref is repaired and the run continues

- **WHEN** a PDF with a broken cross-reference table is normalized and qpdf is available
- **THEN** the file is repaired, normalization proceeds on the repaired copy, and the report records that repair ran

#### Scenario: Repair is unavailable

- **WHEN** the same file is normalized and qpdf is absent
- **THEN** the run fails with a blocking finding naming the parse failure and stating that installing qpdf would enable repair

#### Scenario: Repair itself fails

- **WHEN** qpdf cannot repair the file
- **THEN** the run fails with an error including qpdf's own diagnostic output

### Requirement: Transparency flattening and colour conversion via Ghostscript

When Ghostscript is available, the tool SHALL be able to run a flattening and colour conversion stage that flattens live transparency, flattens optional content groups, and converts colour to a target space using a supplied ICC profile.

The stage SHALL be off by default, because it rewrites page content and can shift appearance. Its Ghostscript invocation SHALL preserve page geometry exactly, embed all fonts, and avoid downsampling images below 300 ppi. The report SHALL record the exact argument list used, so a result can be reproduced or audited.

#### Scenario: Flattening removes live transparency

- **WHEN** a file with soft masks and non-Normal blend modes is normalized with flattening enabled
- **THEN** the output's preflight reports no live transparency finding

#### Scenario: Geometry survives the stage

- **WHEN** any file passes through the Ghostscript stage
- **THEN** every output page's `MediaBox` and `TrimBox` are byte-for-byte the values normalization set, and the page count is unchanged

#### Scenario: Colour conversion uses the supplied profile

- **WHEN** the caller enables CMYK conversion and supplies a GRACoL ICC profile
- **THEN** the output's colour is converted through that profile, and the report names the profile file and its digest

#### Scenario: No profile supplied

- **WHEN** the caller enables CMYK conversion without supplying a profile
- **THEN** the run fails with an error stating that a profile is required, rather than falling back to an unspecified default conversion

#### Scenario: Invocation is recorded

- **WHEN** the Ghostscript stage runs
- **THEN** the report contains the full argument list, the exit status, and any stderr output

#### Scenario: Stage failure does not corrupt the output

- **WHEN** Ghostscript exits non-zero
- **THEN** no partial output replaces the pre-stage file, the run fails with Ghostscript's diagnostics, and the pre-stage file is either kept at a stated path or removed cleanly

### Requirement: Native image colour conversion

Independently of Ghostscript, the tool SHALL be able to convert embedded raster images between colour spaces natively using an ICC transform, for cases where only images need conversion and rewriting vector content is undesirable.

This stage SHALL leave vector colour operators untouched and SHALL say so in the report, so the caller is not misled into thinking the whole document was converted.

#### Scenario: Images are converted, vectors are not

- **WHEN** native image conversion runs on a file containing RGB images and RGB vector fills
- **THEN** the images are converted and the report states explicitly that vector colour was left in RGB and that Ghostscript or Lulu's own normalizer will handle it

#### Scenario: Unsupported image encoding is skipped

- **WHEN** an image uses an encoding the native path cannot decode
- **THEN** that image is left unchanged and a warning names it, rather than the stage failing

### Requirement: Stage ordering and idempotence

The pipeline SHALL run stages in a fixed order — repair, spread splitting, geometry, gutter, page padding, sanitation, then optional flattening and colour conversion — and SHALL produce byte-identical page geometry when the same input and options are run twice.

#### Scenario: Order is fixed and reported

- **WHEN** a run enables repair, flattening, and colour conversion
- **THEN** the report lists the stages in execution order with their durations

#### Scenario: Geometry is decided before content rewriting

- **WHEN** flattening is enabled
- **THEN** it runs after page geometry has been set, so flattening cannot change which rectangle is the trim

