//! Bounded, anonymous OpenRouter model discovery, separate from inference.
use super::{OpenRouterModel, SelectedModelMetadata};
use chrono::{DateTime, Utc};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

const MAX_BYTES: usize = 8 * 1024 * 1024;
const SOURCE: &str = "https://openrouter.ai/api/v1/models";

#[derive(Debug, Clone)]
pub struct CatalogSnapshot {
    pub fetched_at: DateTime<Utc>,
    pub models: Vec<OpenRouterModel>,
    pub excluded_entries: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogPolicy {
    PreferCache,
    Refresh,
    Offline,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogFreshness {
    Fresh,
    Stale,
}
#[derive(Debug, Clone)]
pub struct CatalogResult {
    pub snapshot: Arc<CatalogSnapshot>,
    pub freshness: CatalogFreshness,
    pub offline: bool,
    pub warning: Option<CatalogWarning>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CatalogError {
    #[error("OpenRouter catalog response is invalid")]
    Invalid,
    #[error("OpenRouter catalog exceeds the size or entry limit")]
    Limit,
    #[error("OpenRouter catalog request failed")]
    Network,
    #[error("OpenRouter catalog service returned HTTP {0}")]
    Http(u16),
    #[error("OpenRouter catalog request was cancelled")]
    Cancelled,
    #[error("No usable OpenRouter catalog cache is available; refresh the catalog online")]
    NoCache,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CatalogWarning {
    #[error("OpenRouter catalog refresh failed; using cached metadata")]
    RefreshFailed,
    #[error("OpenRouter catalog was fetched but could not be saved")]
    SaveFailed,
}
pub struct CatalogClient {
    client: reqwest::Client,
    cache_path: Option<PathBuf>,
    endpoint: String,
    #[cfg(test)]
    now: Option<DateTime<Utc>>,
}
impl CatalogClient {
    pub fn new(cache_path: Option<PathBuf>) -> Result<Self, CatalogError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|_| CatalogError::Network)?;
        Ok(Self {
            client,
            cache_path,
            endpoint: SOURCE.to_owned(),
            #[cfg(test)]
            now: None,
        })
    }
    fn now(&self) -> DateTime<Utc> {
        #[cfg(test)]
        if let Some(now) = self.now {
            return now;
        }
        Utc::now()
    }
    pub async fn load(
        &self,
        policy: CatalogPolicy,
        cancel: CancellationToken,
    ) -> Result<CatalogResult, CatalogError> {
        if cancel.is_cancelled() {
            return Err(CatalogError::Cancelled);
        }
        let cached = self
            .cache_path
            .as_deref()
            .and_then(|p| read_cached(p, self.now()).ok().flatten());
        if policy == CatalogPolicy::Offline {
            return cached
                .map(|mut r| {
                    r.offline = true;
                    r
                })
                .ok_or(CatalogError::NoCache);
        }
        if policy == CatalogPolicy::PreferCache
            && let Some(result) = cached
                .as_ref()
                .filter(|r| r.freshness == CatalogFreshness::Fresh)
        {
            return Ok(result.clone());
        }
        let fetched = tokio::select! { biased; _=cancel.cancelled()=>return Err(CatalogError::Cancelled),result=self.fetch()=>result };
        match fetched {
            Ok(snapshot) => {
                let warning = self
                    .cache_path
                    .as_deref()
                    .and_then(|p| save_cache(p, &snapshot).err())
                    .map(|_| CatalogWarning::SaveFailed);
                Ok(CatalogResult {
                    snapshot: Arc::new(snapshot),
                    freshness: CatalogFreshness::Fresh,
                    offline: false,
                    warning,
                })
            }
            Err(error) => cached
                .map(|mut result| {
                    result.warning = Some(CatalogWarning::RefreshFailed);
                    result
                })
                .ok_or(error),
        }
    }
    async fn fetch(&self) -> Result<CatalogSnapshot, CatalogError> {
        let mut response = self
            .client
            .get(&self.endpoint)
            .send()
            .await
            .map_err(|_| CatalogError::Network)?;
        if !response.status().is_success() {
            return Err(CatalogError::Http(response.status().as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_BYTES as u64)
        {
            return Err(CatalogError::Limit);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| CatalogError::Network)? {
            if chunk.len() > MAX_BYTES - bytes.len() {
                return Err(CatalogError::Limit);
            }
            bytes.extend_from_slice(&chunk);
        }
        parse_catalog(&bytes, self.now())
    }
}
pub fn default_cache_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|p| p.join("doge-code/openrouter/models-v1.json"))
}
#[derive(serde::Serialize, serde::Deserialize)]
struct CacheEnvelope {
    schema_version: u32,
    source: String,
    fetched_at: DateTime<Utc>,
    models: Vec<OpenRouterModel>,
    excluded_entries: usize,
}
pub fn read_cached(path: &Path, now: DateTime<Utc>) -> Result<Option<CatalogResult>, CatalogError> {
    use std::io::Read;
    let Ok(file) = std::fs::File::open(path) else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    if file
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > MAX_BYTES
    {
        return Ok(None);
    }
    let Ok(mut envelope) = serde_json::from_slice::<CacheEnvelope>(&bytes) else {
        return Ok(None);
    };
    if envelope.schema_version != 1
        || envelope.source != SOURCE
        || envelope.fetched_at > now + chrono::Duration::minutes(5)
        || envelope.models.is_empty()
        || envelope.models.len() > 10000
    {
        return Ok(None);
    }
    let mut ids = std::collections::HashSet::new();
    for model in &mut envelope.models {
        if super::model_id(&model.id).is_err() || !ids.insert(model.id.clone()) {
            return Ok(None);
        }
        model.name = model.name.take().filter(|s| s.chars().count() <= 256);
        model.context_window = model.context_window.filter(|n| *n > 0);
        for price in [
            &mut model.prices.prompt,
            &mut model.prices.completion,
            &mut model.prices.request,
        ] {
            *price = price.take().filter(|s| s.len() <= 128);
        }
    }
    let freshness = if now - envelope.fetched_at < chrono::Duration::hours(24) {
        CatalogFreshness::Fresh
    } else {
        CatalogFreshness::Stale
    };
    Ok(Some(CatalogResult {
        snapshot: Arc::new(CatalogSnapshot {
            fetched_at: envelope.fetched_at,
            models: envelope.models,
            excluded_entries: envelope.excluded_entries,
        }),
        freshness,
        offline: false,
        warning: None,
    }))
}
fn save_cache(path: &Path, snapshot: &CatalogSnapshot) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("cache directory unavailable"))?;
    let envelope = CacheEnvelope {
        schema_version: 1,
        source: SOURCE.to_owned(),
        fetched_at: snapshot.fetched_at,
        models: snapshot.models.clone(),
        excluded_entries: snapshot.excluded_entries,
    };
    let bytes = serde_json::to_vec(&envelope)?;
    if bytes.len() > MAX_BYTES {
        return Err(std::io::Error::other("cache exceeds limit"));
    }
    std::fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(&bytes)?;
    temp.flush()?;
    temp.persist(path).map_err(|e| e.error)?;
    Ok(())
}
fn strings(value: Option<&serde_json::Value>) -> Option<Vec<String>> {
    value?
        .as_array()?
        .iter()
        .map(|v| v.as_str().map(str::to_owned))
        .collect()
}
fn price(value: Option<&serde_json::Value>) -> Option<String> {
    value?
        .as_str()
        .filter(|s| s.len() <= 128)
        .map(str::to_owned)
}
pub fn parse_catalog(
    bytes: &[u8],
    fetched_at: DateTime<Utc>,
) -> Result<CatalogSnapshot, CatalogError> {
    use super::{CatalogPrices, EffortSupport, ReasoningMetadata};
    if bytes.len() > MAX_BYTES {
        return Err(CatalogError::Limit);
    }
    let outer: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| CatalogError::Invalid)?;
    let data = outer
        .as_object()
        .and_then(|o| o.get("data"))
        .and_then(serde_json::Value::as_array)
        .ok_or(CatalogError::Invalid)?;
    if data.len() > 10000 {
        return Err(CatalogError::Limit);
    }
    let mut ids = std::collections::HashSet::new();
    let mut models = Vec::new();
    let mut excluded_entries = 0;
    for item in data {
        let Some(id) = item
            .get("id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| super::model_id(id).is_ok())
        else {
            excluded_entries += 1;
            continue;
        };
        if !ids.insert(id) {
            return Err(CatalogError::Invalid);
        }
        let architecture = item.get("architecture");
        let pricing = item.get("pricing");
        let reasoning = item.get("reasoning");
        let supported_efforts = match reasoning.and_then(|r| r.get("supported_efforts")) {
            Some(serde_json::Value::Null) => EffortSupport::GatewayAccepted,
            value => strings(value)
                .map(EffortSupport::Listed)
                .unwrap_or(EffortSupport::Unknown),
        };
        models.push(OpenRouterModel {
            id: id.to_owned(),
            name: item
                .get("name")
                .and_then(serde_json::Value::as_str)
                .filter(|s| s.chars().count() <= 256)
                .map(str::to_owned),
            input_modalities: strings(architecture.and_then(|a| a.get("input_modalities"))),
            output_modalities: strings(architecture.and_then(|a| a.get("output_modalities"))),
            supported_parameters: strings(item.get("supported_parameters")),
            context_window: item
                .get("context_length")
                .and_then(serde_json::Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| *n > 0),
            prices: CatalogPrices {
                prompt: price(pricing.and_then(|p| p.get("prompt"))),
                completion: price(pricing.and_then(|p| p.get("completion"))),
                request: price(pricing.and_then(|p| p.get("request"))),
                conditional: pricing
                    .and_then(serde_json::Value::as_object)
                    .and_then(|p| match p.get("overrides") {
                        None => Some(false),
                        Some(value) => value
                            .as_array()
                            .filter(|entries| entries.iter().all(serde_json::Value::is_object))
                            .map(|entries| !entries.is_empty()),
                    }),
            },
            reasoning: ReasoningMetadata {
                supported_efforts,
                mandatory: reasoning
                    .and_then(|r| r.get("mandatory"))
                    .and_then(serde_json::Value::as_bool),
            },
        });
    }
    if models.is_empty() {
        return Err(CatalogError::Invalid);
    }
    Ok(CatalogSnapshot {
        fetched_at,
        models,
        excluded_entries,
    })
}
impl CatalogSnapshot {
    pub fn selected_metadata(&self, id: &str) -> Option<Arc<SelectedModelMetadata>> {
        self.models.iter().find(|m| m.id == id).map(|model| {
            Arc::new(SelectedModelMetadata {
                provider: crate::features::openai_subscription::ProviderKind::Openrouter,
                fetched_at: self.fetched_at,
                model: model.clone(),
            })
        })
    }
}
fn safe(value: &str) -> String {
    crate::features::safe_display::text(value, None)
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}
fn display_price(value: Option<&str>) -> String {
    value
        .filter(|s| s.parse::<f64>().is_ok_and(|n| n.is_finite() && n >= 0.0))
        .map(safe)
        .unwrap_or_else(|| "unknown".to_owned())
}
fn display_list(value: Option<&Vec<String>>) -> String {
    value
        .map(|v| v.iter().map(|s| safe(s)).collect::<Vec<_>>().join(", "))
        .unwrap_or_else(|| "unknown".into())
}
pub fn format_catalog(result: &CatalogResult, exact_model: Option<&str>) -> anyhow::Result<String> {
    use std::fmt::Write;
    let models: Vec<_> = match exact_model {
        Some(id) => vec![
            result
                .snapshot
                .models
                .iter()
                .find(|m| m.id == id)
                .ok_or_else(|| anyhow::anyhow!("OpenRouter model ID not found in catalog"))?,
        ],
        None => result.snapshot.models.iter().collect(),
    };
    let mut output = format!(
        "OpenRouter — metered billing; reference prices in USD (not a billing estimate)\nFetched: {} | {}{} | excluded entries: {}\n",
        result.snapshot.fetched_at.to_rfc3339(),
        if result.freshness == CatalogFreshness::Fresh {
            "fresh"
        } else {
            "stale"
        },
        if result.offline { " / offline" } else { "" },
        result.snapshot.excluded_entries
    );
    if let Some(warning) = result.warning {
        writeln!(output, "Warning: {warning}")?;
    }
    for model in models {
        writeln!(
            output,
            "{}{}",
            safe(&model.id),
            model
                .name
                .as_ref()
                .map(|s| format!(" — {}", safe(s)))
                .unwrap_or_default()
        )?;
        writeln!(
            output,
            "  input: {}; output: {}; supported parameters: {}",
            display_list(model.input_modalities.as_ref()),
            display_list(model.output_modalities.as_ref()),
            display_list(model.supported_parameters.as_ref())
        )?;
        writeln!(
            output,
            "  context: {}; prompt USD/token: {}; completion USD/token: {}; request USD: {}; conditional/overridden pricing: {}",
            model
                .context_window
                .map(|n| n.to_string())
                .unwrap_or_else(|| "unknown".into()),
            display_price(model.prices.prompt.as_deref()),
            display_price(model.prices.completion.as_deref()),
            display_price(model.prices.request.as_deref()),
            match model.prices.conditional {
                Some(true) => "present",
                Some(false) => "not reported",
                None => "unknown",
            }
        )?;
        writeln!(
            output,
            "  reasoning efforts: {}; mandatory: {}",
            match &model.reasoning.supported_efforts {
                super::EffortSupport::Unknown => "unknown".into(),
                super::EffortSupport::GatewayAccepted => "gateway accepted".into(),
                super::EffortSupport::Listed(e) =>
                    e.iter().map(|s| safe(s)).collect::<Vec<_>>().join(", "),
            },
            model
                .reasoning
                .mandatory
                .map(|b| b.to_string())
                .unwrap_or_else(|| "unknown".into())
        )?;
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::super::EffortSupport;
    use super::*;
    use chrono::{Duration, TimeZone};
    use serde_json::{Value, json};
    fn now() -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000, 0).unwrap()
    }
    fn model() -> Value {
        json!({"id":"Publisher/Model:free"})
    }
    fn parse(data: Vec<Value>) -> Result<CatalogSnapshot, CatalogError> {
        parse_catalog(&serde_json::to_vec(&json!({"data":data})).unwrap(), now())
    }
    #[test]
    fn openrouter_catalog_preserves_unknowns_and_exact_ids() {
        let snapshot = parse(vec![model(), json!({"id":"p/b", "architecture":{"input_modalities":[],"output_modalities":["text"]}, "supported_parameters":[], "context_length":0, "pricing":{"prompt":"0", "completion":"0.0001","request":"1", "overrides":[{"min_prompt_tokens":200000,"prompt":"1"}]}, "reasoning":{"supported_efforts":null, "mandatory":true}})]).unwrap();
        let a = &snapshot.models[0];
        assert_eq!(a.id, "Publisher/Model:free");
        assert_eq!(a.supported_parameters, None);
        assert_eq!(a.output_modalities, None);
        assert_eq!(a.context_window, None);
        assert_eq!(a.prices.prompt, None);
        assert_eq!(a.reasoning.supported_efforts, EffortSupport::Unknown);
        let b = &snapshot.models[1];
        assert_eq!(b.input_modalities, Some(vec![]));
        assert_eq!(b.supported_parameters, Some(vec![]));
        assert_eq!(b.context_window, None);
        assert_eq!(
            b.reasoning.supported_efforts,
            EffortSupport::GatewayAccepted
        );
        assert_eq!(b.reasoning.mandatory, Some(true));
        assert_eq!(b.prices.prompt.as_deref(), Some("0"));
        assert_eq!(b.prices.conditional, Some(true));
        assert!(snapshot.selected_metadata("publisher/model:free").is_none());
        assert_eq!(
            snapshot
                .selected_metadata("Publisher/Model:free")
                .unwrap()
                .model
                .id,
            a.id
        );
    }
    #[test]
    fn openrouter_catalog_optional_invalid_values_are_unknown() {
        for value in [
            json!(0),
            json!(-1),
            json!("5"),
            json!(4294967296u64),
            Value::Null,
        ] {
            let snapshot=parse(vec![json!({"id":"p/a", "context_length":value, "name":true, "architecture":{"output_modalities":["text", 1]}, "supported_parameters":false,"pricing":{"prompt":1},"reasoning":{"supported_efforts":["high",false],"mandatory":"true"}})]).unwrap();
            let m = &snapshot.models[0];
            assert_eq!(m.context_window, None);
            assert_eq!(m.name, None);
            assert_eq!(m.output_modalities, None);
            assert_eq!(m.supported_parameters, None);
            assert_eq!(m.prices.prompt, None);
            assert_eq!(m.reasoning.supported_efforts, EffortSupport::Unknown);
            assert_eq!(m.reasoning.mandatory, None);
        }
    }
    #[test]
    fn openrouter_catalog_limits_and_invalid_entries() {
        let s = parse(vec![model(), json!({"id":"bad"}), Value::Null]).unwrap();
        assert_eq!(s.excluded_entries, 2);
        assert!(parse(vec![json!({"id":"bad"})]).is_err());
        assert!(parse(vec![model(), model()]).is_err());
        for n in [256, 257] {
            let s=parse(vec![json!({"id":"p/a","name":"界".repeat(n),"pricing":{"prompt":"0".repeat(if n==256 {128}else{129})}})]).unwrap();
            assert_eq!(s.models[0].name.is_some(), n == 256);
            assert_eq!(s.models[0].prices.prompt.is_some(), n == 256);
        }
        assert!(parse(vec![model(); 10001]).is_err());
        let entries = (0..10000).map(|n| json!({"id":format!("p/{n}")})).collect();
        assert_eq!(parse(entries).unwrap().models.len(), 10000);
        let mut bytes = serde_json::to_vec(&json!({"data":[model()]})).unwrap();
        bytes.resize(MAX_BYTES, b' ');
        assert!(parse_catalog(&bytes, now()).is_ok());
        bytes.push(b' ');
        assert_eq!(
            parse_catalog(&bytes, now()).unwrap_err(),
            CatalogError::Limit
        );
        for outer in [
            json!([]),
            json!({}),
            json!({"data":null}),
            json!({"data":[]}),
        ] {
            assert!(parse_catalog(&serde_json::to_vec(&outer).unwrap(), now()).is_err());
        }
    }
    fn write_cache(path: &Path, fetched_at: DateTime<Utc>) {
        let snapshot = parse(vec![model()]).unwrap();
        std::fs::write(path,serde_json::to_vec(&json!({"schema_version":1,"source":SOURCE,"fetched_at":fetched_at,"models":snapshot.models,"excluded_entries":0})).unwrap()).unwrap();
    }
    #[test]
    fn openrouter_catalog_cache_time_and_envelope_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.json");
        for (age, expected) in [
            (
                Duration::hours(24) - Duration::seconds(1),
                Some(CatalogFreshness::Fresh),
            ),
            (Duration::hours(24), Some(CatalogFreshness::Stale)),
            (-Duration::minutes(5), Some(CatalogFreshness::Fresh)),
            (-Duration::minutes(5) - Duration::seconds(1), None),
        ] {
            write_cache(&path, now() - age);
            assert_eq!(
                read_cached(&path, now()).unwrap().map(|r| r.freshness),
                expected
            );
        }
        write_cache(&path, now());
        let mut envelope: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for (key, value) in [
            ("schema_version", json!(2)),
            ("source", json!("https://untrusted.invalid")),
        ] {
            let mut bad = envelope.clone();
            bad[key] = value;
            std::fs::write(&path, bad.to_string()).unwrap();
            assert!(read_cached(&path, now()).unwrap().is_none());
        }
        envelope["models"][0]["id"] = json!("invalid");
        std::fs::write(&path, envelope.to_string()).unwrap();
        assert!(read_cached(&path, now()).unwrap().is_none());
        std::fs::write(&path, vec![b' '; MAX_BYTES + 1]).unwrap();
        assert!(read_cached(&path, now()).unwrap().is_none());
    }
    #[test]
    fn openrouter_catalog_format_is_safe_and_exact() {
        let snapshot=parse(vec![json!({"id":"P/model:free","name":"\u{001b}[31mVisible\r\nName","pricing":{"prompt":"NaN","completion":"-1","request":"0"}})]).unwrap();
        let result = CatalogResult {
            snapshot: Arc::new(snapshot),
            freshness: CatalogFreshness::Stale,
            offline: true,
            warning: Some(CatalogWarning::RefreshFailed),
        };
        let rendered = format_catalog(&result, Some("P/model:free")).unwrap();
        assert!(rendered.contains("P/model:free"));
        assert!(rendered.contains("unknown"));
        assert!(rendered.contains("USD"));
        assert!(!rendered.contains('\u{001b}'));
        assert!(!rendered.contains('\r'));
        assert!(!rendered.contains("NaN"));
        assert!(format_catalog(&result, Some("p/model:free")).is_err());
    }
    async fn server(
        status: u16,
        headers: &str,
        body: Vec<u8>,
    ) -> (
        String,
        Arc<std::sync::atomic::AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/models", listener.local_addr().unwrap());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = calls.clone();
        let headers = headers.to_owned();
        let handle = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0u8; 8192];
                let n = socket.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..n]).to_lowercase();
                assert!(request.starts_with("get /models "));
                for forbidden in ["authorization:", "cookie:", "http-referer:", "x-title:"] {
                    assert!(!request.contains(forbidden));
                }
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let response = format!(
                    "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
                    body.len()
                );
                if socket.write_all(response.as_bytes()).await.is_ok() {
                    let _ = socket.write_all(&body).await;
                }
            }
        });
        (url, calls, handle)
    }
    fn test_client(endpoint: String, path: Option<PathBuf>) -> CatalogClient {
        let mut client = CatalogClient::new(path).unwrap();
        client.endpoint = endpoint;
        client.now = Some(now());
        client
    }
    #[tokio::test]
    async fn openrouter_catalog_policy_and_cancellation() {
        use std::sync::atomic::Ordering;
        let (url, calls, handle) = server(
            200,
            "",
            serde_json::to_vec(&json!({"data":[model()]})).unwrap(),
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.json");
        write_cache(&path, now());
        let client = test_client(url, Some(path.clone()));
        assert!(
            !client
                .load(CatalogPolicy::PreferCache, CancellationToken::new())
                .await
                .unwrap()
                .offline
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(
            client
                .load(CatalogPolicy::Offline, CancellationToken::new())
                .await
                .unwrap()
                .offline
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        client
            .load(CatalogPolicy::Refresh, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        write_cache(&path, now() - Duration::hours(25));
        client
            .load(CatalogPolicy::PreferCache, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(
            client
                .load(CatalogPolicy::Refresh, cancel)
                .await
                .unwrap_err(),
            CatalogError::Cancelled
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        handle.abort();
    }
    #[tokio::test]
    async fn openrouter_catalog_failed_refresh_preserves_cache_and_warns() {
        let (url, calls, handle) = server(503, "", b"SECRET_RESPONSE_BODY".to_vec()).await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.json");
        write_cache(&path, now() - Duration::hours(25));
        let original = std::fs::read(&path).unwrap();
        let client = test_client(url.clone(), Some(path.clone()));
        let result = client
            .load(CatalogPolicy::PreferCache, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.warning, Some(CatalogWarning::RefreshFailed));
        assert_eq!(result.freshness, CatalogFreshness::Stale);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let error = test_client(url, None)
            .load(CatalogPolicy::Refresh, CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error, CatalogError::Http(503));
        assert!(!error.to_string().contains("SECRET"));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        handle.abort();
    }
    #[tokio::test]
    async fn openrouter_catalog_offline_without_cache_and_save_failure() {
        let dir = tempfile::tempdir().unwrap();
        let (url, calls, handle) = server(
            200,
            "",
            serde_json::to_vec(&json!({"data":[model()]})).unwrap(),
        )
        .await;
        let client = test_client(url, Some(dir.path().to_owned()));
        assert_eq!(
            client
                .load(CatalogPolicy::Offline, CancellationToken::new())
                .await
                .unwrap_err(),
            CatalogError::NoCache
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        let result = client
            .load(CatalogPolicy::Refresh, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.warning, Some(CatalogWarning::SaveFailed));
        assert_eq!(result.snapshot.models.len(), 1);
        handle.abort();
    }
    #[tokio::test]
    async fn openrouter_catalog_redirect_is_not_followed() {
        let (url, calls, handle) =
            server(302, "Location: http://127.0.0.1:1/SECRET\r\n", Vec::new()).await;
        assert_eq!(
            test_client(url, None)
                .load(CatalogPolicy::Refresh, CancellationToken::new())
                .await
                .unwrap_err(),
            CatalogError::Http(302)
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        handle.abort();
    }
    #[tokio::test]
    async fn openrouter_catalog_cancel_during_read() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = test_client(
            format!("http://{}/models", listener.local_addr().unwrap()),
            None,
        );
        let (started, ready) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0; 2048];
            let read = socket.read(&mut buf).await.unwrap();
            assert!(read > 0, "fixture received an HTTP request");
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10000\r\n\r\n{")
                .await
                .unwrap();
            let _ = started.send(());
            std::future::pending::<()>().await;
        });
        let cancel = CancellationToken::new();
        let request = client.load(CatalogPolicy::Refresh, cancel.clone());
        tokio::pin!(request);
        tokio::select! { _=ready=>{},result=&mut request=>panic!("unexpected {result:?}") };
        cancel.cancel();
        assert_eq!(request.await.unwrap_err(), CatalogError::Cancelled);
        handle.abort();
    }

    #[tokio::test]
    async fn openrouter_catalog_decompressed_size_is_bounded() {
        // Run-length encoded gzip of valid JSON followed by 8 MiB of whitespace.
        let runs: &[(u8, usize)] = &[
            (31, 1),
            (139, 1),
            (8, 1),
            (0, 5),
            (2, 1),
            (255, 1),
            (236, 1),
            (193, 1),
            (161, 1),
            (13, 1),
            (192, 1),
            (32, 1),
            (20, 1),
            (5, 1),
            (192, 1),
            (85, 1),
            (154, 1),
            (183, 1),
            (64, 1),
            (61, 1),
            (171, 1),
            (52, 1),
            (136, 1),
            (159, 1),
            (96, 1),
            (112, 1),
            (136, 1),
            (58, 1),
            (194, 1),
            (238, 1),
            (44, 1),
            (114, 1),
            (119, 1),
            (59, 1),
            (163, 1),
            (254, 1),
            (74, 1),
            (251, 1),
            (118, 1),
            (230, 1),
            (72, 1),
            (203, 1),
            (122, 1),
            (43, 1),
            (167, 1),
            (159, 1),
            (7, 1),
            (0, 4089),
            (184, 1),
            (237, 1),
            (193, 1),
            (1, 2),
            (0, 3),
            (128, 1),
            (144, 1),
            (255, 1),
            (175, 1),
            (27, 1),
            (18, 1),
            (0, 4038),
            (96, 1),
            (45, 1),
            (245, 1),
            (156, 1),
            (253, 1),
            (10, 1),
            (23, 1),
            (0, 1),
            (128, 1),
            (0, 1),
        ];
        let body: Vec<u8> = runs
            .iter()
            .flat_map(|&(byte, count)| std::iter::repeat_n(byte, count))
            .collect();
        assert!(body.len() < 10000);
        let (url, _, handle) = server(200, "Content-Encoding: gzip\r\n", body).await;
        assert_eq!(
            test_client(url, None)
                .load(CatalogPolicy::Refresh, CancellationToken::new())
                .await
                .unwrap_err(),
            CatalogError::Limit
        );
        handle.abort();
    }
    #[test]
    fn openrouter_catalog_cache_exact_size_and_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.json");
        write_cache(&path, now());
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.resize(MAX_BYTES, b' ');
        std::fs::write(&path, &bytes).unwrap();
        assert!(read_cached(&path, now()).unwrap().is_some());
        for bytes in [b"not json".as_slice(), b"{}", b"null"] {
            std::fs::write(&path, bytes).unwrap();
            assert!(read_cached(&path, now()).unwrap().is_none());
        }
        assert!(
            read_cached(&dir.path().join("missing"), now())
                .unwrap()
                .is_none()
        );
    }
    #[tokio::test]
    async fn openrouter_catalog_invalid_updates_never_replace_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.json");
        write_cache(&path, now());
        let original = std::fs::read(&path).unwrap();
        for body in [
            b"not JSON".to_vec(),
            serde_json::to_vec(&json!({"data":[model(),model()]})).unwrap(),
            serde_json::to_vec(&json!({"data":[{"id":"invalid"}]})).unwrap(),
        ] {
            let (url, calls, handle) = server(200, "", body).await;
            let result = test_client(url, Some(path.clone()))
                .load(CatalogPolicy::Refresh, CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(result.warning, Some(CatalogWarning::RefreshFailed));
            assert_eq!(std::fs::read(&path).unwrap(), original);
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
            handle.abort();
        }
    }
    #[tokio::test]
    async fn openrouter_catalog_serialized_cache_limit_keeps_memory_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.json");
        write_cache(&path, now());
        let original = std::fs::read(&path).unwrap();
        let body = serde_json::to_vec(
            &json!({"data":[{"id":"p/a","supported_parameters":["x".repeat(MAX_BYTES-100)]}]}),
        )
        .unwrap();
        assert!(body.len() <= MAX_BYTES);
        let (url, _, handle) = server(200, "", body).await;
        let result = test_client(url, Some(path.clone()))
            .load(CatalogPolicy::Refresh, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.warning, Some(CatalogWarning::SaveFailed));
        assert_eq!(result.snapshot.models[0].id, "p/a");
        assert_eq!(std::fs::read(path).unwrap(), original);
        handle.abort();
    }
    #[tokio::test]
    async fn openrouter_catalog_memory_only_and_normalized_cache() {
        let body = serde_json::to_vec(
            &json!({"data":[{"id":"p/a","unknown_private_field":"SHOULD_NOT_PERSIST"}]}),
        )
        .unwrap();
        let (url, calls, handle) = server(200, "", body).await;
        let memory = test_client(url.clone(), None)
            .load(CatalogPolicy::Refresh, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(memory.warning, None);
        assert_eq!(memory.snapshot.models[0].id, "p/a");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/models.json");
        let client = test_client(url, Some(path.clone()));
        client
            .load(CatalogPolicy::Refresh, CancellationToken::new())
            .await
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            !String::from_utf8(bytes)
                .unwrap()
                .contains("SHOULD_NOT_PERSIST")
        );
        assert!(read_cached(&path, now()).unwrap().is_some());
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        handle.abort();
    }
    #[tokio::test]
    async fn openrouter_catalog_total_timeout_bounds_body_read() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = test_client(
            format!("http://{}/models", listener.local_addr().unwrap()),
            None,
        );
        let (started, ready) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0; 2048];
            let read = socket.read(&mut buf).await.unwrap();
            assert!(read > 0, "fixture received an HTTP request");
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10000\r\n\r\n{")
                .await
                .unwrap();
            let _ = started.send(());
            std::future::pending::<()>().await;
        });
        let request = client.load(CatalogPolicy::Refresh, CancellationToken::new());
        tokio::pin!(request);
        tokio::select! {_=ready=>{},result=&mut request=>panic!("unexpected {result:?}")};
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(11)).await;
        assert_eq!(request.await.unwrap_err(), CatalogError::Network);
        handle.abort();
    }
    #[test]
    fn openrouter_catalog_conditional_prices_only_follow_valid_overrides() {
        for (pricing, expected) in [
            (
                json!({"prompt":"1","image":"0","input_cache_read":"0"}),
                Some(false),
            ),
            (json!({"overrides":[]}), Some(false)),
            (
                json!({"overrides":[{"min_prompt_tokens":100,"prompt":"2"}]}),
                Some(true),
            ),
            (json!({"overrides":"invalid"}), None),
            (json!({"overrides":[false]}), None),
            (serde_json::Value::Null, None),
        ] {
            let snapshot = parse(vec![json!({"id":"p/a","pricing":pricing})]).unwrap();
            assert_eq!(snapshot.models[0].prices.conditional, expected);
        }
    }
}
