//! Tests that CronCommands add variants accept agent fields.

#[cfg(test)]
mod tests {
    #[test]
    fn add_variant_accepts_agent_fields() {
        let _add = crate::CronCommands::Add {
            expression: "*/5 * * * *".into(),
            tz: None,
            command: "echo test".into(),
            job_type: "shell".into(),
            model: None,
            session_target: "isolated".into(),
            delivery_channel: None,
            delivery_to: None,
            name: None,
            context_files: Vec::new(),
        };
    }

    #[test]
    fn add_at_variant_accepts_agent_fields() {
        let _add_at = crate::CronCommands::AddAt {
            at: "2025-01-15T14:00:00Z".into(),
            command: "echo test".into(),
            job_type: "shell".into(),
            model: None,
            session_target: "isolated".into(),
            delivery_channel: None,
            delivery_to: None,
            name: None,
            context_files: Vec::new(),
        };
    }

    #[test]
    fn add_every_variant_accepts_agent_fields() {
        let _add_every = crate::CronCommands::AddEvery {
            every_ms: 60000,
            command: "echo test".into(),
            job_type: "shell".into(),
            model: None,
            session_target: "isolated".into(),
            delivery_channel: None,
            delivery_to: None,
            name: None,
            context_files: Vec::new(),
        };
    }

    #[test]
    fn once_variant_accepts_agent_fields() {
        let _once = crate::CronCommands::Once {
            delay: "30m".into(),
            command: "echo test".into(),
            job_type: "shell".into(),
            model: None,
            session_target: "isolated".into(),
            delivery_channel: None,
            delivery_to: None,
            name: None,
            context_files: Vec::new(),
        };
    }
}
