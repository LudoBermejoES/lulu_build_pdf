# cli Specification

## Purpose
TBD - created by archiving change prepare-pdf-for-lulu. Update Purpose after archive.
## Requirements
### Requirement: Command surface

The CLI SHALL be a single binary named `lulu-prep` exposing these subcommands:

- `check` — preflight an existing PDF against a product and report findings, writing no PDF.
- `interior` — normalize an interior PDF for a product.
- `cover` — generate a cover template, or fit supplied cover artwork, for a product and interior page count.
- `book` — run `interior` then `cover` in one pass, taking the cover's page count from the normalized interior.
- `products` — search and describe the embedded catalog.
- `spine` — print the spine width and cover canvas for a product and page count.

Every subcommand SHALL support `--help` describing its options with units stated explicitly.

#### Scenario: Help is self-describing

- **WHEN** the user runs `lulu-prep interior --help`
- **THEN** every option is listed with its default and, where it is a measurement, its unit

#### Scenario: Book command keeps the pair in step

- **WHEN** the user runs `lulu-prep book` with an interior PDF and a product
- **THEN** the interior is normalized first, and the cover is built from the normalized interior's final page count without the user supplying it

#### Scenario: Spine query needs no PDF

- **WHEN** the user runs `lulu-prep spine` with a product and a page count
- **THEN** the spine width and cover canvas are printed without any PDF input

### Requirement: Product selection

A product SHALL be selectable either by `pod_package_id` in dotted or legacy form, or by naming its components — trim size, ink, quality, binding, paper, and finish — which the CLI resolves to a single SKU.

When component selection is ambiguous, the CLI SHALL list the matching SKUs and exit without acting, rather than choosing one.

#### Scenario: SKU is given directly

- **WHEN** the user passes `--sku 0600X0900.BW.STD.PB.060UW444.MXX`
- **THEN** that product is used and its resolved specification is echoed in the report header

#### Scenario: Components resolve to one product

- **WHEN** the user passes a trim size, binding, paper, ink, quality, and finish that together match exactly one catalog entry
- **THEN** that entry is used and its `pod_package_id` is printed

#### Scenario: Ambiguous components are refused

- **WHEN** the user passes only a trim size and a binding, matching many SKUs
- **THEN** the CLI lists the candidate SKUs with their distinguishing attributes and exits non-zero without writing any file

#### Scenario: Catalog search

- **WHEN** the user runs `lulu-prep products` with a trim size filter
- **THEN** the matching products are listed with SKU, book type, trim size, size with bleed, binding, paper, and page-count range

### Requirement: Configuration precedence

Options SHALL resolve in a fixed precedence: command-line flags, then environment variables, then a project configuration file, then a user configuration file, then built-in defaults. The effective configuration SHALL be printable, with each value's source named.

#### Scenario: Flag beats configuration file

- **WHEN** a fit mode is set in the configuration file and a different one is passed on the command line
- **THEN** the command-line value is used

#### Scenario: Effective configuration is inspectable

- **WHEN** the user asks the CLI to print its effective configuration
- **THEN** each option is listed with its value and whether it came from a flag, the environment, a config file, or a default

### Requirement: Exit codes

The CLI SHALL use distinct exit codes so it can drive a build: `0` when no blocking finding remains, `1` when the run completed but blocking findings remain, `2` for invalid usage or an unresolvable product, `3` for an I/O or PDF parse failure that prevented completion, and `4` for a required external tool or credential that was missing.

The code reported SHALL be the one the failing operation determined. A failure classified as an I/O failure SHALL NOT be reported as invalid usage.

Warnings SHALL NOT affect the exit code by default, and a strict option SHALL make warnings exit non-zero.

#### Scenario: Clean run succeeds

- **WHEN** normalization completes and preflight of the output reports no blocking findings
- **THEN** the exit code is 0

#### Scenario: Blocking findings remain

- **WHEN** the output still has an unembedded font that the tool cannot fix
- **THEN** the exit code is 1, and the report names the finding

#### Scenario: Strict mode promotes warnings

- **WHEN** the run has warnings but no blocking findings and the strict option is set
- **THEN** the exit code is 1

#### Scenario: Missing external tool

- **WHEN** flattening was explicitly requested and Ghostscript is absent
- **THEN** the exit code is 4

#### Scenario: A write failure is an I/O failure

- **WHEN** the output path is unwritable because the volume is full or the directory is not writable
- **THEN** the exit code is 3, not 2

#### Scenario: An overwrite refusal is a usage failure

- **WHEN** the derived output path exists and `--force` is absent
- **THEN** the exit code is 2 and nothing is written

### Requirement: Output paths and safety

The CLI SHALL derive default output paths from the input name and role — an interior producing a `-interior` suffixed file and a cover a `-cover` suffixed file — inside a caller-specified directory, and SHALL never write over an existing file without `--force`.

When the name a path is derived from is a product identifier rather than a file path, the whole identifier SHALL be preserved, so that two products differing only in a dotted trailing segment cannot derive the same filename.

