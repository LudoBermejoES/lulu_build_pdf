# external-tool-pipeline (delta)

## ADDED Requirements

### Requirement: Every subprocess invocation is bounded and drained concurrently

Every invocation of an external tool SHALL be subject to a timeout, not only the capability-detection probe, because the invocations that process the document are the ones that can hang on hostile input.

Where a child process's output is captured, it SHALL be drained concurrently with the child's execution, so that a child writing more than a pipe buffer's worth of output cannot deadlock against a parent waiting for it to exit.

A timeout SHALL be reported as a stage failure naming the tool and the elapsed time, and SHALL leave the pre-stage file intact.

#### Scenario: A hanging repair is bounded

- **WHEN** qpdf is invoked to repair a file and does not exit within the configured timeout
- **THEN** the run fails reporting that qpdf timed out, the child is terminated, and no temporary file is left behind

#### Scenario: A hanging flatten is bounded

- **WHEN** the Ghostscript stage is invoked and Ghostscript does not exit within the configured timeout
- **THEN** the run fails reporting the timeout, and the pre-flatten file is unchanged

#### Scenario: Large tool output does not deadlock

- **WHEN** an invoked tool writes more output than a pipe buffer holds and then exits successfully
- **THEN** the invocation completes, and the captured output is complete rather than truncated

### Requirement: The report states what the pipeline actually did to the bytes

The report SHALL record whether the input was structurally repaired before analysis, so a reader can tell whether the bytes analysed were the ones supplied.

When a stage replaces the output, the conformance the report states SHALL describe the file that was actually written, not an earlier intermediate.

#### Scenario: A repair is recorded

- **WHEN** the native parser cannot read the input and qpdf repairs it before normalization
- **THEN** the report states that the input was repaired by qpdf, and the analysis is understood to apply to the repaired bytes

#### Scenario: The flattened output is the one described

- **WHEN** the Ghostscript stage runs and replaces the normalized output
- **THEN** the report's conformance verdict describes the flattened file, not the pre-flatten file

### Requirement: A failed repair explains itself

When structural repair is attempted and fails, the diagnostics from the repair tool SHALL be reported, rather than being discarded in favour of the original parse error and a suggestion to install a tool that already ran.

#### Scenario: qpdf ran and could not fix the file

- **WHEN** the native parser fails, qpdf is available, and qpdf also fails to repair the file
- **THEN** the reported error includes qpdf's own diagnostics and states that repair was attempted, rather than suggesting qpdf as an unattempted remedy

## MODIFIED Requirements

### Requirement: Transparency flattening and colour conversion via Ghostscript

When Ghostscript is available, the tool SHALL be able to run a flattening and colour conversion stage that flattens live transparency, flattens optional content groups, and converts colour to a target space using a supplied ICC profile.

The stage SHALL be off by default, because it rewrites page content and can shift appearance. Its Ghostscript invocation SHALL be bounded by a timeout, SHALL preserve page geometry exactly, embed all fonts, and avoid downsampling images below 300 ppi. The report SHALL record the exact argument list used, so a result can be reproduced or audited.

The geometry-preservation check SHALL compare actual numbers on both sides, and SHALL fail rather than pass when either side's box values cannot be read as four numbers.

#### Scenario: Flattening removes live transparency

- **WHEN** a file with soft masks and non-Normal blend modes is normalized with flattening enabled
- **THEN** the output's preflight reports no live transparency finding

#### Scenario: Geometry survives the stage

- **WHEN** any file passes through the Ghostscript stage
- **THEN** every output page's `MediaBox` and `TrimBox` are byte-for-byte the values normalization set, and the page count is unchanged

#### Scenario: An unreadable box fails the geometry check

- **WHEN** a page's box entries cannot be read as four numbers on either side of the stage
- **THEN** the geometry-preservation check fails naming that page, rather than passing because both sides yielded nothing to compare

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
