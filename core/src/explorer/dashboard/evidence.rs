//! Typed parse of `details_json` / `route_json` plus known-token decimals.
use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One swap leg from `route_json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RouteLeg {
    pub pool: Option<String>,
    pub amm: Option<String>,
    pub token_in: Option<String>,
    pub token_out: Option<String>,
    pub token_source: Option<String>,
    pub amount_in: Option<String>,
    pub amount_out: Option<String>,
}

/// Sectioned evidence blob for one op (P10 drill-down).
#[derive(Debug, Clone, Serialize, Default)]
pub struct OpEvidence {
    pub tx_hash: String,
    pub block_number: u64,
    pub kind: String,
    pub subkind: Option<String>,
    pub confidence: String,
    pub detector: String,
    pub eoa: String,
    pub contract: Option<String>,
    pub profit_usd: Option<f64>,
    pub net_profit_usd: Option<f64>,
    pub gas_cost_usd: Option<f64>,
    pub volume_usd: Option<f64>,
    pub flashloan_fee_usd: Option<f64>,
    pub profit_token: Option<String>,
    pub profit_amount: Option<String>,
    pub profit_amount_human: Option<String>,
    pub excluded_from_net: bool,
    pub route: Vec<RouteLeg>,
    pub victim_hashes: Vec<String>,
    pub tags: Vec<String>,
    pub reasons: Vec<String>,
    pub pnl_basis: Option<String>,
    pub arb_meta: Option<Value>,
    pub details: Value,
}

/// Parse `route_json` array (or null/empty → empty vec).
pub fn parse_route(route_json: Option<&str>) -> Vec<RouteLeg> {
    let Some(raw) = route_json.filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .map(|leg| RouteLeg {
            pool: str_field(leg, "pool"),
            amm: str_field(leg, "amm"),
            token_in: str_field(leg, "token_in"),
            token_out: str_field(leg, "token_out"),
            token_source: str_field(leg, "token_source"),
            amount_in: str_field(leg, "amount_in"),
            amount_out: str_field(leg, "amount_out"),
        })
        .collect()
}

/// Parse victim hash list JSON.
pub fn parse_victim_hashes(raw: Option<&str>) -> Vec<String> {
    let Some(s) = raw.filter(|x| !x.is_empty()) else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<String>>(s).unwrap_or_default()
}

/// Build evidence from op fields + optional details JSON.
#[allow(clippy::too_many_arguments)]
pub fn build_evidence(
    tx_hash: &str,
    block_number: u64,
    kind: &str,
    subkind: Option<&str>,
    confidence: &str,
    detector: &str,
    eoa: &str,
    contract: Option<&str>,
    profit_usd: Option<f64>,
    net_profit_usd: Option<f64>,
    gas_cost_usd: Option<f64>,
    volume_usd: Option<f64>,
    flashloan_fee_usd: Option<f64>,
    profit_token: Option<&str>,
    profit_amount: Option<&str>,
    excluded_from_net: bool,
    route_json: Option<&str>,
    victim_hashes: Option<&str>,
    details_json: Option<&str>,
) -> OpEvidence {
    let details: Value = details_json
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Value::Object(Default::default()));
    let tags = string_array(&details, "tags");
    let reasons = string_array(&details, "reasons");
    let pnl_basis = details
        .get("pnl_basis")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let arb_meta = details.get("arb_meta").cloned();
    let profit_amount_human = profit_token
        .zip(profit_amount)
        .and_then(|(tok, amt)| humanize_amount(tok, amt));
    OpEvidence {
        tx_hash: tx_hash.to_string(),
        block_number,
        kind: kind.to_string(),
        subkind: subkind.map(str::to_string),
        confidence: confidence.to_string(),
        detector: detector.to_string(),
        eoa: eoa.to_string(),
        contract: contract.map(str::to_string),
        profit_usd,
        net_profit_usd,
        gas_cost_usd,
        volume_usd,
        flashloan_fee_usd,
        profit_token: profit_token.map(str::to_string),
        profit_amount: profit_amount.map(str::to_string),
        profit_amount_human,
        excluded_from_net,
        route: parse_route(route_json),
        victim_hashes: parse_victim_hashes(victim_hashes),
        tags,
        reasons,
        pnl_basis,
        arb_meta,
        details,
    }
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(str::to_string)
}

fn string_array(details: &Value, key: &str) -> Vec<String> {
    details
        .get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Decimals lookup keyed by lowercased address (and native sentinel).
fn decimals_map() -> &'static HashMap<String, u8> {
    static MAP: OnceLock<HashMap<String, u8>> = OnceLock::new();
    MAP.get_or_init(|| {
        let raw = include_str!("../../../data/known_tokens.json");
        let v: Value = serde_json::from_str(raw).unwrap_or(Value::Null);
        let mut m = HashMap::new();
        // Native wei sentinel used in explorer amounts.
        m.insert(
            "0xffffffffffffffffffffffffffffffffffffffff".to_string(),
            18,
        );
        if let Some(obj) = v.get("wrapped_native_by_chain").and_then(|x| x.as_object()) {
            for entry in obj.values() {
                if let (Some(addr), Some(dec)) = (
                    entry.get("address").and_then(|a| a.as_str()),
                    entry.get("decimals").and_then(|d| d.as_u64()),
                ) {
                    m.insert(addr.to_ascii_lowercase(), dec as u8);
                }
            }
        }
        if let Some(arr) = v.get("always_warm").and_then(|x| x.as_array()) {
            for entry in arr {
                if let (Some(addr), Some(dec)) = (
                    entry.get("address").and_then(|a| a.as_str()),
                    entry.get("decimals").and_then(|d| d.as_u64()),
                ) {
                    m.insert(addr.to_ascii_lowercase(), dec as u8);
                }
            }
        }
        m
    })
}

/// Humanize a raw U256 decimal string when decimals are known.
pub fn humanize_amount(token: &str, amount: &str) -> Option<String> {
    let dec = *decimals_map().get(&token.to_ascii_lowercase())?;
    let Ok(raw) = amount.parse::<f64>() else {
        return Some(format!("{amount} raw only (decimals unknown)"));
    };
    let scale = 10f64.powi(dec as i32);
    Some(format!("{:.6}", raw / scale))
}

/// Escape JSON for embedding inside `<script type="application/json">`.
///
/// `serde_json` does not escape `<`, so a payload containing `</script>` would
/// break the HTML document. Replace `<` with `\u003c`.
pub fn escape_json_for_script(json: &str) -> String {
    json.replace('<', "\\u003c")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_blocks_script_breakout() {
        let raw = r#"{"x":"</script><script>alert(1)"}"#;
        let esc = escape_json_for_script(raw);
        assert!(!esc.contains("</script>"));
        assert!(esc.contains("\\u003c/script>"));
    }

    #[test]
    fn parse_route_legs() {
        let raw = r#"[{"pool":"0xabc","amm":"v3","token_source":"registry"}]"#;
        let legs = parse_route(Some(raw));
        assert_eq!(legs.len(), 1);
        assert_eq!(legs[0].pool.as_deref(), Some("0xabc"));
        assert_eq!(legs[0].token_source.as_deref(), Some("registry"));
    }
}
