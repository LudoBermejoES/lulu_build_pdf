//! Opt-in verification against Lulu's Print API: OAuth client-credentials
//! auth, `cover-dimensions` cross-checking, and `validate-interior`/
//! `validate-cover` submission and polling.
//!
//! Entirely behind the `lulu-api` Cargo feature (this whole module is
//! `#[cfg(feature = "lulu-api")]`) — a default build has no HTTP client and
//! makes no network call under any invocation. Credentials come from the
//! environment (`LULU_CLIENT_KEY` / `LULU_CLIENT_SECRET`), never argv, and
//! never appear in errors or reports.

use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Environment {
    Sandbox,
    Production,
    /// Any other base URL (no trailing slash) — for pointing at a mock
    /// server in tests; never selected by production code.
    Custom(String),
}

impl Environment {
    fn base_url(&self) -> &str {
        match self {
            Environment::Sandbox => "https://api.sandbox.lulu.com",
            Environment::Production => "https://api.lulu.com",
            Environment::Custom(url) => url,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Environment::Sandbox => "sandbox",
            Environment::Production => "production",
            Environment::Custom(_) => "custom",
        }
    }
}

/// Client key and secret for the OAuth client-credentials grant. Its `Debug`
/// impl redacts both fields — never derive `Debug` on the raw strings
/// directly, or a stray `{:?}` in a log line leaks a credential.
pub struct Credentials {
    client_key: String,
    client_secret: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("client_key", &"***")
            .field("client_secret", &"***")
            .finish()
    }
}

impl Credentials {
    pub fn new(client_key: impl Into<String>, client_secret: impl Into<String>) -> Self {
        Credentials {
            client_key: client_key.into(),
            client_secret: client_secret.into(),
        }
    }

    /// Reads `LULU_CLIENT_KEY` and `LULU_CLIENT_SECRET` from the process
    /// environment. Naming these in an error is fine — the values never are.
    pub fn from_env() -> Result<Self, LuluApiError> {
        let client_key =
            std::env::var("LULU_CLIENT_KEY").map_err(|_| LuluApiError::MissingCredentials)?;
        let client_secret =
            std::env::var("LULU_CLIENT_SECRET").map_err(|_| LuluApiError::MissingCredentials)?;
        Ok(Credentials::new(client_key, client_secret))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LuluApiError {
    #[error("Lulu API credentials not found: set LULU_CLIENT_KEY and LULU_CLIENT_SECRET")]
    MissingCredentials,
    #[error("request to Lulu's API failed: {0}")]
    Transport(String),
    #[error("Lulu API returned HTTP {status}: {body}")]
    Api { status: u16, body: String },
    #[error(
        "polling timed out after {elapsed:?}; job {job_id} last observed status: {last_status}"
    )]
    Timeout {
        elapsed: Duration,
        last_status: String,
        job_id: i64,
    },
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

/// A bearer token obtained from the client-credentials grant. Its `Debug`
/// impl redacts the token itself.
pub struct AccessToken(String);

impl std::fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AccessToken(***)")
    }
}

impl AccessToken {
    fn as_str(&self) -> &str {
        &self.0
    }
}

const MAX_RETRY_ATTEMPTS: u32 = 3;

/// Retries `f` with bounded exponential backoff (200ms, 400ms, 800ms, ...)
/// on a transport error or HTTP 5xx. Never retries a 4xx — those are
/// treated as final on the first attempt. Only ever used for idempotent
/// requests (token fetch, `cover-dimensions`, and polling GETs) — never for
/// the resource-creating `validate-interior`/`validate-cover` POST, which
/// could otherwise create duplicate validation jobs on retry.
async fn send_with_retry<F, Fut>(mut f: F) -> Result<reqwest::Response, LuluApiError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<reqwest::Response, reqwest::Error>>,
{
    let mut attempt = 0;
    loop {
        attempt += 1;
        match f().await {
            Ok(resp) if resp.status().is_server_error() && attempt < MAX_RETRY_ATTEMPTS => {
                tokio::time::sleep(Duration::from_millis(200 * 2u64.pow(attempt - 1))).await;
            }
            Ok(resp) => return Ok(resp),
            Err(e) if attempt < MAX_RETRY_ATTEMPTS => {
                tokio::time::sleep(Duration::from_millis(200 * 2u64.pow(attempt - 1))).await;
                let _ = e;
            }
            Err(e) => return Err(LuluApiError::Transport(e.to_string())),
        }
    }
}

