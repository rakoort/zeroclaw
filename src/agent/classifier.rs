use crate::config::schema::{
    ClassificationMode, ClassificationTiers, ClassificationWeights, QueryClassificationConfig,
};

// ── Weighted scorer ──────────────────────────────────────────────

/// Tier score boundaries (applied to the weighted average in [-1, 1]).
const TIER_SIMPLE_MAX: f64 = -0.1;
const TIER_MEDIUM_MAX: f64 = 0.2;
const TIER_COMPLEX_MAX: f64 = 0.4;

/// Weighted multi-dimension scorer for content-based classification.
pub struct WeightedScorer<'a> {
    weights: &'a ClassificationWeights,
    tiers: &'a ClassificationTiers,
}

impl<'a> WeightedScorer<'a> {
    pub fn new(weights: &'a ClassificationWeights, tiers: &'a ClassificationTiers) -> Self {
        Self { weights, tiers }
    }

    /// Classify a message into a tier, return the corresponding hint string.
    pub fn classify(&self, message: &str, turn_count: usize) -> Option<String> {
        let score = self.score(message, turn_count);
        let tier_hint = if score < TIER_SIMPLE_MAX {
            &self.tiers.simple
        } else if score < TIER_MEDIUM_MAX {
            &self.tiers.medium
        } else if score < TIER_COMPLEX_MAX {
            &self.tiers.complex
        } else {
            &self.tiers.reasoning
        };
        tier_hint.clone()
    }

    /// Compute weighted score across all dimensions.
    fn score(&self, message: &str, turn_count: usize) -> f64 {
        let length = self.score_length(message);
        let code = self.score_code_density(message);
        let question = self.score_question_complexity(message);
        let depth = self.score_conversation_depth(turn_count);
        let tool = self.score_tool_hint(message);
        let domain = self.score_domain_specificity(message);

        let total = self.total_weight();
        (length * self.weights.length
            + code * self.weights.code_density
            + question * self.weights.question_complexity
            + depth * self.weights.conversation_depth
            + tool * self.weights.tool_hint
            + domain * self.weights.domain_specificity)
            / total
    }

    fn total_weight(&self) -> f64 {
        let sum = self.weights.length
            + self.weights.code_density
            + self.weights.question_complexity
            + self.weights.conversation_depth
            + self.weights.tool_hint
            + self.weights.domain_specificity;
        if sum <= 0.0 {
            1.0
        } else {
            sum
        }
    }

    /// Length: short messages pull toward simple, long messages toward complex.
    /// Range: [-1, 1]. Neutral point at ~200 chars.
    fn score_length(&self, message: &str) -> f64 {
        let len = message.len() as f64;
        // Sigmoid-like: 0 chars -> -0.5, 200 chars -> 0, 500+ chars -> ~0.8
        ((len - 200.0) / 300.0).clamp(-1.0, 1.0)
    }

    /// Code density: presence of code markers and fenced code blocks.
    /// No code markers -> 0 (neutral), high density -> 1.
    fn score_code_density(&self, message: &str) -> f64 {
        let inline_markers = [
            "fn ", "def ", "class ", "->", "=>", "import ", "use ", "pub ", "async ", "struct ",
            "impl ", "return ", "const ", "let ",
        ];
        let word_count = message.split_whitespace().count().max(1) as f64;
        let marker_count: f64 = inline_markers
            .iter()
            .map(|m| message.matches(m).count() as f64)
            .sum();
        // Fenced code blocks are strong signals.
        let fence_count = message.matches("```").count() as f64;
        let boosted = marker_count + fence_count * 3.0;

        if boosted == 0.0 {
            return 0.0; // no code signal -> neutral
        }
        // Scale: 2+ markers in a short message is a strong signal.
        let density = boosted / word_count;
        (density * 3.0).clamp(0.0, 1.0)
    }

