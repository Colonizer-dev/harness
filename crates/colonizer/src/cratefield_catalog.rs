//! Cratefield module catalogue: fetches from CATALOG.json and crates.io, with fallback to disk cache
//! and bundled snapshot. Serves Settings → Modules with status (published|unreleased|planned), versions,
//! ports, and links. One-hour TTL cache with graceful network failure handling (issue #1291).

use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const CATALOGUE_URL: &str = "https://raw.githubusercontent.com/Cratefield/harness/main/docs/control-plane/CATALOG.json";
const REGISTRY_URL: &str = "https://crates.io/api/v1/crates?q=cratefield-module&per_page=100";
const CACHE_TTL: Duration = Duration::from_secs(3600);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) const FEATURE: crate::features::Feature = crate::features::Feature {
    name: "cratefield_catalog",
    routes,
    token_scope: None,
    activity: &[],
    kinds: &[],
    start_tasks: None,
};

fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new().route("/api/cratefield/catalog", routing::get(catalog))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Live,
    Cache,
    Snapshot,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModuleStatus {
    Published,
    Unreleased,
    Planned,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModuleLinks {
    pub docs_rs: String,
    #[serde(rename = "crate")]
    pub crate_ref: String,
    pub catalogue: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogueModule {
    pub slug: String,
    pub name: String,
    pub summary: String,
    pub crate_name: String,
    pub version: Option<String>,
    pub registry_version: Option<String>,
    pub status: ModuleStatus,
    pub depends_on: Vec<String>,
    pub needs: Vec<String>,
    pub optional: Vec<String>,
    pub tables: Vec<String>,
    pub links: ModuleLinks,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogueResponse {
    pub source: Source,
    pub fetched_at: String,
    pub error: Option<String>,
    pub registry: Source,
    pub catalogue_url: String,
    pub modules: Vec<CatalogueModule>,
}

#[derive(Clone, Debug, Deserialize)]
struct CatalogueJson {
    modules: Vec<RawModule>,
}

#[derive(Clone, Debug, Deserialize)]
struct RawModule {
    slug: String,
    name: String,
    summary: String,
    depends_on: Vec<String>,
    releases: Vec<Release>,
    detail: Detail,
}

#[derive(Clone, Debug, Deserialize)]
struct Release {
    version: String,
}

#[derive(Clone, Debug, Deserialize)]
struct Detail {
    crate_name: String,
    needs: Vec<String>,
    #[serde(default)]
    optional: Vec<String>,
    #[serde(default)]
    tables: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct RegistryResponse {
    crates: Vec<CrateMeta>,
}

#[derive(Clone, Debug, Deserialize)]
struct CrateMeta {
    name: String,
    max_version: String,
}

fn compute_status(version: Option<&str>, registry_version: Option<&str>) -> ModuleStatus {
    match (version, registry_version) {
        (_, Some(_)) => ModuleStatus::Published,
        (Some(_), None) => ModuleStatus::Unreleased,
        (None, None) => ModuleStatus::Planned,
    }
}

/// Choose which registry source to use based on what succeeded.
#[allow(dead_code)]
fn select_registry_source<'a>(
    live_registry: Option<&'a std::collections::HashMap<String, String>>,
    cached_registry: Option<&'a std::collections::HashMap<String, String>>,
) -> (Source, Option<&'a std::collections::HashMap<String, String>>) {
    if live_registry.is_some() {
        (Source::Live, live_registry)
    } else if cached_registry.is_some() {
        (Source::Cache, cached_registry)
    } else {
        (Source::Snapshot, None)
    }
}

fn make_links(crate_name: &str) -> ModuleLinks {
    ModuleLinks {
        docs_rs: format!("https://docs.rs/{}", crate_name),
        crate_ref: format!("https://crates.io/crates/{}", crate_name),
        catalogue: CATALOGUE_URL.to_string(),
    }
}

/// The bundled snapshot: 10 modules with their latest catalogue version and current registry
/// versions, fetched 2026-10-19.
static SNAPSHOT: LazyLock<CatalogueResponse> =
    LazyLock::new(|| match serde_json::from_str(include_str!("../cratefield-catalog.json")) {
        Ok(resp) => resp,
        Err(e) => panic!("cratefield-catalog.json did not parse: {e}"),
    });

struct CachedCatalogue {
    response: CatalogueResponse,
    fetched_at: Instant,
}

static CACHE: LazyLock<Mutex<Option<CachedCatalogue>>> = LazyLock::new(|| Mutex::new(None));

async fn get_from_cache() -> Option<CatalogueResponse> {
    let guard = CACHE.lock().await;
    guard.as_ref().and_then(|cached| {
        if cached.fetched_at.elapsed() < CACHE_TTL {
            Some(cached.response.clone())
        } else {
            None
        }
    })
}

async fn set_cache(response: CatalogueResponse) {
    let mut guard = CACHE.lock().await;
    *guard = Some(CachedCatalogue {
        response,
        fetched_at: Instant::now(),
    });
}

pub async fn catalog() -> Json<CatalogueResponse> {
    if let Some(cached) = get_from_cache().await {
        return Json(cached);
    }

    let live_cat = fetch_catalogue().await;
    let live_reg = if live_cat.is_ok() {
        fetch_registry().await.ok()
    } else {
        None
    };
    let (cached_cat, cached_reg) = if live_cat.is_err() {
        load_disk_cache().await
    } else {
        (None, None)
    };

    let response = match (live_cat, live_reg, cached_cat, cached_reg) {
        (Ok(cat), Some(reg), _, _) => {
            let modules = build_modules(cat, Some(&reg));
            save_disk_cache(&modules).await;
            build_response(Source::Live, Source::Live, None, modules)
        }
        (Ok(cat), None, _, Some(cached_reg)) => {
            build_response(Source::Live, Source::Cache, None, build_modules(cat, Some(&cached_reg)))
        }
        (Ok(cat), None, _, _) => build_response(Source::Live, Source::Snapshot, None, build_modules(cat, None)),
        (Err(cat_err), _, Some(cat), Some(reg)) => {
            build_response(Source::Cache, Source::Cache, Some(cat_err), build_modules(cat, Some(&reg)))
        }
        (Err(cat_err), _, Some(cat), _) => {
            build_response(Source::Cache, Source::Snapshot, Some(cat_err), build_modules(cat, None))
        }
        (Err(cat_err), _, None, _) => {
            let snapshot = SNAPSHOT.clone();
            CatalogueResponse {
                source: Source::Snapshot,
                fetched_at: snapshot.fetched_at.clone(),
                error: Some(cat_err),
                registry: snapshot.registry.clone(),
                catalogue_url: CATALOGUE_URL.to_string(),
                modules: snapshot.modules.clone(),
            }
        }
    };

    set_cache(response.clone()).await;
    Json(response)
}

fn build_response(source: Source, registry: Source, error: Option<String>, modules: Vec<CatalogueModule>) -> CatalogueResponse {
    CatalogueResponse {
        source,
        fetched_at: Utc::now().to_rfc3339(),
        error,
        registry,
        catalogue_url: CATALOGUE_URL.to_string(),
        modules,
    }
}

fn build_modules(catalogue: CatalogueJson, registry: Option<&std::collections::HashMap<String, String>>) -> Vec<CatalogueModule> {
    catalogue
        .modules
        .into_iter()
        .map(|raw| {
            let version = raw.releases.first().map(|r| r.version.clone());
            let registry_version = registry.and_then(|reg| reg.get(&raw.detail.crate_name).cloned());
            let status = compute_status(version.as_deref(), registry_version.as_deref());

            CatalogueModule {
                slug: raw.slug,
                name: raw.name,
                summary: raw.summary,
                crate_name: raw.detail.crate_name.clone(),
                version,
                registry_version,
                status,
                depends_on: raw.depends_on,
                needs: raw.detail.needs,
                optional: raw.detail.optional,
                tables: raw.detail.tables,
                links: make_links(&raw.detail.crate_name),
            }
        })
        .collect()
}

async fn fetch_catalogue() -> Result<CatalogueJson, String> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {e}"))?;

    let res = client
        .get(CATALOGUE_URL)
        .header("User-Agent", "Colonizer")
        .send()
        .await
        .map_err(|e| format!("Catalogue fetch failed: {e}"))?;

    if !res.status().is_success() {
        return Err(format!("Catalogue returned {}", res.status()));
    }

    res.json().await.map_err(|e| format!("Catalogue parse failed: {e}"))
}

async fn fetch_registry() -> Result<std::collections::HashMap<String, String>, String> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {e}"))?;

    let res = client
        .get(REGISTRY_URL)
        .header("User-Agent", "Colonizer")
        .send()
        .await
        .map_err(|e| format!("Registry fetch failed: {e}"))?;

    if !res.status().is_success() {
        return Err(format!("Registry returned {}", res.status()));
    }

    let data: RegistryResponse = res.json().await.map_err(|e| format!("Registry parse failed: {e}"))?;

    Ok(data.crates.into_iter().map(|c| (c.name, c.max_version)).collect())
}

async fn load_disk_cache() -> (Option<CatalogueJson>, Option<std::collections::HashMap<String, String>>) {
    // TODO: Implement disk cache loading from <data_dir>/cratefield/catalog.json
    // For now, return None to fall back to snapshot.
    (None, None)
}

async fn save_disk_cache(_modules: &[CatalogueModule]) {
    // TODO: Implement disk cache saving to <data_dir>/cratefield/catalog.json
}

#[cfg(test)]
mod tests;
