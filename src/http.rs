//! The shared HTTP layer every adapter fetches through. It exists to be a POLITE,
//! predictable client against boards that are not ours: a desktop-browser User-Agent
//! (several ATSes 403 a default client UA), requests serialized per host with a delay
//! between them, hard timeouts so a dead board can't hang a scan, and — deliberately —
//! no automatic retries, because a retry storm against someone else's careers page is
//! exactly the rudeness this project won't commit.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::Url;
use tokio::sync::Mutex as AsyncMutex;

use crate::adapter::AdapterError;

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
        })
    }

    /// GET a URL, returning the body text. Enforces the per-host delay first, maps an
    /// HTTP non-success to [`AdapterError::BoardUnreachable`] and any network failure to
    /// [`AdapterError::Transport`] — so a caller never mistakes a dead fetch for a body.
    pub async fn get_text(&self, url: &str) -> Result<String, AdapterError> {
        self.send_gated(url, self.client.get(url)).await
    }

    /// POST a JSON body, returning the response text. Same politeness gate and error
    /// mapping as [`get_text`](Self::get_text) — Workday's search endpoint needs it.
    pub async fn post_json(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<String, AdapterError> {
        self.send_gated(url, self.client.post(url).json(body)).await
    }

    /// Run a request behind the per-host politeness gate: wait out the remaining delay for
    /// this host, send, then record the end-time (even on failure, so a failing host stays
    /// spaced out).
    async fn send_gated(
        &self,
        url: &str,
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
        result
    }
}

async fn execute(url: &str, request: reqwest::RequestBuilder) -> Result<String, AdapterError> {
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

    response
        .text()
        .await
        .map_err(|e| AdapterError::Transport(format!("reading body from {url}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fast_client() -> HttpClient {
        HttpClient::new(HttpConfig {
            politeness: Duration::from_millis(80),
            connect_timeout: Duration::from_millis(500),
            total_timeout: Duration::from_millis(500),
            user_agent: DEFAULT_USER_AGENT.to_owned(),
        })
        .unwrap()
    }

    #[tokio::test]
    async fn sends_browser_ua_and_returns_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;

        let body = fast_client().get_text(&server.uri()).await.unwrap();
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

        let err = fast_client().get_text(&server.uri()).await.unwrap_err();
        assert_eq!(err, AdapterError::BoardUnreachable { status: 503 });
    }

    #[tokio::test]
    async fn a_hanging_board_hits_the_hard_timeout() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
            .mount(&server)
            .await;

        let err = fast_client().get_text(&server.uri()).await.unwrap_err();
        // A 500ms timeout against a 5s response — must be a Transport failure, not a hang.
        assert!(matches!(err, AdapterError::Transport(_)), "got {err:?}");
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
        client.get_text(&server.uri()).await.unwrap();
        client.get_text(&server.uri()).await.unwrap();
        // Second request waits ~80ms behind the first. Allow slack below the nominal
        // delay to stay robust on a loaded runner, but prove a real gap exists.
        assert!(
            start.elapsed() >= Duration::from_millis(60),
            "two same-host requests took only {:?}; politeness gap missing",
            start.elapsed()
        );
    }
}