    /// Question complexity: complex vs simple question words.
    /// No question words -> 0 (neutral).
    fn score_question_complexity(&self, message: &str) -> f64 {
        let lower = message.to_lowercase();
        let complex_words = [
            "explain",
            "compare",
            "contrast",
            "design",
            "architect",
            "analyze",
            "evaluate",
            "tradeoff",
            "trade-off",
            "recommend",
            "why",
            "how does",
            "what are the implications",
        ];
        let simple_words = ["what", "when", "where", "who", "yes", "no", "ok", "thanks"];

        let complex_hits: f64 = complex_words.iter().filter(|w| lower.contains(*w)).count() as f64;
        let simple_hits: f64 = simple_words.iter().filter(|w| lower.contains(*w)).count() as f64;

        let net = complex_hits - simple_hits * 0.5;
        (net / 3.0).clamp(-1.0, 1.0)
    }

    /// Conversation depth: deeper conversations trend toward complex.
    /// 0 turns -> -0.5, 10 turns -> 0.5, 20+ -> 1.0.
    fn score_conversation_depth(&self, turn_count: usize) -> f64 {
        let normalized = (turn_count as f64 / 10.0) - 0.5;
        normalized.clamp(-1.0, 1.0)
    }

    /// Tool hint: presence of tool/API/system words.
    /// No tool words -> 0 (neutral).
    fn score_tool_hint(&self, message: &str) -> f64 {
        let lower = message.to_lowercase();
        let tool_words = [
            "api", "endpoint", "curl", "http", "webhook", "command", "shell", "execute", "run",
            "deploy", "docker", "database", "query", "mutation", "graphql", "rest",
        ];
        let hits: f64 = tool_words.iter().filter(|w| lower.contains(*w)).count() as f64;
        if hits == 0.0 {
            return 0.0;
        }
        (hits / 3.0).clamp(0.0, 1.0)
    }

    /// Domain specificity: density of technical jargon.
    /// No domain words -> 0 (neutral).
    fn score_domain_specificity(&self, message: &str) -> f64 {
        let lower = message.to_lowercase();
        let domain_words = [
            "architecture",
            "microservice",
            "monolith",
            "scalab",
            "latency",
            "throughput",
            "consistency",
            "partition",
            "replication",
            "sharding",
            "caching",
            "load balanc",
            "circuit breaker",
            "backpressure",
            "idempoten",
        ];
        let hits: f64 = domain_words.iter().filter(|w| lower.contains(*w)).count() as f64;
        if hits == 0.0 {
            return 0.0;
        }
        let word_count = message.split_whitespace().count().max(1) as f64;
        let density = hits / word_count.sqrt();
        (density * 2.0).clamp(0.0, 1.0)
    }
}

// ── Classifier dispatch ──────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassificationDecision {
    pub hint: String,
    pub priority: i32,
}

/// Classify a user message against the configured rules and return the
/// matching hint string, if any.
///
/// Returns `None` when classification is disabled, no rules are configured,
/// or no rule matches the message.
pub fn classify(config: &QueryClassificationConfig, message: &str) -> Option<String> {
    classify_with_decision(config, message).map(|decision| decision.hint)
}

/// Classify a user message and return the matched hint together with
/// match metadata for observability.
///
/// Delegates to `classify_with_context` with `turn_count = 0`.
pub fn classify_with_decision(
    config: &QueryClassificationConfig,
    message: &str,
) -> Option<ClassificationDecision> {
    classify_with_context(config, message, 0)
}

/// Classify with conversation context (turn count for weighted mode).
pub fn classify_with_context(
    config: &QueryClassificationConfig,
    message: &str,
    turn_count: usize,
) -> Option<ClassificationDecision> {
    if !config.enabled {
        return None;
    }

    match config.mode {
        ClassificationMode::Weighted => {
            let scorer = WeightedScorer::new(&config.weights, &config.tiers);
            scorer
                .classify(message, turn_count)
                .map(|hint| ClassificationDecision { hint, priority: 0 })
        }
        ClassificationMode::Rules => classify_rules(config, message),
    }
}

