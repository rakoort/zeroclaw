use crate::config::schema::{
    ClassificationMode, ClassificationTiers, QueryClassificationConfig, ScoringConfig,
    ScoringOverrides, Tier,
};

// ── 14-dimension scorer ─────────────────────────────────────────

/// Single dimension result from the 14-dimension scoring pipeline.
struct DimensionScore {
    name: &'static str,
    score: f64,
    signal: Option<String>,
}

/// Match keywords in text and return a dimension score.
///
/// `low_threshold` / `high_threshold` control how many keyword matches
/// trigger low vs high scores.
fn score_keyword_match(
    text: &str,
    keywords: &[&str],
    name: &'static str,
    label: &str,
    low_threshold: usize,
    high_threshold: usize,
    low_score: f64,
    high_score: f64,
) -> DimensionScore {
    let lower = text.to_lowercase();
    let matches: Vec<&&str> = keywords
        .iter()
        .filter(|kw| lower.contains(&kw.to_lowercase()))
        .collect();
    if matches.len() >= high_threshold {
        DimensionScore {
            name,
            score: high_score,
            signal: Some(format!(
                "{label} ({})",
                matches
                    .iter()
                    .take(3)
                    .map(|s| **s)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    } else if matches.len() >= low_threshold {
        DimensionScore {
            name,
            score: low_score,
            signal: Some(format!(
                "{label} ({})",
                matches
                    .iter()
                    .take(3)
                    .map(|s| **s)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    } else {
        DimensionScore {
            name,
            score: 0.0,
            signal: None,
        }
    }
}

fn score_token_count(estimated_tokens: usize) -> DimensionScore {
    let (score, signal) = if estimated_tokens < 50 {
        (-1.0, Some("short input (<50 tokens)".to_string()))
    } else if estimated_tokens > 500 {
        (1.0, Some("long input (>500 tokens)".to_string()))
    } else {
        (0.0, None)
    };
    DimensionScore {
        name: "token_count",
        score,
        signal,
    }
}

fn score_code_presence(text: &str) -> DimensionScore {
    let keywords = [
        "function", "class", "import", "def", "SELECT", "async", "await", "const", "let", "return",
        "```",
    ];
    score_keyword_match(text, &keywords, "code_presence", "code", 1, 3, 0.5, 1.0)
}

fn score_reasoning_markers(text: &str) -> DimensionScore {
    let keywords = [
        "prove",
        "theorem",
        "derive",
        "step by step",
        "chain of thought",
        "formally",
        "mathematical",
        "proof",
        "logically",
        "compare",
        "contrast",
        "analyze",
        "consider",
    ];
    score_keyword_match(
        text,
        &keywords,
        "reasoning_markers",
        "reasoning",
        1,
        2,
        0.5,
        1.0,
    )
}

fn score_technical_terms(text: &str) -> DimensionScore {
    let keywords = [
        "algorithm",
        "optimize",
        "architecture",
        "distributed",
        "kubernetes",
        "microservice",
        "database",
        "infrastructure",
        "scalab",
        "latency",
        "throughput",
        "consistency",
        "monolith",
        "deployment",
        "replication",
        "sharding",
    ];
    score_keyword_match(
        text,
        &keywords,
        "technical_terms",
        "technical",
        1,
        3,
        0.5,
        1.0,
    )
}

fn score_creative_markers(text: &str) -> DimensionScore {
    let keywords = [
        "story",
        "poem",
        "compose",
        "brainstorm",
        "creative",
        "imagine",
        "write a",
    ];
    score_keyword_match(
        text,
        &keywords,
        "creative_markers",
        "creative",
        1,
        2,
        0.3,
        0.7,
    )
}

fn score_simple_indicators(text: &str) -> DimensionScore {
    let keywords = [
        "what is",
        "define",
        "translate",
        "hello",
        "yes or no",
        "capital of",
        "how old",
        "who is",
        "when was",
    ];
    // Simple indicators pull score NEGATIVE.
    score_keyword_match(
        text,
        &keywords,
        "simple_indicators",
        "simple",
        1,
        2,
        -0.5,
        -1.0,
    )
}

fn score_multi_step_patterns(text: &str) -> DimensionScore {
    let lower = text.to_lowercase();
    // Simple pattern matching without pulling in regex crate.
    let has_first_then = lower.contains("first") && lower.contains("then");
    let has_step_n =
        lower.contains("step 1") || lower.contains("step 2") || lower.contains("step 3");
    let has_numbered_list = {
        let mut found = false;
        for line in lower.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("1.") || trimmed.starts_with("2.") || trimmed.starts_with("3.") {
                found = true;
                break;
            }
        }
        found
    };

    if has_first_then || has_step_n || has_numbered_list {
        DimensionScore {
            name: "multi_step_patterns",
            score: 0.5,
            signal: Some("multi-step pattern detected".to_string()),
        }
    } else {
        DimensionScore {
            name: "multi_step_patterns",
            score: 0.0,
            signal: None,
        }
    }
}

/// Question complexity: counts `?` marks and detects analytical question
/// words (compare, contrast, explain, recommend, etc.).
fn score_question_complexity_v2(text: &str) -> DimensionScore {
    let q_count = text.matches('?').count();
    let complex_words = [
        "compare",
        "contrast",
        "explain",
        "recommend",
        "evaluate",
        "analyze",
        "tradeoff",
        "trade-off",
        "implication",
    ];
    let lower = text.to_lowercase();
    let word_hits: usize = complex_words.iter().filter(|w| lower.contains(*w)).count();

    let score = if q_count > 3 || word_hits >= 3 {
        1.0
    } else if q_count > 1 || word_hits >= 2 {
        0.5
    } else if word_hits == 1 {
        0.3
    } else {
        0.0
    };

    let signal = if score > 0.0 {
        let mut parts = Vec::new();
        if q_count > 0 {
            parts.push(format!("{q_count}?"));
        }
        if word_hits > 0 {
            let matched: Vec<&str> = complex_words
                .iter()
                .filter(|w| lower.contains(*w))
                .copied()
                .take(3)
                .collect();
            parts.push(matched.join(", "));
        }
        Some(format!("question complexity ({})", parts.join("; ")))
    } else {
        None
    };

    DimensionScore {
        name: "question_complexity",
        score,
        signal,
    }
}

fn score_imperative_verbs(text: &str) -> DimensionScore {
    let keywords = [
        "build",
        "create",
        "implement",
        "design",
        "develop",
        "generate",
        "deploy",
        "configure",
        "set up",
        "recommend",
        "consider",
        "evaluate",
    ];
    score_keyword_match(
        text,
        &keywords,
        "imperative_verbs",
        "imperative",
        1,
        3,
        0.3,
        0.7,
    )
}

fn score_constraint_count(text: &str) -> DimensionScore {
    let keywords = [
        "under",
        "at most",
        "at least",
        "within",
        "no more than",
        "maximum",
        "minimum",
        "limit",
        "budget",
    ];
    score_keyword_match(
        text,
        &keywords,
        "constraint_count",
        "constraints",
        1,
        3,
        0.3,
        0.7,
    )
}

fn score_output_format(text: &str) -> DimensionScore {
    let keywords = [
        "json",
        "yaml",
        "xml",
        "table",
        "csv",
        "markdown",
        "schema",
        "format as",
        "structured",
    ];
    score_keyword_match(
        text,
        &keywords,
        "output_format",
        "structured output",
        1,
        2,
        0.3,
        0.7,
    )
}

fn score_reference_complexity(text: &str) -> DimensionScore {
    let keywords = [
        "above",
        "below",
        "previous",
        "following",
        "the docs",
        "the api",
        "the code",
        "earlier",
    ];
    score_keyword_match(
        text,
        &keywords,
        "reference_complexity",
        "references",
        1,
        3,
        0.3,
        0.7,
    )
}

fn score_negation_complexity(text: &str) -> DimensionScore {
    let keywords = [
        "don't", "do not", "avoid", "never", "without", "except", "exclude",
    ];
    score_keyword_match(
        text,
        &keywords,
        "negation_complexity",
        "negation",
        1,
        3,
        0.3,
        0.7,
    )
}

fn score_domain_specificity_v2(text: &str) -> DimensionScore {
    let keywords = [
        "quantum",
        "fpga",
        "vlsi",
        "risc-v",
        "genomics",
        "proteomics",
        "topological",
        "homomorphic",
        "zero-knowledge",
    ];
    score_keyword_match(
        text,
        &keywords,
        "domain_specificity",
        "domain-specific",
        1,
        2,
        0.5,
        1.0,
    )
}

fn score_agentic_task(text: &str) -> (f64, Option<String>) {
    let keywords: &[&str] = &[
        "read file",
        "read the file",
        "look at",
        "edit",
        "modify",
        "update the",
        "change the",
        "write to",
        "create file",
        "execute",
        "deploy",
        "install",
        "after that",
        "once done",
        "step 1",
        "step 2",
        "fix",
        "debug",
        "until it works",
        "iterate",
        "verify",
        "confirm",
    ];
    let lower = text.to_lowercase();
    let matches: Vec<&&str> = keywords
        .iter()
        .filter(|kw| lower.contains(&kw.to_lowercase()))
        .collect();
    let count = matches.len();
    let score = if count >= 4 {
        1.0
    } else if count == 3 {
        0.6
    } else if count >= 1 {
        0.2
    } else {
        0.0
    };
    let signal = if count > 0 {
        Some(format!(
            "agentic ({count} markers: {})",
            matches
                .iter()
                .take(3)
                .map(|s| **s)
                .collect::<Vec<_>>()
                .join(", ")
        ))
    } else {
        None
    };
    (score, signal)
}

/// Sigmoid confidence calibration.
///
/// Maps the distance from a tier boundary to a confidence value in (0, 1).
/// At the boundary (distance=0) confidence is exactly 0.5.
pub(crate) fn calibrate_confidence(distance_from_boundary: f64, steepness: f64) -> f64 {
    1.0 / (1.0 + (-steepness * distance_from_boundary).exp())
}

/// Compute the 14-dimension weighted score and return a full decision.
fn score_v2(
    message: &str,
    estimated_tokens: usize,
    scoring: &ScoringConfig,
    tiers: &ClassificationTiers,
) -> Option<ClassificationDecision> {
    let w = &scoring.dimension_weights;
    let bounds = &scoring.tier_boundaries;

    // Collect all 14 dimension scores.
    let dimensions: Vec<(f64, DimensionScore)> = vec![
        (w.token_count, score_token_count(estimated_tokens)),
        (w.code_presence, score_code_presence(message)),
        (w.reasoning_markers, score_reasoning_markers(message)),
        (w.technical_terms, score_technical_terms(message)),
        (w.creative_markers, score_creative_markers(message)),
        (w.simple_indicators, score_simple_indicators(message)),
        (w.multi_step_patterns, score_multi_step_patterns(message)),
        (w.question_complexity, score_question_complexity_v2(message)),
        (w.imperative_verbs, score_imperative_verbs(message)),
        (w.constraint_count, score_constraint_count(message)),
        (w.output_format, score_output_format(message)),
        (w.reference_complexity, score_reference_complexity(message)),
        (w.negation_complexity, score_negation_complexity(message)),
        (w.domain_specificity, score_domain_specificity_v2(message)),
    ];

    // Weighted sum (not averaged — tier boundaries are calibrated for raw sums).
    let weighted_sum: f64 = dimensions
        .iter()
        .map(|(weight, ds)| weight * ds.score)
        .sum();
    let raw_score = weighted_sum;

    // Agentic score (separate dimension, also added to weighted sum).
    let (agentic_score, agentic_signal) = score_agentic_task(message);
    let combined_score = raw_score + (agentic_score * w.agentic_task);

    // Collect signals.
    let mut signals: Vec<String> = dimensions
        .iter()
        .filter_map(|(_, ds)| ds.signal.clone())
        .collect();
    if let Some(s) = agentic_signal {
        signals.push(s);
    }

    // --- Scoring overrides (applied before tier mapping) ---
    let overrides = &scoring.overrides;

    // 1. Reasoning keyword override: 2+ reasoning keywords -> force REASONING, confidence >= 0.85
    let reasoning_keywords = [
        "prove",
        "theorem",
        "derive",
        "step by step",
        "chain of thought",
        "formally",
        "mathematical",
        "proof",
        "logically",
    ];
    let lower = message.to_lowercase();
    let reasoning_hits: usize = reasoning_keywords
        .iter()
        .filter(|kw| lower.contains(*kw))
        .count();

    if reasoning_hits >= 2 {
        let hint = tiers.reasoning.clone().unwrap_or_default();
        if hint.is_empty() {
            return None;
        }
        return Some(ClassificationDecision {
            hint,
            priority: 0,
            tier: Tier::Reasoning,
            confidence: 0.85_f64.max(calibrate_confidence(0.3, scoring.confidence_steepness)),
            agentic_score,
            signals,
        });
    }

    // 2. Large context override: estimated tokens > max_tokens_force_complex -> force COMPLEX
    if estimated_tokens > overrides.max_tokens_force_complex {
        let hint = tiers.complex.clone().unwrap_or_default();
        if hint.is_empty() {
            return None;
        }
        return Some(ClassificationDecision {
            hint,
            priority: 0,
            tier: Tier::Complex,
            confidence: calibrate_confidence(0.4, scoring.confidence_steepness),
            agentic_score,
            signals,
        });
    }

    // --- Tier mapping ---
    let (tier, closest_boundary_distance) = if combined_score < bounds.simple_medium {
        let dist = bounds.simple_medium - combined_score;
        (Tier::Simple, dist)
    } else if combined_score < bounds.medium_complex {
        let dist_low = combined_score - bounds.simple_medium;
        let dist_high = bounds.medium_complex - combined_score;
        (Tier::Medium, dist_low.min(dist_high))
    } else if combined_score < bounds.complex_reasoning {
        let dist_low = combined_score - bounds.medium_complex;
        let dist_high = bounds.complex_reasoning - combined_score;
        (Tier::Complex, dist_low.min(dist_high))
    } else {
        let dist = combined_score - bounds.complex_reasoning;
        (Tier::Reasoning, dist)
    };

    let confidence = calibrate_confidence(closest_boundary_distance, scoring.confidence_steepness);

    // 3. Structured output override: bump minimum tier
    let tier = apply_structured_output_override(message, tier, overrides);

    // If confidence < threshold -> use ambiguous default tier
    let tier = if confidence < scoring.confidence_threshold {
        overrides.ambiguous_default_tier.clone()
    } else {
        tier
    };

    let hint = match &tier {
        Tier::Simple => tiers.simple.clone(),
        Tier::Medium => tiers.medium.clone(),
        Tier::Complex => tiers.complex.clone(),
        Tier::Reasoning => tiers.reasoning.clone(),
    };

    let hint = hint?;

    Some(ClassificationDecision {
        hint,
        priority: 0,
        tier,
        confidence,
        agentic_score,
        signals,
    })
}

/// If the message contains structured output markers, bump the tier to at
/// least the configured minimum.
fn apply_structured_output_override(
    message: &str,
    current: Tier,
    overrides: &ScoringOverrides,
) -> Tier {
    let structured_keywords = ["json", "yaml", "schema", "structured", "format as"];
    let lower = message.to_lowercase();
    let has_structured = structured_keywords.iter().any(|kw| lower.contains(kw));
    if !has_structured {
        return current;
    }
    let min_tier = &overrides.structured_output_min_tier;
    tier_max(current, min_tier.clone())
}

/// Return the higher of two tiers (Simple < Medium < Complex < Reasoning).
fn tier_max(a: Tier, b: Tier) -> Tier {
    let rank = |t: &Tier| match t {
        Tier::Simple => 0,
        Tier::Medium => 1,
        Tier::Complex => 2,
        Tier::Reasoning => 3,
    };
    if rank(&a) >= rank(&b) {
        a
    } else {
        b
    }
}

// ── Classifier dispatch ──────────────────────────────────────────

/// Classification result with scoring metadata for observability.
///
/// Extended in the 14-dimension scorer upgrade to carry tier, confidence,
/// agentic score, and signal explanations alongside the original hint/priority.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassificationDecision {
    pub hint: String,
    pub priority: i32,
    /// Complexity tier determined by the scorer.
    pub tier: Tier,
    /// Sigmoid-calibrated confidence in the tier assignment (0.0..1.0).
    pub confidence: f64,
    /// Agentic-task score (0.0..1.0) -- how much the message looks like
    /// a multi-step agent task.
    pub agentic_score: f64,
    /// Human-readable signal explanations from each scoring dimension.
    pub signals: Vec<String>,
}

impl Default for ClassificationDecision {
    fn default() -> Self {
        Self {
            hint: String::new(),
            priority: 0,
            tier: Tier::default(),
            confidence: 0.5,
            agentic_score: 0.0,
            signals: Vec::new(),
        }
    }
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
    _turn_count: usize,
) -> Option<ClassificationDecision> {
    if !config.enabled {
        return None;
    }

    match config.mode {
        ClassificationMode::Weighted => {
            // Use the 14-dimension scorer via ScoringConfig.
            // Estimate tokens as ~4 chars per token (common LLM heuristic).
            let estimated_tokens = message.len() / 4;
            score_v2(message, estimated_tokens, &config.scoring, &config.tiers)
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
                ..Default::default()
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
        PlanningConfig, QueryClassificationConfig, ScoringConfig,
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

        // Short greeting -> simple
        assert_eq!(classify(&config, "hi"), Some("hint:simple".into()));

        // Long complex reasoning request -> reasoning or complex
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

    // ── v2 scorer tests (14-dimension + sigmoid confidence) ────────

    fn make_weighted_v2_config() -> QueryClassificationConfig {
        QueryClassificationConfig {
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
            scoring: ScoringConfig::default(),
            planning: PlanningConfig::default(),
        }
    }

    #[test]
    fn scorer_v2_short_simple_message() {
        let config = make_weighted_v2_config();
        let result = classify_with_context(&config, "hi", 0).unwrap();
        assert_eq!(result.hint, "hint:simple");
        assert!(result.agentic_score < 0.3);
        assert!(result.confidence >= 0.5);
    }

    #[test]
    fn scorer_v2_agentic_message() {
        let config = make_weighted_v2_config();
        let result = classify_with_context(
            &config,
            "edit the config file, deploy to staging, then verify the endpoint works",
            0,
        )
        .unwrap();
        assert!(
            result.agentic_score >= 0.5,
            "agentic_score={}",
            result.agentic_score
        );
    }

    #[test]
    fn scorer_v2_reasoning_override() {
        let config = make_weighted_v2_config();
        let result = classify_with_context(
            &config,
            "prove this theorem step by step using chain of thought reasoning",
            0,
        )
        .unwrap();
        assert_eq!(result.hint, "hint:reasoning");
        assert!(result.confidence >= 0.85);
    }

    #[test]
    fn scorer_v2_multi_step_message() {
        let config = make_weighted_v2_config();
        let result = classify_with_context(
            &config,
            "first read the file, then update the database, step 3 is to notify the team",
            0,
        )
        .unwrap();
        assert!(result.hint != "hint:simple");
    }

    #[test]
    fn sigmoid_confidence_at_boundary_is_half() {
        let confidence = calibrate_confidence(0.0, 12.0);
        assert!((confidence - 0.5).abs() < 0.01);
    }

    #[test]
    fn sigmoid_confidence_far_from_boundary() {
        let confidence = calibrate_confidence(0.5, 12.0);
        assert!(confidence > 0.99);
    }
}
