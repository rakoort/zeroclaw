pub mod client;
pub mod tools;

#[cfg(test)]
mod tests {
    use super::client::SlackClient;
    use super::tools::all_slack_tools;
    use std::sync::Arc;

    #[test]
    fn all_slack_tools_returns_9_tools() {
        let client = Arc::new(SlackClient::new("xoxb-test".into(), "xapp-test".into()));
        let tools = all_slack_tools(client);
        assert_eq!(tools.len(), 9);
    }

    #[test]
    fn all_slack_tools_have_valid_json_schemas() {
        let client = Arc::new(SlackClient::new("xoxb-test".into(), "xapp-test".into()));
        let tools = all_slack_tools(client);
        for tool in &tools {
            let schema = tool.parameters_schema();
            assert_eq!(
                schema["type"], "object",
                "Tool {} schema must be object",
                tool.name()
            );
            assert!(
                schema.get("properties").is_some(),
                "Tool {} must have properties",
                tool.name()
            );
        }
    }
}
