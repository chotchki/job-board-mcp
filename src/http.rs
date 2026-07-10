//! The shared HTTP layer every adapter fetches through. It exists to be a POLITE,
//! predictable client against boards that are not ours: a desktop-browser User-Agent
//! (several ATSes 403 a default client UA), requests serialized per host with a delay
//! between them, hard timeouts so a dead board can't hang a scan, and — deliberately —
//! no automatic retries, because a retry storm against someone else's careers page is
//! exactly the rudeness this project won't commit.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use reqwest::Url;
use tokio::sync::Mutex as AsyncMutex;

use crate::adapter::AdapterError;
use crate::config::BoardConfig;
use crate::model::{Ats, BoardId};
use crate::store::{RawCapture, Store};

/// The board context a fetch runs under — what the capture ledger keys a raw response by.
/// Built cheaply per fetch from a [`BoardConfig`]; threaded through so the HTTP layer can
/// attribute a captured body to its board without importing the whole config.
pub struct FetchCtx {
    pub board_id: BoardId,
    pub ats: Ats,
}

impl FetchCtx {
    pub fn from_board(board: &BoardConfig) -> Self {
        Self {
            board_id: board.id.clone(),
            ats: board.ats,
        }
    }
}

/// Where the HTTP layer writes raw responses when capture is enabled. Holds a shared
/// handle to the store, the retention window, and the INJECTED clock — so a capture
/// timestamp still flows from the one sanctioned [`crate::clock::now`], and the store
/// itself keeps reading no clock.
struct Capture {
    store: Arc<Store>,
    retain_days: u32,
    now: fn() -> DateTime<Utc>,
}

/// A recent desktop Chrome UA. Boards that 403 a bare client UA generally wave this
/// through, and it's honest about being a browser-shaped read.
pub const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
     AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// Tunables for [`HttpClient`]. `Default` is the production posture; tests dial the
/// timings down.
#[derive(Debug, Clone)]
pub struct HttpConfig {
    /// Minimum gap between two requests to the SAME host, measured from the end of one
    /// to the start of the next.
    pub politeness: Duration,
    pub connect_timeout: Duration,
    pub total_timeout: Duration,
    pub user_agent: String,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            politeness: Duration::from_secs(1),
            connect_timeout: Duration::from_secs(10),
            total_timeout: Duration::from_secs(30),
            user_agent: DEFAULT_USER_AGENT.to_owned(),
        }
    }
}

/// A politeness-enforcing HTTP client. Clone-free by design — build one and share it by
/// reference across adapters.
pub struct HttpClient {
    client: reqwest::Client,
    politeness: Duration,
    // Per-host gate holding the end-time of that host's last request. The async mutex
    // serializes requests to one host; the outer std mutex only guards the map lookup
    // and is never held across an await.
    hosts: Mutex<HashMap<String, Arc<AsyncMutex<Option<Instant>>>>>,
    // Raw-response capture sink, when enabled. None = capture off.
    capture: Option<Capture>,
}

impl HttpClient {
    pub fn new(config: HttpConfig) -> Result<Self, AdapterError> {
        let client = reqwest::Client::builder()
            .user_agent(config.user_agent)
            .connect_timeout(config.connect_timeout)
            .timeout(config.total_timeout)
            // No retry middleware, by intent — one request, one answer.
            .build()
            .map_err(|e| AdapterError::Transport(format!("building HTTP client: {e}")))?;
        Ok(Self {
            client,
            politeness: config.politeness,
            hosts: Mutex::new(HashMap::new()),
            capture: None,
        })
    }

    /// Enable raw-response capture: every SUCCESSFUL fetch records its body to `store`,
    /// stamped by `now`, with rows past `retain_days` purged on each write. The caller
    /// only calls this when the window is non-zero — a `0`-day window means "off" and is
    /// expressed by never wiring a sink, not by a zero here.
    pub fn with_capture(
        mut self,
        store: Arc<Store>,
        retain_days: u32,
        now: fn() -> DateTime<Utc>,
    ) -> Self {
        self.capture = Some(Capture {
            store,
            retain_days,
            now,
        });
        self
    }

