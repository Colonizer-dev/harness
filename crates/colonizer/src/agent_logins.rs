//! The `agents` and `orchestrator` keys of `GET /api/status` (issue #1211): which agent modules
//! have a credential colonies can run on, so the cockpit's setup check asks "is there a model a
//! colony can use" rather than "is Claude signed in".
//!
//! Detection follows each module's own runner: Claude Code uses the saved Claude login, Codex
//! reads `CODEX_API_KEY` or `OPENAI_API_KEY` (the mothership pushes the `openai` provider's key
//! in as the former), Grok Build reads `XAI_API_KEY` (from the `xai-grok` provider), and OpenCode,
//! Pi, Hermes and ACP reach models only through gateway routes, so any provider with a key (or no
//! auth) satisfies them. Only the presence of a credential and an account label ever leave here;
//! never a value.

use serde_json::{Value, json};

/// What the detection needs to know about one configured model provider.
pub(crate) struct ProviderFact {
    pub id: String,
    pub preset: String,
    pub name: String,
    pub has_key: bool,
    /// Auth `none`: a local server needs no key.
    pub keyless: bool,
}

impl ProviderFact {
    fn usable(&self) -> bool {
        self.has_key || self.keyless
    }
    fn is(&self, id: &str) -> bool {
        self.id == id || self.preset == id
    }
}

/// One agent's verdict.
fn entry(id: &str, name: &str, signed_in: bool, account: Option<String>, kind: &str, checked_at: &str) -> Value {
    json!({
        "id": id,
        "name": name,
        "signed_in": signed_in,
        "account": if signed_in { account } else { None },
        "kind": kind,
        "checked_at": checked_at,
    })
}

/// A key on a provider named like the vendor's, else the vendor's env variable in the mothership's
/// own environment (presence only).
fn vendor_key(
    providers: &[ProviderFact],
    provider_id: &str,
    env_names: &[&str],
    env_present: &dyn Fn(&str) -> bool,
) -> Option<String> {
    if let Some(p) = providers.iter().find(|p| p.is(provider_id) && p.has_key) {
        return Some(format!("{} API key", p.name));
    }
    env_names
        .iter()
        .find(|n| env_present(n))
        .map(|n| format!("{n} set on the Mothership"))
}

/// One entry per installed agent module, in the order given. `claude` is the `claude` object of the
/// status body.
pub(crate) fn agent_logins(
    agents: &[(String, String)],
    claude: &Value,
    providers: &[ProviderFact],
    env_present: &dyn Fn(&str) -> bool,
    checked_at: &str,
) -> Vec<Value> {
    let routes: Vec<&ProviderFact> = providers.iter().filter(|p| p.usable()).collect();
    agents
        .iter()
        .map(|(id, name)| match id.as_str() {
            "claude-code" => {
                let on = claude["configured"].as_bool().unwrap_or(false);
                let account = claude["account"]
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| claude["source"].as_str().map(str::to_string));
                entry(id, name, on, account, "subscription", checked_at)
            }
            "codex" => {
                let key = vendor_key(providers, "openai", &["CODEX_API_KEY", "OPENAI_API_KEY"], env_present);
                entry(id, name, key.is_some(), key, "api_key", checked_at)
            }
            "grok-build" => {
                let key = vendor_key(providers, "xai-grok", &["XAI_API_KEY"], env_present);
                entry(id, name, key.is_some(), key, "api_key", checked_at)
            }
            _ => {
                let account = match routes.len() {
                    0 => None,
                    1 => Some(format!("via {}", routes[0].name)),
                    n => Some(format!("via {n} providers")),
                };
                entry(id, name, !routes.is_empty(), account, "gateway", checked_at)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(id: &str, has_key: bool, keyless: bool) -> ProviderFact {
        ProviderFact {
            id: id.into(),
            preset: id.into(),
            name: id.to_uppercase(),
            has_key,
            keyless,
        }
    }

    fn agents() -> Vec<(String, String)> {
        ["claude-code", "codex", "grok-build", "opencode", "pi", "hermes", "acp"]
            .iter()
            .map(|id| (id.to_string(), id.to_string()))
            .collect()
    }

    fn by_id<'a>(list: &'a [Value], id: &str) -> &'a Value {
        list.iter().find(|v| v["id"] == id).unwrap()
    }

    #[test]
    fn nothing_configured_signs_nobody_in() {
        let claude = json!({"configured": false});
        let list = agent_logins(&agents(), &claude, &[], &|_| false, "t");
        assert_eq!(list.len(), 7);
        assert!(list.iter().all(|a| a["signed_in"] == false && a["account"].is_null()));
    }

    #[test]
    fn each_agent_is_detected_the_way_its_runner_reads_credentials() {
        let claude = json!({"configured": true, "account": "me@x.dev", "source": "saved token"});
        let providers = [provider("openai", true, false), provider("minimax", true, false)];
        let list = agent_logins(&agents(), &claude, &providers, &|_| false, "t");
        assert_eq!(by_id(&list, "claude-code")["account"], "me@x.dev");
        assert_eq!(by_id(&list, "codex")["signed_in"], true);
        assert_eq!(by_id(&list, "grok-build")["signed_in"], false);
        assert_eq!(by_id(&list, "opencode")["account"], "via 2 providers");
        assert_eq!(by_id(&list, "pi")["kind"], "gateway");
    }

    #[test]
    fn a_vendor_env_variable_counts_and_its_value_is_never_read() {
        let claude = json!({"configured": false});
        let list = agent_logins(&agents(), &claude, &[], &|n| n == "XAI_API_KEY", "t");
        let grok = by_id(&list, "grok-build");
        assert_eq!(grok["signed_in"], true);
        assert_eq!(grok["account"], "XAI_API_KEY set on the Mothership");
        assert_eq!(by_id(&list, "codex")["signed_in"], false);
    }

    #[test]
    fn a_local_server_with_no_auth_is_a_route_and_a_keyless_cloud_provider_is_not() {
        let claude = json!({"configured": false});
        let list = agent_logins(&agents(), &claude, &[provider("minimax", false, false)], &|_| false, "t");
        assert_eq!(by_id(&list, "opencode")["signed_in"], false);
        let list = agent_logins(&agents(), &claude, &[provider("lan", false, true)], &|_| false, "t");
        assert_eq!(by_id(&list, "opencode")["account"], "via LAN");
    }
}