async fn body_or_api_error(resp: reqwest::Response) -> Result<String, LuluApiError> {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if status.is_success() {
        Ok(body)
    } else {
        Err(LuluApiError::Api {
            status: status.as_u16(),
            body,
        })
    }
}

/// Obtains an OAuth access token via the client-credentials grant.
pub async fn get_access_token(
    client: &reqwest::Client,
    env: Environment,
    creds: &Credentials,
) -> Result<AccessToken, LuluApiError> {
    let url = format!(
        "{}/auth/realms/glasstree/protocol/openid-connect/token",
        env.base_url()
    );
    let resp = send_with_retry(|| {
        client
            .post(&url)
            .basic_auth(&creds.client_key, Some(&creds.client_secret))
            .form(&[("grant_type", "client_credentials")])
            .send()
    })
    .await?;
    let body = body_or_api_error(resp).await?;
    let parsed: TokenResponse = serde_json::from_str(&body).map_err(|e| LuluApiError::Api {
        status: 200,
        body: format!("could not parse token response: {e}"),
    })?;
    Ok(AccessToken(parsed.access_token))
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CoverDimensions {
    pub width: String,
    pub height: String,
    pub unit: String,
}

#[derive(Deserialize)]
struct CoverDimensionsRaw {
    width: String,
    height: String,
    unit: String,
}

/// Calls `cover-dimensions`: Lulu's own authoritative cover width/height for
/// a product and interior page count. `unit` is `"pt"`, `"mm"`, or `"in"`;
/// Lulu defaults to points when omitted, so this always sends it explicitly.
pub async fn cover_dimensions(
    client: &reqwest::Client,
    env: Environment,
    token: &AccessToken,
    pod_package_id: &str,
    interior_page_count: u32,
    unit: &str,
) -> Result<CoverDimensions, LuluApiError> {
    let url = format!("{}/cover-dimensions/", env.base_url());
    let payload = serde_json::json!({
        "pod_package_id": pod_package_id,
        "interior_page_count": interior_page_count,
        "unit": unit,
    });
    let resp = send_with_retry(|| {
        client
            .post(&url)
            .bearer_auth(token.as_str())
            .json(&payload)
            .send()
    })
    .await?;
    let body = body_or_api_error(resp).await?;
    let raw: CoverDimensionsRaw = serde_json::from_str(&body).map_err(|e| LuluApiError::Api {
        status: 201,
        body: format!("could not parse cover-dimensions response: {e}"),
    })?;
    Ok(CoverDimensions {
        width: raw.width,
        height: raw.height,
        unit: raw.unit,
    })
}

/// A `validate-interior` or `validate-cover` record, as returned by either
/// the creating POST or a polling GET. `status` is kept as the raw string
/// Lulu returns — its documented lifecycle is `NULL`, `VALIDATING`,
/// `VALIDATED`, `NORMALIZING`, `NORMALIZED`, `ERROR`, but the two endpoints'
/// published schemas don't agree on which subset each uses, so this stays
/// forward-compatible with an unrecognised value rather than failing to parse.
#[derive(Debug, Clone, Deserialize)]
pub struct ValidationRecord {
    pub id: i64,
    pub source_url: String,
    #[serde(default)]
    pub page_count: Option<String>,
    #[serde(default)]
    pub errors: Vec<String>,
    pub status: String,
}

impl ValidationRecord {
    pub fn is_terminal_success(&self) -> bool {
        matches!(self.status.as_str(), "VALIDATED" | "NORMALIZED")
    }

    pub fn is_error(&self) -> bool {
        self.status == "ERROR"
    }

    pub fn is_terminal(&self) -> bool {
        self.is_terminal_success() || self.is_error()
    }
}

fn parse_validation_record(body: &str) -> Result<ValidationRecord, LuluApiError> {
    serde_json::from_str(body).map_err(|e| LuluApiError::Api {
        status: 201,
        body: format!("could not parse validation response: {e}"),
    })
}

/// Submits an interior file for validation. Not retried on failure — a
/// resource-creating POST retried blindly could create duplicate validation
/// jobs; a transient failure here should be retried by the caller as a new,
/// deliberate call, not automatically.
pub async fn submit_validate_interior(
    client: &reqwest::Client,
    env: Environment,
    token: &AccessToken,
    source_url: &str,
    pod_package_id: Option<&str>,
) -> Result<ValidationRecord, LuluApiError> {
    let url = format!("{}/validate-interior/", env.base_url());
    let mut payload = serde_json::json!({ "source_url": source_url });
    if let Some(sku) = pod_package_id {
        payload["pod_package_id"] = serde_json::Value::String(sku.to_string());
    }
    let resp = client
        .post(&url)
        .bearer_auth(token.as_str())
        .json(&payload)
        .send()
        .await
        .map_err(|e| LuluApiError::Transport(e.to_string()))?;
    let body = body_or_api_error(resp).await?;
    parse_validation_record(&body)
}

/// Submits a cover file for validation, alongside the product and the
/// interior page count it must match. Not retried, for the same reason as
/// [`submit_validate_interior`].
pub async fn submit_validate_cover(
    client: &reqwest::Client,
    env: Environment,
    token: &AccessToken,
    source_url: &str,
    pod_package_id: &str,
    interior_page_count: u32,
) -> Result<ValidationRecord, LuluApiError> {
    let url = format!("{}/validate-cover/", env.base_url());
    let payload = serde_json::json!({
        "source_url": source_url,
        "pod_package_id": pod_package_id,
        "interior_page_count": interior_page_count,
    });
    let resp = client
        .post(&url)
        .bearer_auth(token.as_str())
        .json(&payload)
        .send()
        .await
        .map_err(|e| LuluApiError::Transport(e.to_string()))?;
    let body = body_or_api_error(resp).await?;
    parse_validation_record(&body)
}

async fn poll_validation(
    client: &reqwest::Client,
    url: &str,
    token: &AccessToken,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<ValidationRecord, LuluApiError> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let resp = send_with_retry(|| client.get(url).bearer_auth(token.as_str()).send()).await?;
        let body = body_or_api_error(resp).await?;
        let record = parse_validation_record(&body)?;
        if record.is_terminal() {
            return Ok(record);
        }
        if std::time::Instant::now() >= deadline {
            return Err(LuluApiError::Timeout {
                elapsed: timeout,
                job_id: record.id,
                last_status: record.status,
            });
        }
        tokio::time::sleep(poll_interval).await;
    }
}