/// Rule-based classification logic (extracted helper).
fn classify_rules(
    config: &QueryClassificationConfig,
    message: &str,
) -> Option<ClassificationDecision> {
    if config.rules.is_empty() {
        return None;
    }

    let lower = message.to_lowercase();
    let len = message.len();

    let mut rules: Vec<_> = config.rules.iter().collect();
    rules.sort_by(|a, b| b.priority.cmp(&a.priority));

    for rule in rules {
        // Length constraints
        if let Some(min) = rule.min_length {
            if len < min {
                continue;
            }
        }
        if let Some(max) = rule.max_length {
            if len > max {
                continue;
            }
        }

        // Check keywords (case-insensitive) and patterns (case-sensitive)
        let keyword_hit = rule
            .keywords
            .iter()
            .any(|kw: &String| lower.contains(&kw.to_lowercase()));
        let pattern_hit = rule
            .patterns
            .iter()
            .any(|pat: &String| message.contains(pat.as_str()));

        if keyword_hit || pattern_hit {
            return Some(ClassificationDecision {
                hint: rule.hint.clone(),
                priority: rule.priority,
            });
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::{
        ClassificationMode, ClassificationRule, ClassificationTiers, ClassificationWeights,
        QueryClassificationConfig,
    };

    fn make_config(enabled: bool, rules: Vec<ClassificationRule>) -> QueryClassificationConfig {
        QueryClassificationConfig {
            enabled,
            rules,
            ..Default::default()
        }
    }

    #[test]
    fn disabled_returns_none() {
        let config = make_config(
            false,
            vec![ClassificationRule {
                hint: "fast".into(),
                keywords: vec!["hello".into()],
                ..Default::default()
            }],
        );
        assert_eq!(classify(&config, "hello"), None);
    }

    #[test]
    fn empty_rules_returns_none() {
        let config = make_config(true, vec![]);
        assert_eq!(classify(&config, "hello"), None);
    }

    #[test]
    fn keyword_match_case_insensitive() {
        let config = make_config(
            true,
            vec![ClassificationRule {
                hint: "fast".into(),
                keywords: vec!["hello".into()],
                ..Default::default()
            }],
        );
        assert_eq!(classify(&config, "HELLO world"), Some("fast".into()));
    }

    #[test]
    fn pattern_match_case_sensitive() {
        let config = make_config(
            true,
            vec![ClassificationRule {
                hint: "code".into(),
                patterns: vec!["fn ".into()],
                ..Default::default()
            }],
        );
        assert_eq!(classify(&config, "fn main()"), Some("code".into()));
        assert_eq!(classify(&config, "FN MAIN()"), None);
    }

    #[test]
    fn length_constraints() {
        let config = make_config(
            true,
            vec![ClassificationRule {
                hint: "fast".into(),
                keywords: vec!["hi".into()],
                max_length: Some(10),
                ..Default::default()
            }],
        );
        assert_eq!(classify(&config, "hi"), Some("fast".into()));
        assert_eq!(
            classify(&config, "hi there, how are you doing today?"),
            None
        );

        let config2 = make_config(
            true,
            vec![ClassificationRule {
                hint: "reasoning".into(),
                keywords: vec!["explain".into()],
                min_length: Some(20),
                ..Default::default()
            }],
        );
        assert_eq!(classify(&config2, "explain"), None);
        assert_eq!(
            classify(&config2, "explain how this works in detail"),
            Some("reasoning".into())
        );
    }

    #[test]
    fn priority_ordering() {
        let config = make_config(
            true,
            vec![
                ClassificationRule {
                    hint: "fast".into(),
                    keywords: vec!["code".into()],
                    priority: 1,
                    ..Default::default()
                },
                ClassificationRule {
                    hint: "code".into(),
                    keywords: vec!["code".into()],
                    priority: 10,
                    ..Default::default()
                },
            ],
        );
        assert_eq!(classify(&config, "write some code"), Some("code".into()));
    }

    #[test]
    fn no_match_returns_none() {
        let config = make_config(
            true,
            vec![ClassificationRule {
                hint: "fast".into(),
                keywords: vec!["hello".into()],
                ..Default::default()
            }],
        );
        assert_eq!(classify(&config, "something completely different"), None);
    }

    #[test]
    fn classify_with_decision_exposes_priority_of_matched_rule() {
        let config = make_config(
            true,
            vec![
                ClassificationRule {
                    hint: "fast".into(),
                    keywords: vec!["code".into()],
                    priority: 3,
                    ..Default::default()
                },
                ClassificationRule {
                    hint: "code".into(),
                    keywords: vec!["code".into()],
                    priority: 10,
                    ..Default::default()
                },
            ],
        );

        let decision = classify_with_decision(&config, "write code now")
            .expect("classification decision expected");
        assert_eq!(decision.hint, "code");
        assert_eq!(decision.priority, 10);
    }

    #[test]
    fn weighted_scorer_short_simple_message() {
        let weights = ClassificationWeights::default();
        let tiers = ClassificationTiers {
            simple: Some("hint:simple".into()),
            medium: Some("hint:medium".into()),
            complex: Some("hint:complex".into()),
            reasoning: Some("hint:reasoning".into()),
        };
        let scorer = WeightedScorer::new(&weights, &tiers);
        let result = scorer.classify("hi", 0);
        assert_eq!(result, Some("hint:simple".to_string()));
    }

    #[test]
    fn weighted_scorer_code_heavy_message() {
        let weights = ClassificationWeights::default();
        let tiers = ClassificationTiers {
            simple: Some("hint:simple".into()),
            medium: Some("hint:medium".into()),
            complex: Some("hint:complex".into()),
            reasoning: Some("hint:reasoning".into()),
        };
        let scorer = WeightedScorer::new(&weights, &tiers);
        let msg = "```rust\nfn main() {\n    let x = 42;\n    println!(\"{x}\");\n}\n```\nPlease explain this code and refactor it for better error handling";
        let result = scorer.classify(msg, 5);
        assert!(result == Some("hint:complex".into()) || result == Some("hint:reasoning".into()));
    }

    #[test]
    fn weighted_scorer_medium_question() {
        let weights = ClassificationWeights::default();
        let tiers = ClassificationTiers {
            simple: Some("hint:simple".into()),
            medium: Some("hint:medium".into()),
            complex: Some("hint:complex".into()),
            reasoning: Some("hint:reasoning".into()),
        };
        let scorer = WeightedScorer::new(&weights, &tiers);
        let result = scorer.classify("What's the weather like in London today?", 2);
        assert!(result == Some("hint:simple".into()) || result == Some("hint:medium".into()));
    }

    #[test]
    fn weighted_scorer_reasoning_message() {
        let weights = ClassificationWeights::default();
        let tiers = ClassificationTiers {
            simple: Some("hint:simple".into()),
            medium: Some("hint:medium".into()),
            complex: Some("hint:complex".into()),
            reasoning: Some("hint:reasoning".into()),
        };
        let scorer = WeightedScorer::new(&weights, &tiers);
        let msg = "Compare and contrast the tradeoffs between using a microservices architecture versus a monolithic architecture for a high-traffic e-commerce platform. Consider scalability, deployment complexity, data consistency, and team organization. Explain which approach you would recommend and why, with specific design patterns for each critical subsystem.";
        let result = scorer.classify(msg, 15);
        assert_eq!(result, Some("hint:reasoning".to_string()));
    }

    #[test]
    fn weighted_scorer_no_tiers_returns_none() {
        let weights = ClassificationWeights::default();
        let tiers = ClassificationTiers::default(); // all None
        let scorer = WeightedScorer::new(&weights, &tiers);
        let result = scorer.classify("hello", 0);
        assert_eq!(result, None);
    }

    #[test]
    fn classify_weighted_mode_returns_tier_hint() {
        let config = QueryClassificationConfig {
            enabled: true,
            mode: ClassificationMode::Weighted,
            rules: vec![],
            tiers: ClassificationTiers {
                simple: Some("hint:simple".into()),
                medium: Some("hint:medium".into()),
                complex: Some("hint:complex".into()),
                reasoning: Some("hint:reasoning".into()),
            },
            weights: ClassificationWeights::default(),
            ..Default::default()
        };
        let result = classify(&config, "hi");
        assert_eq!(result, Some("hint:simple".to_string()));
    }

    #[test]
    fn classify_rules_mode_still_works() {
        let config = QueryClassificationConfig {
            enabled: true,
            mode: ClassificationMode::Rules,
            rules: vec![ClassificationRule {
                hint: "fast".into(),
                keywords: vec!["hello".into()],
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(classify(&config, "hello"), Some("fast".into()));
    }

    #[test]
    fn classify_with_context_weighted_dispatches() {
        let config = QueryClassificationConfig {
            enabled: true,
            mode: ClassificationMode::Weighted,
            rules: vec![],
            tiers: ClassificationTiers {
                simple: Some("hint:simple".into()),
                medium: Some("hint:medium".into()),
                complex: Some("hint:complex".into()),
                reasoning: Some("hint:reasoning".into()),
            },
            weights: ClassificationWeights::default(),
            ..Default::default()
        };
        let decision = classify_with_context(&config, "hi", 0)
            .expect("weighted classification should return a decision");
        assert_eq!(decision.hint, "hint:simple");
        assert_eq!(decision.priority, 0);
    }

    #[test]
    fn classify_with_context_rules_dispatches() {
        let config = QueryClassificationConfig {
            enabled: true,
            mode: ClassificationMode::Rules,
            rules: vec![ClassificationRule {
                hint: "fast".into(),
                keywords: vec!["hello".into()],
                priority: 5,
                ..Default::default()
            }],
            ..Default::default()
        };
        let decision = classify_with_context(&config, "hello", 0)
            .expect("rules classification should return a decision");
        assert_eq!(decision.hint, "fast");
        assert_eq!(decision.priority, 5);
    }

    #[test]
    fn classify_with_context_disabled_returns_none() {
        let config = QueryClassificationConfig {
            enabled: false,
            mode: ClassificationMode::Weighted,
            tiers: ClassificationTiers {
                simple: Some("hint:simple".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(classify_with_context(&config, "hi", 0), None);
    }

    /// Integration test: exercises the full classification routing stack
    /// (weighted mode, rules mode, and disabled mode) in a single flow.
    /// This is a post-implementation regression test for Tasks 7-10;
    /// no new production code is introduced.
    #[test]
    fn full_weighted_classification_flow() {
        let config = QueryClassificationConfig {
            enabled: true,
            mode: ClassificationMode::Weighted,
            rules: vec![],
            tiers: ClassificationTiers {
                simple: Some("hint:simple".into()),
                medium: Some("hint:medium".into()),
                complex: Some("hint:complex".into()),
                reasoning: Some("hint:reasoning".into()),
            },
            weights: ClassificationWeights::default(),
            ..Default::default()
        };

        // Short greeting → simple
        assert_eq!(classify(&config, "hi"), Some("hint:simple".into()));

        // Long complex reasoning request → reasoning or complex
        let reasoning_msg = "Compare and contrast the tradeoffs between microservices and monolithic \
            architecture for a high-traffic e-commerce platform. Consider scalability, deployment \
            complexity, data consistency, team organization, and latency implications. Explain your \
            recommendation with specific design patterns.";
        let result = classify_with_context(&config, reasoning_msg, 15);
        assert!(
            result.as_ref().map(|d| d.hint.as_str()) == Some("hint:reasoning")
                || result.as_ref().map(|d| d.hint.as_str()) == Some("hint:complex"),
            "Long reasoning request should be complex or reasoning tier, got: {:?}",
            result
        );

        // Verify rules mode still works alongside weighted
        let rules_config = QueryClassificationConfig {
            enabled: true,
            mode: ClassificationMode::Rules,
            rules: vec![ClassificationRule {
                hint: "fast".into(),
                keywords: vec!["hello".into()],
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(classify(&rules_config, "hello world"), Some("fast".into()));

        // Verify disabled returns None in both modes
        let disabled_weighted = QueryClassificationConfig {
            enabled: false,
            mode: ClassificationMode::Weighted,
            ..Default::default()
        };
        assert_eq!(classify(&disabled_weighted, "anything"), None);
    }
}
