//! Typed parse of `details_json` / `route_json` plus known-token decimals/symbols.
use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::OpRecord;

/// Reconciled recorded execution economics; not a return on borrowed capital.
/// Display USD remains available on the event/evidence even when this cohort is unavailable.
#[derive(Debug, Clone, Serialize, Default)]
pub struct OperationEconomics {
    pub eligible: bool,
    pub reason: Option<String>,
    pub gross_usd: Option<f64>,
    pub cost_usd: Option<f64>,
    pub net_usd: Option<f64>,
    pub gas_usd: Option<f64>,
    pub flash_fee_usd: Option<f64>,
    pub flash_fee_netted: bool,
    pub roi_pct: Option<f64>,
}

impl OperationEconomics {
    pub fn from_record(op: &OpRecord) -> Self {
        let details = parse_details(op.details_json.as_deref());
        Self::with_details(op, &details)
    }

    fn with_details(op: &OpRecord, details: &Value) -> Self {
        let netted = details.get("flashloan_fee_netted_in_ledger").and_then(Value::as_bool) == Some(true);
        let mut result = Self {
            gross_usd: finite(op.profit_usd),
            net_usd: finite(op.net_profit_usd),
            gas_usd: finite(op.gas_cost_usd),
            flash_fee_usd: finite(op.flashloan_fee_usd),
            flash_fee_netted: netted,
            ..Self::default()
        };
        match complete_economics(op, details, netted) {
            Ok((gross, gas, fee, net)) => {
                let cost = gas + fee;
                result.eligible = true;
                result.gross_usd = Some(gross);
                result.cost_usd = Some(cost);
                result.gas_usd = Some(gas);
                result.flash_fee_usd = Some(fee);
                result.net_usd = Some(net);
                result.roi_pct = cost_ratio(net, cost, 100.0);
            }
            Err(reason) => result.reason = Some(reason.to_string()),
        }
        result
    }
}

fn finite(value: Option<f64>) -> Option<f64> {
    value.filter(|v| v.is_finite())
}

pub(super) fn cost_ratio(value: f64, cost: f64, scale: f64) -> Option<f64> {
    (cost.is_finite() && cost > 0.0 && value.is_finite())
        .then(|| value / cost * scale)
        .filter(|v| v.is_finite())
}

fn reconciles(a: f64, b: f64) -> bool {
    // USD storage is f64; allow rounding, never a material accounting difference.
    (a - b).abs() <= 1e-8 + 1e-9 * a.abs().max(b.abs())
}

fn detail_issue(details: &Value, key: &str) -> bool {
    details.get(key).and_then(Value::as_bool) == Some(true)
        || ["tags", "reasons", "pricing_issues"].iter().any(|field| {
            details.get(field).and_then(Value::as_array).is_some_and(|values| {
                values.iter().filter_map(Value::as_str).any(|v| v.eq_ignore_ascii_case(key))
            })
        })
}