/// Polls a `validate-interior` record by id until it reaches a terminal
/// status (`VALIDATED`, `NORMALIZED`, or `ERROR`) or `timeout` elapses.
pub async fn poll_validate_interior(
    client: &reqwest::Client,
    env: Environment,
    token: &AccessToken,
    id: i64,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<ValidationRecord, LuluApiError> {
    let url = format!("{}/validate-interior/{id}/", env.base_url());
    poll_validation(client, &url, token, timeout, poll_interval).await
}

/// Polls a `validate-cover` record by id until it reaches a terminal status
/// (`NORMALIZED` or `ERROR`) or `timeout` elapses.
pub async fn poll_validate_cover(
    client: &reqwest::Client,
    env: Environment,
    token: &AccessToken,
    id: i64,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<ValidationRecord, LuluApiError> {
    let url = format!("{}/validate-cover/{id}/", env.base_url());
    poll_validation(client, &url, token, timeout, poll_interval).await
}

/// Compares Lulu's own `cover-dimensions` answer against a locally computed
/// canvas, in points. A disagreement beyond 1 pt in either dimension is
/// blocking — it means the local formula or catalog is wrong for this
/// product — since Lulu's answer is authoritative.
pub async fn verify_cover_canvas(
    client: &reqwest::Client,
    env: Environment,
    token: &AccessToken,
    pod_package_id: &str,
    interior_page_count: u32,
    locally_computed: crate::units::Size,
) -> Result<Vec<crate::report::Finding>, LuluApiError> {
    let remote = cover_dimensions(
        client,
        env,
        token,
        pod_package_id,
        interior_page_count,
        "pt",
    )
    .await?;
    let parse = |s: &str| -> f64 { s.trim().parse().unwrap_or(f64::NAN) };
    let (remote_w, remote_h) = (parse(&remote.width), parse(&remote.height));
    let (local_w, local_h) = (
        locally_computed.width.as_points(),
        locally_computed.height.as_points(),
    );

    let mut findings = Vec::new();
    if !remote_w.is_finite()
        || !remote_h.is_finite()
        || (remote_w - local_w).abs() > 1.0
        || (remote_h - local_h).abs() > 1.0
    {
        findings.push(
            crate::report::Finding::new(
                "lulu-api.cover-dimensions-mismatch",
                crate::report::Severity::Blocking,
                format!(
                    "Lulu's cover-dimensions endpoint returned {} x {} {} for {pod_package_id} at {interior_page_count} pages, but the local computation gives {local_w:.1} x {local_h:.1} pt",
                    remote.width, remote.height, remote.unit
                ),
            )
            .with_observed(format!("{local_w:.1} x {local_h:.1} pt (local)"))
            .with_expected(format!("{} x {} {} (Lulu)", remote.width, remote.height, remote.unit))
            .fixable(false),
        );
    }
    Ok(findings)
}

/// The full cover verification the CLI's Lulu-API-verification stage runs:
/// always the free, no-URL `cover-dimensions` cross-check
/// ([`verify_cover_canvas`]), and — only when a publicly reachable
/// `cover_source_url` is supplied — the `validate-cover` file check as well,
/// polled to a terminal status. Both validation endpoints need a URL Lulu
/// can fetch the file from, which this tool cannot itself provide (it has no
/// hosting of its own); when the caller has none, file validation is skipped
/// with an explanatory info finding rather than silently omitted or forced
/// on the caller to route around.
#[allow(clippy::too_many_arguments)]
pub async fn verify_cover(
    client: &reqwest::Client,
    env: Environment,
    token: &AccessToken,
    pod_package_id: &str,
    interior_page_count: u32,
    locally_computed_canvas: crate::units::Size,
    cover_source_url: Option<&str>,
    poll_timeout: Duration,
    poll_interval: Duration,
) -> Result<Vec<crate::report::Finding>, LuluApiError> {
    let mut findings = verify_cover_canvas(
        client,
        env.clone(),
        token,
        pod_package_id,
        interior_page_count,
        locally_computed_canvas,
    )
    .await?;

    match cover_source_url {
        Some(url) => {
            let record = submit_validate_cover(
                client,
                env.clone(),
                token,
                url,
                pod_package_id,
                interior_page_count,
            )
            .await?;
            let record =
                poll_validate_cover(client, env, token, record.id, poll_timeout, poll_interval)
                    .await?;
            findings.extend(validation_record_to_findings(&record));
        }
        None => {
            findings.push(crate::report::Finding::new(
                "lulu-api.file-validation-skipped",
                crate::report::Severity::Info,
                "no publicly reachable cover URL was supplied; skipped Lulu's validate-cover file check (the cover-dimensions cross-check above still ran, since it needs no URL)".to_string(),
            ));
        }
    }
    Ok(findings)
}

#[derive(Debug, thiserror::Error)]
pub enum HardcoverApiError {
    #[error(transparent)]
    Api(#[from] LuluApiError),
    #[error("Lulu's cover-dimensions endpoint returned a non-numeric {0}")]
    NonNumericDimension(&'static str),
    #[error("this product's spine width could not be computed: {0}")]
    Spine(#[from] crate::geometry::SpineError),
}

/// Builds hardcover (case wrap / linen wrap) cover geometry from Lulu's live
/// `cover-dimensions` endpoint, for a product and page count absent from the
/// local template table ([`crate::cover::cover_geometry`] refuses those
/// rather than guessing). The canvas comes from Lulu; fold positions are
/// derived by centring the published hardcover spine width formula within
/// it, and the hinge is Lulu's documented 0.25 in constant.
pub async fn hardcover_geometry_via_api(
    client: &reqwest::Client,
    env: Environment,
    token: &AccessToken,
    entry: &crate::catalog::CatalogEntry,
    page_count: u32,
) -> Result<crate::cover::CoverGeometry, HardcoverApiError> {
    let dims = cover_dimensions(client, env, token, &entry.sku, page_count, "pt").await?;
    let width: f64 = dims
        .width
        .trim()
        .parse()
        .map_err(|_| HardcoverApiError::NonNumericDimension("width"))?;
    let height: f64 = dims
        .height
        .trim()
        .parse()
        .map_err(|_| HardcoverApiError::NonNumericDimension("height"))?;
    let canvas = crate::units::Size::new(
        crate::units::Length::from_points(width),
        crate::units::Length::from_points(height),
    );

    let spine = crate::geometry::spine_width(entry.binding, page_count, entry.interior_ppi)?;
    let spine_width = match spine {
        crate::geometry::SpineWidth::Hardcover(w) => w,
        _ => crate::units::Length::ZERO,
    };
    let fold1 = (canvas.width - spine_width) / 2.0;
    let fold2 = fold1 + spine_width;
    let hinge = crate::units::Length::from_inches(0.25);
    let zero = crate::units::Length::ZERO;

    Ok(crate::cover::CoverGeometry {
        canvas,
        back_panel: crate::units::Rect {
            x0: zero,
            y0: zero,
            x1: fold1,
            y1: canvas.height,
        },
        spine: crate::units::Rect {
            x0: fold1,
            y0: zero,
            x1: fold2,
            y1: canvas.height,
        },
        front_panel: crate::units::Rect {
            x0: fold2,
            y0: zero,
            x1: canvas.width,
            y1: canvas.height,
        },
        fold_positions: (fold1, fold2),
        safety_margin: crate::geometry::cover_safety_margin(entry.binding),
        hinge_zones: Some((
            crate::units::Rect {
                x0: fold1,
                y0: zero,
                x1: fold1 + hinge,
                y1: canvas.height,
            },
            crate::units::Rect {
                x0: fold2 - hinge,
                y0: zero,
                x1: fold2,
                y1: canvas.height,
            },
        )),
        page_count,
    })
}

/// Maps a Lulu-published `errors` string (from `validate-interior` /
/// `validate-cover`) to the corresponding local finding code, for the cases
/// this crate's own preflight checks are designed to predict — page size
/// mismatches, unembedded fonts, and too few pages. `None` for an error
/// string this crate has no local equivalent check for.
fn link_known_lulu_error(message: &str) -> Option<&'static str> {
    let lower = message.to_lowercase();
    if lower.contains("different sizes of pages") {
        Some(crate::report::codes::GEOMETRY_MIXED_PAGE_SIZES)
    } else if lower.contains("font") && lower.contains("embed") {
        Some(crate::report::codes::FONTS_NOT_EMBEDDED)
    } else if lower.contains("not enough pages") || lower.contains("at least 2 pages") {
        Some(crate::report::codes::PAGE_COUNT_BELOW_MINIMUM)
    } else if lower.contains("page size")
        && (lower.contains("pod package") || lower.contains("sku"))
    {
        Some(crate::report::codes::GEOMETRY_PAGE_SIZE_MISMATCH)
    } else {
        None
    }
}

/// Turns a terminal [`ValidationRecord`] into findings: `ERROR` becomes one
/// blocking finding per entry in Lulu's `errors` list, reproduced verbatim
/// and attributed to Lulu, linked to the corresponding local finding code
/// where [`link_known_lulu_error`] recognises it. A successful record
/// (`VALIDATED`/`NORMALIZED`) produces no findings.
pub fn validation_record_to_findings(record: &ValidationRecord) -> Vec<crate::report::Finding> {
    if !record.is_error() {
        return Vec::new();
    }
    record
        .errors
        .iter()
        .map(|message| {
            let mut finding = crate::report::Finding::new(
                link_known_lulu_error(message).unwrap_or("lulu-api.validation-error"),
                crate::report::Severity::Blocking,
                format!("Lulu: {message}"),
            )
            .fixable(false);
            if let Some(local_code) = link_known_lulu_error(message) {
                finding = finding.with_observed(format!(
                    "also predicted locally by finding code '{local_code}'"
                ));
            }
            finding
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn credentials_debug_never_prints_the_secret() {
        let creds = Credentials::new("my-client-key", "super-secret-value");
        let debug_output = format!("{creds:?}");
        assert!(!debug_output.contains("my-client-key"));
        assert!(!debug_output.contains("super-secret-value"));
    }

    #[test]
    fn access_token_debug_never_prints_the_token() {
        let token = AccessToken("eyJhbGciOi.super.secret".to_string());
        let debug_output = format!("{token:?}");
        assert!(!debug_output.contains("eyJhbGciOi"));
    }

    #[test]
    fn missing_credentials_error_names_the_env_vars_not_a_value() {
        // SAFETY: test-only env manipulation; no other test in this module reads these vars concurrently.
        unsafe {
            std::env::remove_var("LULU_CLIENT_KEY");
            std::env::remove_var("LULU_CLIENT_SECRET");
        }
        let err = Credentials::from_env().unwrap_err();
        let message = err.to_string();
        assert!(message.contains("LULU_CLIENT_KEY"));
        assert!(message.contains("LULU_CLIENT_SECRET"));
    }

    #[tokio::test]
    async fn token_is_obtained_via_client_credentials() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/auth/realms/glasstree/protocol/openid-connect/token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({ "access_token": "abc123", "expires_in": 3600 }),
                ),
            )
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let creds = Credentials::new("key", "secret");
        let url = format!(
            "{}/auth/realms/glasstree/protocol/openid-connect/token",
            server.uri()
        );
        let resp = client
            .post(&url)
            .basic_auth(&creds.client_key, Some(&creds.client_secret))
            .form(&[("grant_type", "client_credentials")])
            .send()
            .await
            .unwrap();
        let body = resp.text().await.unwrap();
        let parsed: TokenResponse = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed.access_token, "abc123");
    }

    #[tokio::test]
    async fn cover_dimensions_parses_lulus_worked_example() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/cover-dimensions/"))
            .respond_with(ResponseTemplate::new(201).set_body_json(
                serde_json::json!({ "width": "920.000", "height": "666.000", "unit": "pt" }),
            ))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let token = AccessToken("test-token".to_string());
        let url = format!("{}/cover-dimensions/", server.uri());
        let payload = serde_json::json!({ "pod_package_id": "0600X0900.BW.STD.PB.060UW444.MXX", "interior_page_count": 210, "unit": "pt" });
        let resp = client
            .post(&url)
            .bearer_auth(token.as_str())
            .json(&payload)
            .send()
            .await
            .unwrap();
        let body = resp.text().await.unwrap();
        let raw: CoverDimensionsRaw = serde_json::from_str(&body).unwrap();
        assert_eq!(raw.width, "920.000");
        assert_eq!(raw.height, "666.000");
    }

    #[tokio::test]
    async fn retries_on_503_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe/"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/probe/"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let url = format!("{}/probe/", server.uri());
        let resp = send_with_retry(|| client.get(&url).send()).await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn does_not_retry_a_4xx() {
        let server = MockServer::start().await;
        let mut call_count_guard = 0;
        Mock::given(method("GET"))
            .and(path("/bad/"))
            .respond_with(ResponseTemplate::new(400).set_body_string("bad request"))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let url = format!("{}/bad/", server.uri());
        let resp = send_with_retry(|| {
            call_count_guard += 1;
            client.get(&url).send()
        })
        .await
        .unwrap();
        assert_eq!(resp.status(), 400);
        assert_eq!(call_count_guard, 1, "a 4xx must not be retried");
    }

    #[tokio::test]
    async fn validate_interior_error_status_carries_lulus_errors_verbatim() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/validate-interior/"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 1, "source_url": "https://example.com/x.pdf", "page_count": "32",
                "errors": ["invalid PDF file", "not enough pages - at least 2 pages are required"],
                "status": "ERROR",
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let token = AccessToken("t".to_string());
        let url = format!("{}/validate-interior/", server.uri());
        let payload = serde_json::json!({ "source_url": "https://example.com/x.pdf" });
        let resp = client
            .post(&url)
            .bearer_auth(token.as_str())
            .json(&payload)
            .send()
            .await
            .unwrap();
        let body = resp.text().await.unwrap();
        let record = parse_validation_record(&body).unwrap();

        assert!(record.is_error());
        assert!(!record.is_terminal_success());
        assert_eq!(
            record.errors,
            vec![
                "invalid PDF file".to_string(),
                "not enough pages - at least 2 pages are required".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn polling_reaches_normalized_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/validate-interior/1/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "id": 1, "source_url": "u", "page_count": "210", "errors": [], "status": "NORMALIZING" })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/validate-interior/1/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "id": 1, "source_url": "u", "page_count": "210", "errors": [], "status": "NORMALIZED" })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let token = AccessToken("t".to_string());
        let url = format!("{}/validate-interior/1/", server.uri());
        let record = poll_validation(
            &client,
            &url,
            &token,
            Duration::from_secs(5),
            Duration::from_millis(10),
        )
        .await
        .unwrap();
        assert!(record.is_terminal_success());
        assert_eq!(record.status, "NORMALIZED");
    }

    #[tokio::test]
    async fn polling_times_out_reporting_the_last_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/validate-interior/2/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "id": 2, "source_url": "u", "page_count": null, "errors": [], "status": "VALIDATING" })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let token = AccessToken("t".to_string());
        let url = format!("{}/validate-interior/2/", server.uri());
        let err = poll_validation(
            &client,
            &url,
            &token,
            Duration::from_millis(30),
            Duration::from_millis(10),
        )
        .await
        .unwrap_err();
        match err {
            LuluApiError::Timeout { last_status, .. } => assert_eq!(last_status, "VALIDATING"),
            other => panic!("expected Timeout, got {other:?}"),
        }
    }

    #[test]
    fn environment_labels_and_urls_are_distinct() {
        assert_ne!(
            Environment::Sandbox.base_url(),
            Environment::Production.base_url()
        );
        assert_eq!(Environment::Sandbox.label(), "sandbox");
        assert_eq!(Environment::Production.label(), "production");
    }

    // --- error-to-finding linking ---

    #[test]
    fn known_lulu_errors_link_to_local_finding_codes() {
        assert_eq!(
            link_known_lulu_error("different sizes of pages detected"),
            Some(crate::report::codes::GEOMETRY_MIXED_PAGE_SIZES)
        );
        assert_eq!(
            link_known_lulu_error("fonts not embedded"),
            Some(crate::report::codes::FONTS_NOT_EMBEDDED)
        );
        assert_eq!(
            link_known_lulu_error("not enough pages - at least 2 pages are required"),
            Some(crate::report::codes::PAGE_COUNT_BELOW_MINIMUM)
        );
        assert_eq!(
            link_known_lulu_error("page size does not match the pod package id"),
            Some(crate::report::codes::GEOMETRY_PAGE_SIZE_MISMATCH)
        );
        assert_eq!(link_known_lulu_error("corrupted images"), None);
    }

    #[test]
    fn validation_record_to_findings_is_empty_on_success() {
        let record = ValidationRecord {
            id: 1,
            source_url: "u".into(),
            page_count: Some("210".into()),
            errors: vec![],
            status: "NORMALIZED".into(),
        };
        assert!(validation_record_to_findings(&record).is_empty());
    }

    #[test]
    fn validation_record_to_findings_reproduces_lulus_errors_verbatim_and_links_known_ones() {
        let record = ValidationRecord {
            id: 1,
            source_url: "u".into(),
            page_count: Some("210".into()),
            errors: vec![
                "fonts not embedded".to_string(),
                "corrupted images".to_string(),
            ],
            status: "ERROR".into(),
        };
        let findings = validation_record_to_findings(&record);
        assert_eq!(findings.len(), 2);
        assert!(findings[0].message.contains("fonts not embedded"));
        assert_eq!(findings[0].code, crate::report::codes::FONTS_NOT_EMBEDDED);
        assert!(findings[1].message.contains("corrupted images"));
        assert_eq!(findings[1].code, "lulu-api.validation-error");
        assert!(findings
            .iter()
            .all(|f| f.severity == crate::report::Severity::Blocking));
    }

    // --- cover-dimensions verification ---

    #[tokio::test]
    async fn matching_dimensions_produce_no_finding() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/cover-dimensions/"))
            .respond_with(ResponseTemplate::new(201).set_body_json(
                serde_json::json!({ "width": "920.000", "height": "666.000", "unit": "pt" }),
            ))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let env = Environment::Custom(server.uri());
        let token = AccessToken("t".to_string());
        let local = crate::units::Size::new(
            crate::units::Length::from_points(920.0),
            crate::units::Length::from_points(666.0),
        );

        let findings = verify_cover_canvas(
            &client,
            env,
            &token,
            "0600X0900.BW.STD.PB.060UW444.MXX",
            212,
            local,
        )
        .await
        .unwrap();
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[tokio::test]
    async fn disagreement_beyond_one_point_is_blocking() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/cover-dimensions/"))
            .respond_with(ResponseTemplate::new(201).set_body_json(
                serde_json::json!({ "width": "925.000", "height": "666.000", "unit": "pt" }),
            ))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let env = Environment::Custom(server.uri());
        let token = AccessToken("t".to_string());
        let local = crate::units::Size::new(
            crate::units::Length::from_points(920.0),
            crate::units::Length::from_points(666.0),
        );

        let findings = verify_cover_canvas(
            &client,
            env,
            &token,
            "0600X0900.BW.STD.PB.060UW444.MXX",
            212,
            local,
        )
        .await
        .unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, "lulu-api.cover-dimensions-mismatch");
        assert_eq!(findings[0].severity, crate::report::Severity::Blocking);
        assert!(findings[0].message.contains("925"));
    }

    // --- verify_cover (dimension check + optional file validation) ---

    #[tokio::test]
    async fn verify_cover_without_a_url_skips_file_validation_but_still_checks_dimensions() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/cover-dimensions/"))
            .respond_with(ResponseTemplate::new(201).set_body_json(
                serde_json::json!({ "width": "920.000", "height": "666.000", "unit": "pt" }),
            ))
            .mount(&server)
            .await;
        // No /validate-cover/ mock is registered at all: if verify_cover ever
        // called it without a URL, this test would fail on the unmocked request.

        let client = reqwest::Client::new();
        let env = Environment::Custom(server.uri());
        let token = AccessToken("t".to_string());
        let local = crate::units::Size::new(
            crate::units::Length::from_points(920.0),
            crate::units::Length::from_points(666.0),
        );

        let findings = verify_cover(
            &client,
            env,
            &token,
            "0600X0900.BW.STD.PB.060UW444.MXX",
            212,
            local,
            None,
            Duration::from_secs(5),
            Duration::from_millis(1),
        )
        .await
        .unwrap();

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, "lulu-api.file-validation-skipped");
        assert_eq!(findings[0].severity, crate::report::Severity::Info);
    }

    #[tokio::test]
    async fn verify_cover_with_a_url_runs_file_validation_to_a_terminal_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/cover-dimensions/"))
            .respond_with(ResponseTemplate::new(201).set_body_json(
                serde_json::json!({ "width": "920.000", "height": "666.000", "unit": "pt" }),
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/validate-cover/"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 7, "source_url": "https://example.com/cover.pdf", "status": "NORMALIZED"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/validate-cover/7/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 7, "source_url": "https://example.com/cover.pdf", "status": "NORMALIZED"
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let env = Environment::Custom(server.uri());
        let token = AccessToken("t".to_string());
        let local = crate::units::Size::new(
            crate::units::Length::from_points(920.0),
            crate::units::Length::from_points(666.0),
        );

        let findings = verify_cover(
            &client,
            env,
            &token,
            "0600X0900.BW.STD.PB.060UW444.MXX",
            212,
            local,
            Some("https://example.com/cover.pdf"),
            Duration::from_secs(5),
            Duration::from_millis(1),
        )
        .await
        .unwrap();

        assert!(
            !findings
                .iter()
                .any(|f| f.code == "lulu-api.file-validation-skipped"),
            "{findings:?}"
        );
    }

    // --- hardcover geometry via API ---

    #[tokio::test]
    async fn hardcover_geometry_via_api_builds_geometry_from_lulus_canvas() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/cover-dimensions/"))
            .respond_with(ResponseTemplate::new(201).set_body_json(
                serde_json::json!({ "width": "936.0", "height": "684.0", "unit": "pt" }),
            ))
            .mount(&server)
            .await;

        let entry = crate::catalog::search(|e| e.binding == crate::catalog::Binding::CaseWrap)
            .first()
            .copied()
            .expect("a case wrap product");
        let client = reqwest::Client::new();
        let env = Environment::Custom(server.uri());
        let token = AccessToken("t".to_string());

        let geo = hardcover_geometry_via_api(&client, env, &token, entry, 210)
            .await
            .unwrap();
        assert_eq!(geo.canvas.width.as_points(), 936.0);
        assert_eq!(geo.canvas.height.as_points(), 684.0);
        assert_eq!(geo.page_count, 210);
        let (left_hinge, right_hinge) = geo
            .hinge_zones
            .expect("hardcover geometry must report hinge zones");
        assert!((left_hinge.width().as_inches() - 0.25).abs() < 1e-9);
        assert!((right_hinge.width().as_inches() - 0.25).abs() < 1e-9);
        // Spine centred: back panel + spine + front panel = full canvas.
        let spine_width = geo.fold_positions.1 - geo.fold_positions.0;
        assert!(
            (geo.back_panel.width() + spine_width + geo.front_panel.width()).as_points()
                - geo.canvas.width.as_points()
                < 1e-6
        );
    }

    #[tokio::test]
    async fn hardcover_geometry_via_api_rejects_non_numeric_dimensions() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/cover-dimensions/"))
            .respond_with(ResponseTemplate::new(201).set_body_json(
                serde_json::json!({ "width": "not-a-number", "height": "684.0", "unit": "pt" }),
            ))
            .mount(&server)
            .await;

        let entry = crate::catalog::search(|e| e.binding == crate::catalog::Binding::CaseWrap)
            .first()
            .copied()
            .expect("a case wrap product");
        let client = reqwest::Client::new();
        let env = Environment::Custom(server.uri());
        let token = AccessToken("t".to_string());

        let err = hardcover_geometry_via_api(&client, env, &token, entry, 210)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            HardcoverApiError::NonNumericDimension("width")
        ));
    }
}