    /// GET a URL, returning the body text. Enforces the per-host delay first, maps an
    /// HTTP non-success to [`AdapterError::BoardUnreachable`] and any network failure to
    /// [`AdapterError::Transport`] — so a caller never mistakes a dead fetch for a body.
    pub async fn get_text(&self, url: &str, ctx: &FetchCtx) -> Result<String, AdapterError> {
        self.send_gated(ctx, url, "GET", None, self.client.get(url))
            .await
    }

    /// POST a JSON body, returning the response text. Same politeness gate and error
    /// mapping as [`get_text`](Self::get_text) — Workday's search endpoint needs it.
    pub async fn post_json(
        &self,
        url: &str,
        body: &serde_json::Value,
        ctx: &FetchCtx,
    ) -> Result<String, AdapterError> {
        // Serialize the request body for the capture record — its content, not the exact
        // bytes reqwest sends, which is what a replayed sample needs anyway.
        let request_body = serde_json::to_string(body).ok();
        self.send_gated(
            ctx,
            url,
            "POST",
            request_body,
            self.client.post(url).json(body),
        )
        .await
    }

    /// Run a request behind the per-host politeness gate: wait out the remaining delay for
    /// this host, send, record the end-time (even on failure, so a failing host stays
    /// spaced out), then — on success and if enabled — capture the raw body.
    async fn send_gated(
        &self,
        ctx: &FetchCtx,
        url: &str,
        method: &str,
        request_body: Option<String>,
        request: reqwest::RequestBuilder,
    ) -> Result<String, AdapterError> {
        let host = Url::parse(url)
            .map_err(|e| AdapterError::Transport(format!("invalid url {url}: {e}")))?
            .host_str()
            .unwrap_or_default()
            .to_owned();

        let gate = {
            let mut map = self.hosts.lock().expect("host map mutex poisoned");
            map.entry(host)
                .or_insert_with(|| Arc::new(AsyncMutex::new(None)))
                .clone()
        };
        // Holding this across the request serializes same-host traffic.
        let mut last = gate.lock().await;
        if let Some(prev) = *last {
            let wait = self
                .politeness
                .saturating_sub(Instant::now().duration_since(prev));
            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
            }
        }

        let result = execute(url, request).await;
        *last = Some(Instant::now());
        // Release the per-host gate BEFORE the capture write — a local DB write must not
        // hold up the next same-host fetch's politeness accounting.
        drop(last);

        let (status, body) = result?;
        self.capture(ctx, url, method, request_body.as_deref(), status, &body)
            .await;
        Ok(body)
    }

    /// Best-effort raw capture: on a capture-write failure, warn and move on — the fetch
    /// already succeeded, and losing a sample must never turn a good fetch into an error.
    async fn capture(
        &self,
        ctx: &FetchCtx,
        url: &str,
        method: &str,
        request_body: Option<&str>,
        status: u16,
        body: &str,
    ) {
        let Some(capture) = &self.capture else {
            return;
        };
        let record = RawCapture {
            board_id: &ctx.board_id,
            ats: ctx.ats,
            url,
            method,
            request_body,
            status,
            body,
        };
        if let Err(e) = capture
            .store
            .record_capture(&record, (capture.now)(), capture.retain_days)
            .await
        {
            tracing::warn!(url, error = %e, "raw capture write failed — fetch unaffected");
        }
    }
}