The CLI SHALL support a dry-run mode that performs all analysis and prints the report and the intended output paths without writing any PDF.

#### Scenario: Default output paths are predictable

- **WHEN** the user normalizes `book.pdf` for a product without naming an output
- **THEN** the output is written as `book-interior.pdf` in the chosen output directory, and the path is printed

#### Scenario: A product identifier is not truncated at its dots

- **WHEN** a template cover is generated for `0600X0900.BW.STD.PB.060UW444.MXX` without naming an output
- **THEN** the derived filename retains the full identifier including the `.MXX` segment, so that the matte and gloss variants of one product cannot collide

#### Scenario: Overwrite requires force

- **WHEN** the derived output path already exists and `--force` is absent
- **THEN** the CLI exits with code 2, names the existing path, and writes nothing

#### Scenario: Dry run writes nothing

- **WHEN** the user passes the dry-run option
- **THEN** the full report is printed, the intended output paths are listed, and no file is created or modified

### Requirement: Report presentation

The CLI SHALL print a human-readable report to stdout by default and SHALL write a JSON report when asked, either to a path or to stdout. When JSON goes to stdout, no human-readable text SHALL be mixed into it.

A single invocation SHALL emit exactly one report document, even when it prepares more than one file. A command that produces both an interior and a cover SHALL emit one document containing both, so that its JSON output is parseable and a report written to a path is not truncated by a second write.

Progress and diagnostic messages SHALL go to stderr, and colour SHALL be disabled when stdout is not a terminal or when a no-colour setting is present.

#### Scenario: JSON on stdout is parseable

- **WHEN** the user requests JSON output to stdout
- **THEN** stdout contains exactly one JSON document, and all progress messages appear on stderr

#### Scenario: A two-file command emits one document

- **WHEN** the `book` command runs with JSON output
- **THEN** stdout contains a single JSON document carrying both the interior and the cover reports, and it parses successfully

#### Scenario: A two-file command does not truncate a report file

- **WHEN** the `book` command runs with a report path
- **THEN** that file contains both reports after the run, not only the last one written

#### Scenario: Report leads with the verdict

- **WHEN** any report is printed
- **THEN** its first line states the verdict, the product, and the final page count

#### Scenario: Colour respects the environment

- **WHEN** stdout is redirected to a file
- **THEN** the output contains no ANSI escape sequences

### Requirement: Reproducibility

Given the same input file, product, and options, two runs SHALL produce reports that differ only in fields that are inherently variable — timestamps, durations, and external tool versions — and SHALL produce output PDFs with identical page geometry and page count.

#### Scenario: Reports are diffable

- **WHEN** the same command runs twice and the two JSON reports are compared with timestamps and durations excluded
- **THEN** the remaining content is identical

#### Scenario: Deterministic PDF identity

- **WHEN** the same command runs twice with a fixed document identifier and creation date supplied
- **THEN** the two output PDFs are byte-identical

### Requirement: An invalid option value is an error, never a silent default

A value supplied for an option — on the command line, in the environment, or in a configuration file — that cannot be parsed SHALL cause the run to fail with exit code 2, naming the option, the offending value, and the accepted values.

An unparseable value SHALL NOT fall through to a lower-precedence layer or to the built-in default, because that silently changes the geometry of the output while appearing to honour the request.

#### Scenario: A misspelled fit mode fails rather than centring

- **WHEN** `LULU_PREP_FIT_MODE=scaletobleed` is set
- **THEN** the run exits 2 naming the variable and listing `center`, `scale-to-bleed`, and `stretch-margins`, rather than silently producing centred output

#### Scenario: An unparseable boolean fails rather than disabling the option

- **WHEN** `LULU_PREP_STRICT=yes-please` is set
- **THEN** the run exits 2 rather than silently running without strict mode

#### Scenario: Every accepted option is actually consumed

- **WHEN** the effective configuration is printed
- **THEN** every value it lists is one that the run actually reads, and no accepted option is resolved and displayed while being ignored

### Requirement: Malformed path and identifier arguments are reported, not fatal

An argument that cannot be interpreted — a non-UTF-8 path, or a document identifier that is not exactly 32 hexadecimal characters — SHALL produce exit code 2 with a message naming the argument, and SHALL NOT panic.

Options that only take effect in combination SHALL report when they are supplied incompletely.

#### Scenario: A non-UTF-8 output path is rejected cleanly

- **WHEN** an output path that is not valid UTF-8 is supplied
- **THEN** the run exits 2 naming the path argument, and the process does not panic

#### Scenario: A malformed document identifier is rejected cleanly

- **WHEN** `--doc-id` is supplied with a 32-byte value containing a multi-byte character
- **THEN** the run exits 2 stating that 32 hexadecimal characters are required, and the process does not panic

#### Scenario: A partially specified reproducibility request is reported

- **WHEN** `--doc-id` is supplied without `--creation-date`, or the reverse
- **THEN** the run reports that both are required for byte-identical output, rather than silently producing non-reproducible output

