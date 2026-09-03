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

### Requirement: Output paths and safety

The CLI SHALL derive default output paths from the input name and role — an interior producing a `-interior` suffixed file and a cover a `-cover` suffixed file — inside a caller-specified directory, and SHALL never write over an existing file without `--force`.

The CLI SHALL support a dry-run mode that performs all analysis and prints the report and the intended output paths without writing any PDF.

#### Scenario: Default output paths are predictable

- **WHEN** the user normalizes `book.pdf` for a product without naming an output
- **THEN** the output is written as `book-interior.pdf` in the chosen output directory, and the path is printed

#### Scenario: Overwrite requires force

- **WHEN** the derived output path already exists and `--force` is absent
- **THEN** the CLI exits with code 2, names the existing path, and writes nothing

#### Scenario: Dry run writes nothing

- **WHEN** the user passes the dry-run option
- **THEN** the full report is printed, the intended output paths are listed, and no file is created or modified

### Requirement: Report presentation

The CLI SHALL print a human-readable report to stdout by default and SHALL write a JSON report when asked, either to a path or to stdout. When JSON goes to stdout, no human-readable text SHALL be mixed into it.

Progress and diagnostic messages SHALL go to stderr, and colour SHALL be disabled when stdout is not a terminal or when a no-colour setting is present.

#### Scenario: JSON on stdout is parseable

- **WHEN** the user requests JSON output to stdout
- **THEN** stdout contains only the JSON document, and all progress messages appear on stderr

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

