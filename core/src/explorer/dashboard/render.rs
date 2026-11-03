//! Snapshot → self-contained HTML (inline JSON + template).
use super::evidence::escape_json_for_script;
use super::DashboardSnapshot;

const TEMPLATE: &str = include_str!("../../../data/dashboard_template.html");

/// Embed snapshot JSON into the HTML template.
pub fn to_html(snapshot: &DashboardSnapshot) -> anyhow::Result<String> {
    let json = serde_json::to_string(snapshot)?;
    let escaped = escape_json_for_script(&json);
    if !TEMPLATE.contains("/*__SNAPSHOT__*/") {
        anyhow::bail!("dashboard_template.html missing /*__SNAPSHOT__*/ placeholder");
    }
    Ok(TEMPLATE.replace("/*__SNAPSHOT__*/", &escaped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::dashboard::evidence::escape_json_for_script;

    #[test]
    fn template_has_placeholder() {
        assert!(TEMPLATE.contains("/*__SNAPSHOT__*/"));
        assert!(TEMPLATE.contains("id=\"snapshot\""));
    }

    #[test]
    fn script_escape_roundtrip_safe() {
        let s = escape_json_for_script(r#"{"details":"</script>"}"#);
        assert!(!s.contains("</script>"));
    }
}
