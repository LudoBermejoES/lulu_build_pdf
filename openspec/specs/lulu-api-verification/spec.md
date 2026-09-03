# lulu-api-verification Specification

## Purpose
TBD - created by archiving change prepare-pdf-for-lulu. Update Purpose after archive.
## Requirements
### Requirement: API verification is opt-in and compiled out by default

Verification against Lulu's Print API SHALL sit behind an off-by-default Cargo feature and an explicit runtime flag. With the feature disabled the tool SHALL contain no HTTP client and SHALL make no network request under any invocation.

#### Scenario: Default build makes no network calls

- **WHEN** the tool is built with default features and run on any input
- **THEN** no network connection is attempted, and no credential is read from the environment

#### Scenario: Feature enabled but flag absent

- **WHEN** the tool is built with the API feature and run without the verification flag
- **THEN** it behaves exactly as the default build, and the report notes that API verification was available but not requested

### Requirement: Credentials and environment selection

The tool SHALL obtain a Lulu OAuth token using the client-credentials grant, from a client key and client secret supplied by environment variable or configuration file, never by command-line argument.

The caller SHALL be able to select the sandbox or production environment, and the report SHALL state which was used. Credentials SHALL never appear in reports, logs, or error messages.

#### Scenario: Token is obtained from the client credentials

- **WHEN** a valid client key and secret are configured and verification is requested
- **THEN** the tool obtains an access token and uses it as a bearer token for subsequent calls

#### Scenario: Sandbox is selected

- **WHEN** the caller selects the sandbox environment
- **THEN** requests go to the sandbox host, and the report names the sandbox environment so a result is not mistaken for a production verdict

#### Scenario: Missing credentials

- **WHEN** verification is requested with no credentials configured
- **THEN** the run fails with an error naming the expected environment variables, and the prepared PDFs already written are left in place

#### Scenario: Secrets are never emitted

- **WHEN** an authentication request fails
- **THEN** the error message and the JSON report contain neither the client secret nor the access token

### Requirement: Cover dimension verification

When verification is enabled, the tool SHALL call the `cover-dimensions` endpoint with the product and final interior page count, and SHALL compare Lulu's answer against its own computed canvas.

A disagreement beyond 1 pt in either dimension SHALL be a blocking finding, since it means the local formula or the catalog is wrong for that product. For hardcover products, Lulu's answer SHALL be treated as authoritative and used in place of any local estimate.

#### Scenario: Local and remote geometry agree

- **WHEN** verification runs for a 6 × 9 in perfect-bound product with 210 pages and Lulu returns 920 × 666 pt
- **THEN** the report records agreement with the locally computed canvas

#### Scenario: Disagreement is blocking

- **WHEN** Lulu returns a canvas differing from the local computation by more than 1 pt
- **THEN** a blocking finding names both values, the product, and the page count, and the run's exit status reflects a blocking finding

#### Scenario: Hardcover dimensions come from the API

- **WHEN** verification runs for a case wrap product whose geometry is absent from the local template table
- **THEN** Lulu's returned dimensions are used to build the cover, and the report records that the geometry came from the API

### Requirement: File validation against Lulu

When verification is enabled and a publicly reachable URL for each prepared file is supplied, the tool SHALL submit the interior to `validate-interior` and the cover to `validate-cover`, poll for the terminal status, and fold Lulu's verdict into the report.

The tool SHALL recognise Lulu's statuses — `NULL`, `VALIDATING`, `VALIDATED`, `NORMALIZING`, `NORMALIZED`, and `ERROR` — and SHALL treat `VALIDATED` and `NORMALIZED` as success and `ERROR` as a blocking finding carrying Lulu's own error list verbatim.

#### Scenario: Interior validates successfully

- **WHEN** the interior is submitted with its `pod_package_id` and Lulu reaches `NORMALIZED`
- **THEN** the report records the terminal status, the normalized page count Lulu reports, and a passing verdict

#### Scenario: Lulu reports errors

- **WHEN** Lulu reaches `ERROR` for the interior
- **THEN** each entry in Lulu's `errors` field becomes a blocking finding, reproduced verbatim and attributed to Lulu rather than to local analysis

#### Scenario: No public URL available

- **WHEN** verification is requested but no reachable URL is supplied for a file
- **THEN** the tool skips file validation for that file, states clearly that Lulu requires a publicly downloadable URL, and still performs cover dimension verification

#### Scenario: Polling is bounded

- **WHEN** validation has not reached a terminal status within the configured timeout
- **THEN** the tool stops polling, reports the last observed status and the validation job identifier so the caller can query it later, and does not report success

#### Scenario: Local checks anticipate Lulu's rejections

- **WHEN** Lulu returns an error that local preflight also detects — mismatched page sizes, unembedded fonts, fewer than two pages, or a page size that does not match the `pod_package_id`
- **THEN** the report links Lulu's error to the corresponding local finding code, so the overlap is visible rather than duplicated

### Requirement: Transport robustness

API calls SHALL apply a connection and request timeout, retry only idempotent requests and only on transport errors or HTTP 5xx with bounded exponential backoff, and never retry a 4xx.

#### Scenario: Transient failure is retried

- **WHEN** a poll request fails with HTTP 503
- **THEN** the tool retries with backoff up to the configured attempt limit before reporting failure

#### Scenario: Client error is not retried

- **WHEN** a request fails with HTTP 400 because the `pod_package_id` is rejected
- **THEN** the tool reports the error immediately, including Lulu's response body, without retrying

#### Scenario: Network failure does not lose work

- **WHEN** the network is unreachable during verification
- **THEN** the prepared interior and cover files remain on disk, and the report states that verification could not be completed

