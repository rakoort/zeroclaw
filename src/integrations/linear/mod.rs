pub mod client;
pub mod tools;

#[cfg(test)]
mod tests {
    use super::client::LinearClient;
    use super::tools::all_linear_tools;
    use std::sync::Arc;

    #[test]
    fn all_linear_tools_returns_14_tools() {
        let client = Arc::new(LinearClient::new("lin_api_test".into()));
        let tools = all_linear_tools(client);
        assert_eq!(tools.len(), 14);
    }

    #[test]
    fn all_linear_tools_have_valid_json_schemas() {
        let client = Arc::new(LinearClient::new("lin_api_test".into()));
        let tools = all_linear_tools(client);
        for tool in &tools {
            let schema = tool.parameters_schema();
            assert_eq!(
                schema["type"], "object",
                "Tool {} schema must be object",
                tool.name()
            );
        }
    }
}
