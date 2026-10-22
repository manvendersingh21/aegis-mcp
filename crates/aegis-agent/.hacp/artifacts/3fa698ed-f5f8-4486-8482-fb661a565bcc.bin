use aegis_agent::{scenario_script, McpClient, Scenarios};
use aegis_core::ScenarioRunner;

#[test]
fn scenario_one_is_greedy() {
    let s = scenario_script(1);
    assert!(s.len() >= 3);
    assert_eq!(s[0].agent_id, "acme_travel_broker_v2");
    let fin = s.iter().find(|c| c.tool == "query_financials").expect("financials");
    assert_eq!(fin.arguments["include_raw_transactions"], true);
    assert!(fin.arguments["fields"].to_string().contains("ssn"));
    assert!(s.iter().any(|c| c.tool == "check_schedule"));
}

#[test]
fn scenario_two_is_injection() {
    let s = scenario_script(2);
    assert!(s.iter().any(|c| c.tool == "export_contacts"));
    assert!(s.iter().any(|c| c.purpose.to_lowercase().contains("ignore previous instructions")));
}

#[test]
fn scenario_three_exists() {
    assert!(!scenario_script(3).is_empty());
}

#[tokio::test]
async fn runner_survives_dead_gateway() {
    // nothing listens on port 9; run must finish without panicking
    let r = Scenarios::new(None);
    tokio::time::timeout(std::time::Duration::from_secs(20), r.run(1, "maya".into(), "http://127.0.0.1:9".into())).await.expect("finishes");
    let c = McpClient::new("http://127.0.0.1:9", "maya", "a", "p");
    assert!(c.list_tools().await.is_err());
}