async fn execute(
    url: &str,
    request: reqwest::RequestBuilder,
) -> Result<(u16, String), AdapterError> {
    let response = request.send().await.map_err(|e| {
        if e.is_timeout() {
            AdapterError::Transport(format!("timeout fetching {url}"))
        } else {
            AdapterError::Transport(format!("fetching {url}: {e}"))
        }
    })?;

    let status = response.status();
    if !status.is_success() {
        return Err(AdapterError::BoardUnreachable {
            status: status.as_u16(),
        });
    }
    let code = status.as_u16();
    let body = response
        .text()
        .await
        .map_err(|e| AdapterError::Transport(format!("reading body from {url}: {e}")))?;
    Ok((code, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fast_config() -> HttpConfig {
        HttpConfig {
            politeness: Duration::from_millis(80),
            connect_timeout: Duration::from_millis(500),
            total_timeout: Duration::from_millis(500),
            user_agent: DEFAULT_USER_AGENT.to_owned(),
        }
    }

    fn fast_client() -> HttpClient {
        HttpClient::new(fast_config()).unwrap()
    }

    fn ctx() -> FetchCtx {
        FetchCtx {
            board_id: BoardId::new("testco"),
            ats: Ats::Greenhouse,
        }
    }

    // A fixed clock for capture tests — a fn pointer, never the wall clock.
    fn fixed_now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    #[tokio::test]
    async fn sends_browser_ua_and_returns_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;

        let body = fast_client().get_text(&server.uri(), &ctx()).await.unwrap();
        assert_eq!(body, "ok");

        // Inspect what actually crossed the wire — the exact UA a board would see.
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let ua = requests[0]
            .headers
            .get("user-agent")
            .expect("user-agent header sent")
            .to_str()
            .unwrap();
        assert_eq!(ua, DEFAULT_USER_AGENT);
    }

    #[tokio::test]
    async fn http_non_success_maps_to_board_unreachable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let err = fast_client()
            .get_text(&server.uri(), &ctx())
            .await
            .unwrap_err();
        assert_eq!(err, AdapterError::BoardUnreachable { status: 503 });
    }

    #[tokio::test]
    async fn a_hanging_board_hits_the_hard_timeout() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
            .mount(&server)
            .await;

        let err = fast_client()
            .get_text(&server.uri(), &ctx())
            .await
            .unwrap_err();
        // A 500ms timeout against a 5s response — must be a Transport failure, not a hang.
        assert!(matches!(err, AdapterError::Transport(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn a_successful_fetch_is_captured_when_enabled() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"jobs":[1]}"#))
            .mount(&server)
            .await;

        let store = Arc::new(Store::open_in_memory().await.unwrap());
        let client =
            HttpClient::new(fast_config())
                .unwrap()
                .with_capture(store.clone(), 7, fixed_now);

        let body = client.get_text(&server.uri(), &ctx()).await.unwrap();
        assert_eq!(body, r#"{"jobs":[1]}"#);

        // The raw body landed in the ledger, attributed to the board and stamped by the
        // injected clock — not the wall clock.
        let metas = store
            .list_captures(Some(&BoardId::new("testco")), 10)
            .await
            .unwrap();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].status, 200);
        assert_eq!(metas[0].captured_at, fixed_now().to_rfc3339());
    }

    #[tokio::test]
    async fn a_failed_fetch_captures_nothing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let store = Arc::new(Store::open_in_memory().await.unwrap());
        let client =
            HttpClient::new(fast_config())
                .unwrap()
                .with_capture(store.clone(), 7, fixed_now);

        let err = client.get_text(&server.uri(), &ctx()).await.unwrap_err();
        assert_eq!(err, AdapterError::BoardUnreachable { status: 503 });
        // No body means no sample — the ledger only holds responses we actually read.
        assert!(store.list_captures(None, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn same_host_requests_are_spaced_by_the_politeness_delay() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;

        let client = fast_client();
        let start = Instant::now();
        client.get_text(&server.uri(), &ctx()).await.unwrap();
        client.get_text(&server.uri(), &ctx()).await.unwrap();
        // Second request waits ~80ms behind the first. Allow slack below the nominal
        // delay to stay robust on a loaded runner, but prove a real gap exists.
        assert!(
            start.elapsed() >= Duration::from_millis(60),
            "two same-host requests took only {:?}; politeness gap missing",
            start.elapsed()
        );
    }
}
