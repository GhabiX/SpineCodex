use anyhow::Result;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::RateLimitWindow;
use codex_protocol::protocol::TokenCountEvent;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;
use codex_history::SpineSamplingStartedItem;
use codex_history::SpineTransitionItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::security_risk::SecurityRiskScore;

fn assert_json_roundtrip(item: RolloutItem) -> Result<()> {
    let json = serde_json::to_string(&item)?;
    let restored: RolloutItem = serde_json::from_str(&json)?;
    assert_eq!(
        serde_json::to_value(&restored)?,
        serde_json::to_value(&item)?
    );
    let line = RolloutLine {
        timestamp: "2026-10-13T11:00:00.000Z".to_string(),
        ordinal: Some(7),
        item,
    };
    let bytes = serde_json::to_vec(&line)?;
    let restored = parse_rollout_line_bytes(&bytes)?;
    assert_eq!(serde_json::to_value(restored)?, serde_json::to_value(line)?);
    Ok(())
}

#[test]
fn decimal_rate_limits_roundtrip_through_rollout_item_and_line() -> Result<()> {
    for used_percent in [12.0, 12.5, 1.25e-8, 99.99999999999999] {
        assert_json_roundtrip(RolloutItem::EventMsg(EventMsg::TokenCount(
            TokenCountEvent {
                info: None,
                rate_limits: Some(RateLimitSnapshot {
                    limit_id: None,
                    limit_name: None,
                    normal_model_slug: None,
                    primary: Some(RateLimitWindow {
                        used_percent,
                        window_minutes: Some(10080),
                        resets_at: Some(1789200718),
                    }),
                    secondary: Some(RateLimitWindow {
                        used_percent: 0.125,
                        window_minutes: Some(60),
                        resets_at: None,
                    }),
                    credits: None,
                    individual_limit: None,
                    spend_control_reached: None,
                    plan_type: None,
                    rate_limit_reached_type: None,
                }),
            },
        )))?;
    }
    Ok(())
}

#[test]
fn decimal_security_risk_scores_roundtrip_through_rollout_item_and_line() -> Result<()> {
    assert_json_roundtrip(RolloutItem::SecurityRiskScore(SecurityRiskScore {
        scores: [("low".to_string(), 0.125), ("high".to_string(), 0.875)].into(),
        call_id: Some("tool-call".to_string()),
        action: Some(serde_json::from_str(
            r#"{"duration":0.25,"sequence":9007199254740993,"wide_positive":18446744073709551616,"wide_negative":-9223372036854775809}"#,
        )?),
        sampled_at: None,
    }))
}

#[test]
fn decimal_rollout_preserves_wide_opaque_spine_numbers() -> Result<()> {
    let payload = serde_json::from_str::<serde_json::Value>(
        r#"{"positive":18446744073709551616,"negative":-9223372036854775809}"#,
    )?;
    assert_json_roundtrip(RolloutItem::SpineTransition(SpineTransitionItem {
        version: 1,
        payload: payload.clone(),
    }))?;
    assert_json_roundtrip(RolloutItem::SpineSamplingStarted(
        SpineSamplingStartedItem {
            version: 1,
            payload: payload.clone(),
            sdk_config: None,
            replay_seed: Some(payload),
        },
    ))
}

#[test]
fn decimal_rollout_accepts_alternate_numeric_spellings() -> Result<()> {
    for spelling in ["0.1250", "1e-08", "0.10000000000000001"] {
        let item_json = format!(
            r#"{{"type":"security_risk_score","payload":{{"scores":{{"score":{spelling}}}}}}}"#
        );
        let item: RolloutItem = serde_json::from_str(&item_json)?;
        let expected = RolloutItem::SecurityRiskScore(SecurityRiskScore {
            scores: [("score".to_string(), spelling.parse::<f64>()?)].into(),
            call_id: None,
            action: None,
            sampled_at: None,
        });
        assert_eq!(
            serde_json::to_value(item)?,
            serde_json::to_value(&expected)?
        );
        let line_json = format!(
            r#"{{"timestamp":"2026-10-13T11:00:00.000Z","ordinal":7,"type":"security_risk_score","payload":{{"scores":{{"score":{spelling}}}}}}}"#
        );
        let line = parse_rollout_line(&line_json)?;
        assert_eq!(
            serde_json::to_value(line.item)?,
            serde_json::to_value(expected)?
        );
    }
    Ok(())
}

#[test]
fn decimal_rollout_duplicate_payload_uses_last_value_like_upstream() -> Result<()> {
    // U's canonical Value boundary resolves duplicate keys before typed decoding.
    let json = r#"{"timestamp":"now","type":"security_risk_score","payload":{"scores":{"first":0.25}},"payload":{"scores":{"last":0.75}}}"#;
    let expected =
        r#"{"timestamp":"now","type":"security_risk_score","payload":{"scores":{"last":0.75}}}"#;
    assert_eq!(
        serde_json::to_value(parse_rollout_line(json)?)?,
        serde_json::to_value(parse_rollout_line(expected)?)?
    );
    Ok(())
}

#[test]
fn decimal_review_confidence_roundtrip_through_rollout_item_and_line() -> Result<()> {
    for confidence in [0.0_f32, 0.125, 1.25e-8, 0.99999994] {
        let review: codex_protocol::protocol::ReviewOutputEvent = serde_json::from_value(json!({
            "findings": [{
                "title": "A finding", "body": "Body", "confidence_score": confidence,
                "priority": 1,
                "code_location": {"absolute_file_path": "/tmp/example.rs", "line_range": {"start": 1, "end": 2}}
            }],
            "overall_correctness": "incorrect", "overall_explanation": "A review",
            "overall_confidence_score": confidence
        }))?;
        assert_json_roundtrip(RolloutItem::EventMsg(EventMsg::ExitedReviewMode(
            codex_protocol::protocol::ExitedReviewModeEvent {
                turn_id: Some("review-turn".into()),
                item_id: Some("review-item".into()),
                review_output: Some(review),
            },
        )))?;
    }
    Ok(())
}

#[test]
fn decimal_rollout_does_not_accept_nonnumeric_rate_limits() -> Result<()> {
    let invalid = json!({
        "timestamp": "2026-10-13T11:00:00.000Z",
        "type": "event_msg",
        "payload": {
            "type": "token_count",
            "info": null,
            "rate_limits": {
                "primary": {"used_percent": {"value": 12.5}}
            }
        }
    });
    assert!(parse_rollout_line(&invalid.to_string()).is_err());
    Ok(())
}