fn complete_economics(
    op: &OpRecord,
    details: &Value,
    netted: bool,
) -> Result<(f64, f64, f64, f64), &'static str> {
    if details.get("pnl_basis").and_then(Value::as_str) == Some("gas_cost_as_profit")
        || details.pointer("/pnl/basis").and_then(Value::as_str) == Some("gas_cost_as_profit")
    {
        return Err("gas_cost_as_profit");
    }
    let jit = matches!(op.kind.as_str(), "jit" | "jit_arb");
    if jit && details.pointer("/jit_accounting/status").and_then(Value::as_str) != Some("verified") {
        return Err("jit_unverified");
    }
    if jit && details.pointer("/jit_accounting/valuation/pricing_complete").and_then(Value::as_bool) != Some(true) {
        return Err("jit_pricing_incomplete");
    }
    let flags = [
        (op.usd_approximate, "usd_approximate"),
        (op.native_unpriced, "native_unpriced"),
        (op.multi_asset_pricing, "multi_asset_pricing"),
        (op.pricing_clamped, "pricing_clamped"),
        (op.flash_fee_unpriced, "flash_fee_unpriced"),
    ];
    for (flag, name) in flags {
        if flag == Some(true) || detail_issue(details, name) {
            return Err(name);
        }
    }
    if details.get("approximate_reason").is_some_and(|v| !v.is_null()) {
        return Err("usd_approximate");
    }
    if !jit && flags.iter().any(|(flag, name)| {
        flag.is_none() && details.get(name).and_then(Value::as_bool) != Some(false)
    }) {
        return Err("legacy_pricing_flags_unknown");
    }
    let profit = finite(op.profit_usd).ok_or("gross_usd_unavailable")?;
    let gas = finite(op.gas_cost_usd).ok_or("gas_usd_unavailable")?;
    let net = finite(op.net_profit_usd).ok_or("net_usd_unavailable")?;
    if gas < 0.0 {
        return Err("negative_execution_cost");
    }
    if jit {
        let valuation = &details["jit_accounting"]["valuation"];
        for (key, stored) in [("gross_usd", profit), ("gas_usd", gas), ("net_profit_usd", net)] {
            let quoted = finite(valuation.get(key).and_then(Value::as_f64)).ok_or("jit_valuation_unavailable")?;
            if !reconciles(quoted, stored) {
                return Err("jit_valuation_mismatch");
            }
        }
    }
    let fee = financing_cost(op, details, netted)?;
    let gross = if netted { profit + fee } else { profit };
    let cost = gas + fee;
    if !gross.is_finite() || !cost.is_finite() {
        return Err("nonfinite_execution_economics");
    }
    if !reconciles(gross - cost, net) {
        return Err("gross_cost_net_mismatch");
    }
    Ok((gross, gas, fee, net))
}

fn financing_cost(op: &OpRecord, details: &Value, netted: bool) -> Result<f64, &'static str> {
    // Older classifiers stored a primary/max fee, not necessarily a sum over all loans.
    if ["flash_providers", "flashloans"].iter().any(|key| {
        details.get(key).and_then(Value::as_array).is_some_and(|v| v.len() > 1)
    }) {
        return Err("legacy_multiple_flash_fees_unknown");
    }
    let raw_fee = ["flash_premium", "flashloan_fee_wei", "flash_fee_wei"]
        .iter().find_map(|key| details.get(key).filter(|v| !v.is_null()));
    let raw_zero = raw_fee.is_some_and(|v| {
        v.as_u64() == Some(0) || v.as_str().is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b == b'0'))
    });
    if let Some(fee) = op.flashloan_fee_usd {
        if !fee.is_finite() || fee < 0.0 {
            return Err("flash_fee_usd_invalid");
        }
        if fee == 0.0 && raw_fee.is_some() && !raw_zero {
            return Err("flash_fee_usd_unavailable");
        }
        return Ok(fee);
    }
    if raw_zero {
        return Ok(0.0);
    }
    let flash_present = netted || raw_fee.is_some()
        || ["flash_arb", "flash_liquidation", "flash_funded", "flashloan_funded"].iter().any(|key| detail_issue(details, key))
        || details.pointer("/arb_meta/flashloan_funded").and_then(Value::as_bool) == Some(true)
        || ["flash_provider", "flash_provider_address", "flashloan_fee_token"].iter().any(|key| {
            details.get(key).is_some_and(|v| !v.is_null())
        })
        || ["flashloans", "flash_providers"].iter().any(|key| {
            details.get(key).and_then(Value::as_array).is_some_and(|v| !v.is_empty())
        });
    if flash_present {
        return Err("flash_fee_usd_unavailable");
    }
    // Modern persisted flags plus object-shaped details and no loan markers mean
    // no recorded financing cost. Missing legacy evidence must not become $0.
    if details.is_object() && op.flash_fee_unpriced == Some(false) {
        Ok(0.0)
    } else {
        Err("legacy_flash_fee_unknown")
    }
}

fn parse_details(raw: Option<&str>) -> Value {
    raw.and_then(|s| serde_json::from_str(s).ok()).unwrap_or(Value::Null)
}

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
    pub amount_in_human: Option<String>,
    pub amount_out_human: Option<String>,
    pub token_in_symbol: Option<String>,
    pub token_out_symbol: Option<String>,
    /// `known_tokens.json` has no `name` field today — always `None` unless extended later.
    pub token_in_name: Option<String>,
    pub token_out_name: Option<String>,
}

