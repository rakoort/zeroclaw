//! Classifier-based fast-path routing.
//!
//! Determines whether an incoming message should bypass the planner (fast path)
//! or go through the full planner pipeline, based on the classifier decision.

use crate::agent::classifier::ClassificationDecision;
use crate::config::schema::Tier;

/// Routing decision produced by [`route_decision`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteDecision {
    /// Classifier says Simple with high confidence — run flat tool loop
    /// with a tight iteration budget, skipping the planner.
    FastPath,
    /// Use the full planner pipeline.
    PlannerPath,
    /// No planner configured — go directly to the flat tool loop with the
    /// normal iteration budget.
    DirectLoop,
}

/// Decide the execution route for a message based on classifier output.
///
/// Returns [`RouteDecision::FastPath`] when all of:
/// - A classifier decision is available
/// - The tier is `Simple`
/// - The confidence meets or exceeds `confidence_threshold`
/// - A planner model is configured (`has_planner` is true)
///
/// Returns [`RouteDecision::PlannerPath`] when a planner is configured but
/// the fast-path conditions are not met.
///
/// Returns [`RouteDecision::DirectLoop`] when no planner is configured.
pub fn route_decision(
    decision: Option<&ClassificationDecision>,
    confidence_threshold: f64,
    has_planner: bool,
) -> RouteDecision {
    if !has_planner {
        return RouteDecision::DirectLoop;
    }

    if let Some(d) = decision {
        if d.tier == Tier::Simple && d.confidence >= confidence_threshold {
            return RouteDecision::FastPath;
        }
    }

    RouteDecision::PlannerPath
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::classifier::ClassificationDecision;
    use crate::config::schema::Tier;

    fn simple_decision(confidence: f64) -> ClassificationDecision {
        ClassificationDecision {
            tier: Tier::Simple,
            confidence,
            ..ClassificationDecision::default()
        }
    }

    fn complex_decision(confidence: f64) -> ClassificationDecision {
        ClassificationDecision {
            tier: Tier::Complex,
            confidence,
            ..ClassificationDecision::default()
        }
    }

    fn medium_decision(confidence: f64) -> ClassificationDecision {
        ClassificationDecision {
            tier: Tier::Medium,
            confidence,
            ..ClassificationDecision::default()
        }
    }

    // ── Fast path tests ──────────────────────────────────────────

    #[test]
    fn simple_high_confidence_routes_to_fast_path() {
        let d = simple_decision(0.9);
        assert_eq!(route_decision(Some(&d), 0.8, true), RouteDecision::FastPath,);
    }

    #[test]
    fn simple_at_threshold_routes_to_fast_path() {
        let d = simple_decision(0.8);
        assert_eq!(route_decision(Some(&d), 0.8, true), RouteDecision::FastPath,);
    }

    #[test]
    fn simple_below_threshold_routes_to_planner() {
        let d = simple_decision(0.79);
        assert_eq!(
            route_decision(Some(&d), 0.8, true),
            RouteDecision::PlannerPath,
        );
    }

    // ── Non-simple tiers always go to planner ────────────────────

    #[test]
    fn medium_tier_routes_to_planner() {
        let d = medium_decision(0.95);
        assert_eq!(
            route_decision(Some(&d), 0.8, true),
            RouteDecision::PlannerPath,
        );
    }

    #[test]
    fn complex_tier_routes_to_planner() {
        let d = complex_decision(0.95);
        assert_eq!(
            route_decision(Some(&d), 0.8, true),
            RouteDecision::PlannerPath,
        );
    }

    #[test]
    fn reasoning_tier_routes_to_planner() {
        let d = ClassificationDecision {
            tier: Tier::Reasoning,
            confidence: 0.99,
            ..ClassificationDecision::default()
        };
        assert_eq!(
            route_decision(Some(&d), 0.8, true),
            RouteDecision::PlannerPath,
        );
    }

    // ── No planner configured → DirectLoop ───────────────────────

    #[test]
    fn no_planner_routes_to_direct_loop() {
        let d = simple_decision(0.95);
        assert_eq!(
            route_decision(Some(&d), 0.8, false),
            RouteDecision::DirectLoop,
        );
    }

    #[test]
    fn no_planner_no_decision_routes_to_direct_loop() {
        assert_eq!(route_decision(None, 0.8, false), RouteDecision::DirectLoop,);
    }

    // ── No classifier decision → PlannerPath (when planner exists) ──

    #[test]
    fn no_decision_with_planner_routes_to_planner() {
        assert_eq!(route_decision(None, 0.8, true), RouteDecision::PlannerPath,);
    }

    // ── Edge cases ───────────────────────────────────────────────

    #[test]
    fn zero_confidence_threshold_routes_all_simple_to_fast_path() {
        let d = simple_decision(0.01);
        assert_eq!(route_decision(Some(&d), 0.0, true), RouteDecision::FastPath,);
    }

    #[test]
    fn threshold_of_one_rejects_near_certain_simple() {
        let d = simple_decision(0.999);
        assert_eq!(
            route_decision(Some(&d), 1.0, true),
            RouteDecision::PlannerPath,
        );
    }

    #[test]
    fn threshold_of_one_accepts_exact_one() {
        let d = simple_decision(1.0);
        assert_eq!(route_decision(Some(&d), 1.0, true), RouteDecision::FastPath,);
    }
}
