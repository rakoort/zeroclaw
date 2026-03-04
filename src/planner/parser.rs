use super::types::Plan;
use anyhow::{bail, Result};

/// Parse a JSON string into a Plan.
pub fn parse_plan(json: &str) -> Result<Plan> {
    serde_json::from_str(json).map_err(|e| anyhow::anyhow!("Plan parse error: {e}"))
}

/// Extract JSON from an LLM response that may contain markdown fences.
pub fn parse_plan_from_response(response: &str) -> Result<Plan> {
    // Try direct parse first
    if let Ok(plan) = parse_plan(response.trim()) {
        return Ok(plan);
    }
    // Try extracting from ```json ... ``` fences
    if let Some(start) = response.find("```json") {
        let json_start = start + 7;
        if let Some(end) = response[json_start..].find("```") {
            return parse_plan(response[json_start..json_start + end].trim());
        }
    }
    // Try extracting from ``` ... ``` fences (without json tag)
    if let Some(start) = response.find("```") {
        let json_start = start + 3;
        if let Some(end) = response[json_start..].find("```") {
            let candidate = response[json_start..json_start + end].trim();
            if candidate.starts_with('{') {
                return parse_plan(candidate);
            }
        }
    }
    bail!("Could not extract plan JSON from response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_direct_json() {
        let json = r#"{"passthrough": true}"#;
        let plan = parse_plan(json).unwrap();
        assert!(plan.is_passthrough());
    }

    #[test]
    fn parse_from_fenced_json() {
        let response = "Here is the plan:\n```json\n{\"passthrough\": false, \"actions\": [{\"type\": \"read\", \"description\": \"read data\"}]}\n```";
        let plan = parse_plan_from_response(response).unwrap();
        assert!(!plan.is_passthrough());
        assert_eq!(plan.actions.len(), 1);
    }

    #[test]
    fn parse_from_bare_fences() {
        let response = "```\n{\"passthrough\": true}\n```";
        let plan = parse_plan_from_response(response).unwrap();
        assert!(plan.is_passthrough());
    }

    #[test]
    fn parse_with_analysis_field() {
        let json = r#"{"analysis": "multi-step task", "passthrough": false, "actions": [{"type": "a", "description": "do a"}]}"#;
        let plan = parse_plan(json).unwrap();
        assert_eq!(plan.analysis.as_deref(), Some("multi-step task"));
    }

    #[test]
    fn parse_invalid_json_fails() {
        let result = parse_plan_from_response("not json at all");
        assert!(result.is_err());
    }
}
