//! Snapshot → self-contained HTML (inline JSON + vendored ECharts + template).
use super::evidence::escape_json_for_script;
use super::DashboardSnapshot;

const TEMPLATE: &str = include_str!("../../../data/dashboard_template.html");
const ECHARTS: &str = include_str!("../../../data/echarts.min.js");

/// Embed snapshot JSON + ECharts into the HTML template.
pub fn to_html(snapshot: &DashboardSnapshot) -> anyhow::Result<String> {
    let json = serde_json::to_string(snapshot)?;
    let escaped = escape_json_for_script(&json);
    if !TEMPLATE.contains("/*__SNAPSHOT__*/") {
        anyhow::bail!("dashboard_template.html missing /*__SNAPSHOT__*/ placeholder");
    }
    if !TEMPLATE.contains("/*__ECHARTS__*/") {
        anyhow::bail!("dashboard_template.html missing /*__ECHARTS__*/ placeholder");
    }
    Ok(TEMPLATE
        .replace("/*__ECHARTS__*/", ECHARTS)
        .replace("/*__SNAPSHOT__*/", &escaped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::dashboard::evidence::escape_json_for_script;

    #[test]
    fn template_has_placeholder() {
        assert!(TEMPLATE.contains("/*__SNAPSHOT__*/"));
        assert!(TEMPLATE.contains("id=\"snapshot\""));
        assert!(TEMPLATE.contains("/*__ECHARTS__*/"));
    }

    #[test]
    fn template_is_multi_view_shell() {
        for id in [
            "nav",
            "view-overview",
            "view-analytics",
            "view-operations",
            "view-searchers",
            "view-pools",
            "view-health",
            "chart-overview-pulse",
            "chart-overview-mix",
            "chart-overview-competitors",
        ] {
            assert!(TEMPLATE.contains(&format!("id=\"{id}\"")), "missing #{id}");
        }
        assert!(TEMPLATE.contains("Data Health") || TEMPLATE.contains("data health"));
        assert!(TEMPLATE.contains("sliceKey"));
        assert!(TEMPLATE.contains("usdAxisValue") || TEMPLATE.contains("function usd("));
        assert!(!TEMPLATE.contains("id=\"filter-bar\""));
        assert!(!TEMPLATE.contains("id=\"f-slice\""));
        assert!(!TEMPLATE.contains("filteredScatter"));
        assert!(!TEMPLATE.contains("kpi-spark"));
        assert!(TEMPLATE.contains(">Overview<") || TEMPLATE.contains("overview: \"Overview\""));
        assert!(!TEMPLATE.contains("Network overview"));
        assert!(TEMPLATE.contains("op_index") || TEMPLATE.contains("indexComplete"));
        assert!(TEMPLATE.contains("chainIcon") || TEMPLATE.contains("chain-ico"));
        assert!(!TEMPLATE.contains("id=\"overview-ops\""));
        assert!(!TEMPLATE.contains("Live operations"));
        assert!(TEMPLATE.contains("amount_in_human") || TEMPLATE.contains("token_in_symbol"));
        assert!(!TEMPLATE.contains("fonts.googleapis"));
        assert!(!TEMPLATE.contains("cdn.jsdelivr"));
        assert!(!TEMPLATE.contains("Fraunces"));
        assert!(!TEMPLATE.contains("#F7F4EF"));
        assert!(ECHARTS.contains("echarts") || ECHARTS.len() > 100_000);
    }

    #[test]
    fn script_escape_roundtrip_safe() {
        let s = escape_json_for_script(r#"{"details":"</script>"}"#);
        assert!(!s.contains("</script>"));
    }
}