/// Sectioned evidence blob for one op (P10 drill-down).
#[derive(Debug, Clone, Serialize, Default)]
pub struct OpEvidence {
    pub id: i64,
    pub tx_index: Option<u64>,
    pub ts: u64,
    pub created_at: u64,
    pub economics: OperationEconomics,
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
    pub usd_approximate: bool,
    pub native_unpriced: bool,
    pub multi_asset_pricing: bool,
    pub pricing_clamped: bool,
    pub flash_fee_unpriced: bool,
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
        .map(|leg| {
            let token_in = str_field(leg, "token_in");
            let token_out = str_field(leg, "token_out");
            let amount_in = str_field(leg, "amount_in");
            let amount_out = str_field(leg, "amount_out");
            let (token_in_symbol, token_in_name) = token_in
                .as_deref()
                .map(token_meta)
                .unwrap_or((None, None));
            let (token_out_symbol, token_out_name) = token_out
                .as_deref()
                .map(token_meta)
                .unwrap_or((None, None));
            let amount_in_human = token_in
                .as_deref()
                .zip(amount_in.as_deref())
                .and_then(|(tok, amt)| humanize_amount(tok, amt));
            let amount_out_human = token_out
                .as_deref()
                .zip(amount_out.as_deref())
                .and_then(|(tok, amt)| humanize_amount(tok, amt));
            RouteLeg {
                pool: str_field(leg, "pool"),
                amm: str_field(leg, "amm"),
                token_in,
                token_out,
                token_source: str_field(leg, "token_source"),
                amount_in,
                amount_out,
                amount_in_human,
                amount_out_human,
                token_in_symbol,
                token_out_symbol,
                token_in_name,
                token_out_name,
            }
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

/// Build evidence and its reconciled economics from the same stored operation.
pub fn build_evidence(op: &OpRecord) -> OpEvidence {
    let parsed = parse_details(op.details_json.as_deref());
    let economics = OperationEconomics::with_details(op, &parsed);
    let details = if parsed.is_null() { Value::Object(Default::default()) } else { parsed };
    let tags = string_array(&details, "tags");
    let reasons = string_array(&details, "reasons");
    let pnl_basis = details
        .get("pnl_basis")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let arb_meta = details.get("arb_meta").cloned();
    let profit_amount_human = op.profit_token.as_deref()
        .zip(op.profit_amount.as_deref())
        .and_then(|(tok, amt)| humanize_amount(tok, amt));
    OpEvidence {
        id: op.id,
        tx_index: op.tx_index,
        ts: op.ts,
        created_at: op.created_at,
        economics,
        tx_hash: op.tx_hash.clone(),
        block_number: op.block_number,
        kind: op.kind.clone(),
        subkind: op.subkind.clone(),
        confidence: op.confidence.clone(),
        detector: op.detector.clone(),
        eoa: op.eoa.clone(),
        contract: op.contract.clone(),
        profit_usd: op.profit_usd,
        net_profit_usd: op.net_profit_usd,
        gas_cost_usd: op.gas_cost_usd,
        volume_usd: op.volume_usd,
        flashloan_fee_usd: op.flashloan_fee_usd,
        profit_token: op.profit_token.clone(),
        profit_amount: op.profit_amount.clone(),
        profit_amount_human,
        excluded_from_net: op.excluded_from_net.unwrap_or(false),
        usd_approximate: op.usd_approximate.unwrap_or(false),
        native_unpriced: op.native_unpriced.unwrap_or(false),
        multi_asset_pricing: op.multi_asset_pricing.unwrap_or(false),
        pricing_clamped: op.pricing_clamped.unwrap_or(false),
        flash_fee_unpriced: op.flash_fee_unpriced.unwrap_or(false),
        route: parse_route(op.route_json.as_deref()),
        victim_hashes: parse_victim_hashes(op.victim_hashes.as_deref()),
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

struct TokenEntry {
    decimals: u8,
    symbol: String,
}

fn token_registry() -> &'static HashMap<String, TokenEntry> {
    static MAP: OnceLock<HashMap<String, TokenEntry>> = OnceLock::new();
    MAP.get_or_init(|| {
        let raw = include_str!("../../../data/known_tokens.json");
        let v: Value = serde_json::from_str(raw).unwrap_or(Value::Null);
        let mut m = HashMap::new();
        // Native wei sentinel used in explorer amounts.
        m.insert(
            "0xffffffffffffffffffffffffffffffffffffffff".to_string(),
            TokenEntry {
                decimals: 18,
                symbol: "NATIVE".to_string(),
            },
        );
        let mut insert = |entry: &Value| {
            let Some(addr) = entry.get("address").and_then(|a| a.as_str()) else {
                return;
            };
            let Some(dec) = entry.get("decimals").and_then(|d| d.as_u64()) else {
                return;
            };
            let symbol = entry
                .get("symbol")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            m.insert(
                addr.to_ascii_lowercase(),
                TokenEntry {
                    decimals: dec as u8,
                    symbol,
                },
            );
        };
        if let Some(obj) = v.get("wrapped_native_by_chain").and_then(|x| x.as_object()) {
            for entry in obj.values() {
                insert(entry);
            }
        }
        if let Some(arr) = v.get("always_warm").and_then(|x| x.as_array()) {
            for entry in arr {
                insert(entry);
            }
        }
        m
    })
}

/// `(symbol, name)` — name is always `None` until `known_tokens.json` grows a name field.
pub fn token_meta(token: &str) -> (Option<String>, Option<String>) {
    match token_registry().get(&token.to_ascii_lowercase()) {
        Some(e) if !e.symbol.is_empty() => (Some(e.symbol.clone()), None),
        _ => (None, None),
    }
}

/// Humanize a raw U256 decimal string when decimals are known.
pub fn humanize_amount(token: &str, amount: &str) -> Option<String> {
    let dec = token_registry()
        .get(&token.to_ascii_lowercase())
        .map(|e| e.decimals)?;
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

    #[test]
    fn known_token_humanize_and_symbol() {
        // Avalanche WAVAX
        let wavax = "0xB31f66AA3C1e785363F0875A1B74E27b85FD66c7";
        let human = humanize_amount(wavax, "1000000000000000000").expect("decimals");
        assert_eq!(human, "1.000000");
        let (sym, name) = token_meta(wavax);
        assert_eq!(sym.as_deref(), Some("WAVAX"));
        assert!(name.is_none());
    }

    #[test]
    fn unknown_token_fallback() {
        let unk = "0x00000000000000000000000000000000000000aa";
        assert!(humanize_amount(unk, "1000").is_none());
        let (sym, name) = token_meta(unk);
        assert!(sym.is_none());
        assert!(name.is_none());
    }

    #[test]
    fn parse_route_enriches_known_amounts() {
        let wavax = "0xB31f66AA3C1e785363F0875A1B74E27b85FD66c7";
        let usdc = "0xB97EF9Ef8734C71904D8002F8b6Bc66Dd9c48a6E";
        let raw = format!(
            r#"[{{"pool":"0xpool","amm":"v3","token_in":"{wavax}","token_out":"{usdc}","amount_in":"2000000000000000000","amount_out":"3000000"}}]"#
        );
        let legs = parse_route(Some(&raw));
        assert_eq!(legs[0].amount_in_human.as_deref(), Some("2.000000"));
        assert_eq!(legs[0].amount_out_human.as_deref(), Some("3.000000"));
        assert_eq!(legs[0].token_in_symbol.as_deref(), Some("WAVAX"));
        assert_eq!(legs[0].token_out_symbol.as_deref(), Some("USDC.e"));
    }

    #[test]
    fn native_sentinel_humanizes() {
        let native = "0xffffffffffffffffffffffffffffffffffffffff";
        assert_eq!(
            humanize_amount(native, "1000000000000000000").as_deref(),
            Some("1.000000")
        );
    }
}
