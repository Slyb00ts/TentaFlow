// =============================================================================
// File: services/camera_ingest/onvif_events.rs — ONVIF PullPoint events
// client (F2 P6.a). Creates the subscription, pulls messages, and tears it
// down.
// =============================================================================
//
// Per the ONVIF Events spec, a PullPoint subscription is a three-call dance:
//
//   1. `CreatePullPointSubscription` (Events service URL)
//      — returns `SubscriptionReference/Address` (the dedicated URL used
//        by subsequent calls) and `TerminationTime` (absolute UTC after
//        which the device drops the subscription unless renewed).
//   2. `PullMessages` (subscription URL) — long-poll for analytics events.
//      The body carries a `Timeout` (PT<seconds>S) and `MessageLimit`.
//   3. `Unsubscribe` (subscription URL) — graceful teardown. Best-effort:
//      cameras drop subscriptions on `TerminationTime` even without it.
//
// We reuse `onvif_media::build_envelope_pub` for the WS-Security envelope
// (UsernameToken with password digest). The Events namespace differs from
// Media — we prepend `xmlns:wsnt` / `xmlns:wsa` once in `build_envelope_pub`
// (they live there because the media envelope already declares them).
//
// Each pulled `NotificationMessage` carries:
//   * `wsnt:Topic` — dotted ONVIF topic, e.g.
//     `tns1:RuleEngine/CellMotionDetector/Motion`.
//   * `wsnt:Message/tt:Message` — the analytics payload. For object
//     detections this contains a metadata XML fragment that we delegate
//     to `onvif_metadata_parser::parse_metadata_xml`.
//   * `UtcTime` attribute on `tt:Message` — the event timestamp.

use chrono::{DateTime, Utc};

use crate::services::camera_ingest::onvif_media::{
    build_envelope_pub, contains_tag_pub, extract_open_tag_attr_pub, extract_xml_text_pub,
    find_close_tag_pub, send_soap_pub, xml_escape_pub, OnvifCredentials, OnvifError,
};
use crate::services::camera_ingest::onvif_metadata_parser::{
    parse_metadata_xml, MetadataItem,
};

const ACTION_CREATE_PULL_POINT: &str =
    "http://www.onvif.org/ver10/events/wsdl/EventPortType/CreatePullPointSubscriptionRequest";
const ACTION_PULL_MESSAGES: &str =
    "http://www.onvif.org/ver10/events/wsdl/PullPointSubscription/PullMessagesRequest";
const ACTION_UNSUBSCRIBE: &str =
    "http://docs.oasis-open.org/wsn/bw-2/SubscriptionManager/UnsubscribeRequest";

/// A live PullPoint subscription as advertised by the device. The
/// `reference_uri` is the URL the device wants subsequent `PullMessages` /
/// `Unsubscribe` requests addressed to (it is typically distinct from the
/// Events service URL).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullPointSubscription {
    pub reference_uri: String,
    pub termination_time_unix: i64,
}

/// A single notification message pulled from the subscription. Multiple
/// `MetadataItem`s can ride one message (one analytics frame contains
/// multiple objects).
#[derive(Debug, Clone, PartialEq)]
pub struct PullPointEvent {
    pub utc_timestamp: i64,
    pub topic: String,
    pub source_token: Option<String>,
    pub items: Vec<MetadataItem>,
}

// =============================================================================
// Public API
// =============================================================================

/// Create a PullPoint subscription on the device. `initial_termination_secs`
/// is clamped to `[60, 3600]` — too short and the device drops the
/// subscription between pulls; too long burdens cameras that enforce a
/// per-subscription cap.
pub async fn create_pull_point_subscription(
    events_service_url: &str,
    creds: &OnvifCredentials,
    initial_termination_secs: u32,
    timeout_ms: u32,
) -> Result<PullPointSubscription, OnvifError> {
    let secs = initial_termination_secs.clamp(60, 3600);
    let body = format!(
        r#"<wsnt:CreatePullPointSubscription xmlns:wsnt="http://docs.oasis-open.org/wsn/b-2">
  <wsnt:InitialTerminationTime>PT{secs}S</wsnt:InitialTerminationTime>
</wsnt:CreatePullPointSubscription>"#,
        secs = secs,
    );
    let envelope = build_envelope_pub(creds, &body);
    let resp = send_soap_pub(
        events_service_url,
        ACTION_CREATE_PULL_POINT,
        envelope,
        timeout_ms,
    )
    .await?;
    parse_create_subscription_response(&resp)
}

/// Long-poll the device for the next batch of events. The device returns
/// up to `max_messages` notifications, blocking up to `timeout_ms`
/// milliseconds while it waits for analytics to fire. Empty `Ok(vec![])`
/// is the normal "nothing happened" outcome.
pub async fn pull_messages(
    subscription_uri: &str,
    creds: &OnvifCredentials,
    max_messages: u32,
    timeout_ms: u32,
) -> Result<Vec<PullPointEvent>, OnvifError> {
    // Long-poll: ask the device to wait up to `(timeout_ms - 1s)` before
    // returning empty. We always carry at least 1 s of headroom so the
    // HTTP timeout never wins the race against the SOAP poll.
    let device_wait_secs = (timeout_ms.saturating_sub(1_000) / 1_000).max(1);
    let limit = max_messages.clamp(1, 1024);
    let body = format!(
        r#"<wsnt:PullMessages xmlns:wsnt="http://www.onvif.org/ver10/events/wsdl">
  <wsnt:Timeout>PT{wait}S</wsnt:Timeout>
  <wsnt:MessageLimit>{limit}</wsnt:MessageLimit>
</wsnt:PullMessages>"#,
        wait = device_wait_secs,
        limit = limit,
    );
    let envelope = build_envelope_pub(creds, &body);
    let resp = send_soap_pub(
        subscription_uri,
        ACTION_PULL_MESSAGES,
        envelope,
        timeout_ms,
    )
    .await?;
    parse_pull_messages_response(&resp)
}

/// Best-effort teardown. A 200 from the device is preferred, but a 4xx /
/// SOAP fault here is non-fatal — the device will drop the subscription
/// at `TerminationTime` either way. The function returns the underlying
/// error so the caller can log it; callers do not need to retry.
pub async fn unsubscribe_pull_point(
    subscription_uri: &str,
    creds: &OnvifCredentials,
    timeout_ms: u32,
) -> Result<(), OnvifError> {
    let body =
        r#"<wsnt:Unsubscribe xmlns:wsnt="http://docs.oasis-open.org/wsn/b-2"/>"#;
    let envelope = build_envelope_pub(creds, body);
    let _resp = send_soap_pub(
        subscription_uri,
        ACTION_UNSUBSCRIBE,
        envelope,
        timeout_ms,
    )
    .await?;
    Ok(())
}

// Suppress dead-code warnings on `xml_escape_pub` import — it is re-exported
// from onvif_media for use by future bodies that need to include
// caller-controlled strings; the bodies built here are all static.
#[allow(dead_code)]
fn _force_use_of_xml_escape() {
    let _ = xml_escape_pub;
}

// =============================================================================
// Response parsing
// =============================================================================

fn parse_create_subscription_response(
    xml: &str,
) -> Result<PullPointSubscription, OnvifError> {
    if !contains_tag_pub(xml, "CreatePullPointSubscriptionResponse") {
        return Err(OnvifError::MalformedResponse(
            "no <CreatePullPointSubscriptionResponse> in body".into(),
        ));
    }
    // SubscriptionReference > Address — the URL we address subsequent
    // calls to. Most cameras emit `<wsa:Address>` but some shorten to
    // `<Address>`; the local-name extractor handles both.
    let reference_uri = extract_xml_text_pub(xml, "Address").ok_or_else(|| {
        OnvifError::MalformedResponse(
            "missing <Address> in CreatePullPointSubscriptionResponse".into(),
        )
    })?;
    let termination_time_text = extract_xml_text_pub(xml, "TerminationTime").ok_or_else(|| {
        OnvifError::MalformedResponse("missing <TerminationTime>".into())
    })?;
    let termination_time_unix = parse_iso8601_to_unix(&termination_time_text).ok_or_else(|| {
        OnvifError::MalformedResponse(format!(
            "TerminationTime is not ISO-8601: {termination_time_text}"
        ))
    })?;
    Ok(PullPointSubscription {
        reference_uri,
        termination_time_unix,
    })
}

fn parse_pull_messages_response(xml: &str) -> Result<Vec<PullPointEvent>, OnvifError> {
    if !contains_tag_pub(xml, "PullMessagesResponse") {
        return Err(OnvifError::MalformedResponse(
            "no <PullMessagesResponse> in body".into(),
        ));
    }
    let mut out = Vec::new();
    // Walk each `<wsnt:NotificationMessage>...</wsnt:NotificationMessage>`
    // block in document order.
    let mut cursor = 0usize;
    while let Some((block, end)) = next_notification_block(xml, cursor) {
        let topic = extract_xml_text_pub(block, "Topic").unwrap_or_default();
        // The Message envelope carries `UtcTime` as an attribute on
        // `<tt:Message>` plus the analytics payload as children. The
        // payload is what the metadata parser consumes.
        let utc_timestamp = extract_message_utc_time(block).unwrap_or(0);
        let source_token = extract_source_token(block);
        let items = parse_metadata_xml(block);
        out.push(PullPointEvent {
            utc_timestamp,
            topic,
            source_token,
            items,
        });
        cursor = end;
    }
    Ok(out)
}

/// Slice each `<...:NotificationMessage>...</NotificationMessage>` block.
fn next_notification_block(xml: &str, start: usize) -> Option<(&str, usize)> {
    let mut cursor = start;
    while cursor < xml.len() {
        let rest = &xml[cursor..];
        let lt = rest.find('<')?;
        let after_lt = &rest[lt + 1..];
        if after_lt.starts_with('/')
            || after_lt.starts_with('!')
            || after_lt.starts_with('?')
        {
            cursor += lt + 1;
            continue;
        }
        let open_end = after_lt.find('>')?;
        let open_body = &after_lt[..open_end];
        let name_end = open_body
            .find(|c: char| c.is_ascii_whitespace() || c == '/')
            .unwrap_or(open_body.len());
        let qname = &open_body[..name_end];
        let local = qname.rsplit(':').next().unwrap_or(qname);
        if local == "NotificationMessage" && !open_body.ends_with('/') {
            let content_start = cursor + lt + 1 + open_end + 1;
            let after_open = &xml[content_start..];
            if let Some(close_idx) = find_close_tag_pub(after_open, "NotificationMessage") {
                let end_offset = content_start + close_idx;
                let after_close = &xml[end_offset..];
                let advance = after_close
                    .find('>')
                    .map(|p| end_offset + p + 1)
                    .unwrap_or(end_offset + 1);
                return Some((&xml[content_start..end_offset], advance));
            }
        }
        cursor += lt + 1 + open_end + 1;
    }
    None
}

/// Extract `UtcTime` attribute from the `<tt:Message UtcTime="...">` tag
/// inside a NotificationMessage block. Returns seconds since unix epoch.
fn extract_message_utc_time(block: &str) -> Option<i64> {
    let mut cursor = 0usize;
    while cursor < block.len() {
        let rest = &block[cursor..];
        let lt = rest.find('<')?;
        let after_lt = &rest[lt + 1..];
        if after_lt.starts_with('/')
            || after_lt.starts_with('!')
            || after_lt.starts_with('?')
        {
            cursor += lt + 1;
            continue;
        }
        let open_end = after_lt.find('>')?;
        let open_body = &after_lt[..open_end];
        let name_end = open_body
            .find(|c: char| c.is_ascii_whitespace() || c == '/')
            .unwrap_or(open_body.len());
        let qname = &open_body[..name_end];
        let local = qname.rsplit(':').next().unwrap_or(qname);
        if local == "Message" {
            if let Some(ts) = extract_open_tag_attr_pub(open_body, "UtcTime") {
                return parse_iso8601_to_unix(&ts);
            }
        }
        cursor += lt + 1 + open_end + 1;
    }
    None
}

/// Extract `<tt:Source><tt:SimpleItem Name="VideoSourceConfigurationToken"
/// Value="..."/>` — the camera/profile the event originated from.
fn extract_source_token(block: &str) -> Option<String> {
    // The Source element wraps one or more SimpleItem children. We want
    // the Value attribute of a SimpleItem whose Name attribute references
    // a video source. Different cameras name the item differently — we
    // accept the common `VideoSourceConfigurationToken`,
    // `VideoSource`, and `Source`.
    let mut cursor = 0usize;
    while cursor < block.len() {
        let rest = &block[cursor..];
        let lt = rest.find('<')?;
        let after_lt = &rest[lt + 1..];
        if after_lt.starts_with('/')
            || after_lt.starts_with('!')
            || after_lt.starts_with('?')
        {
            cursor += lt + 1;
            continue;
        }
        let open_end = after_lt.find('>')?;
        let open_body = &after_lt[..open_end];
        let name_end = open_body
            .find(|c: char| c.is_ascii_whitespace() || c == '/')
            .unwrap_or(open_body.len());
        let qname = &open_body[..name_end];
        let local = qname.rsplit(':').next().unwrap_or(qname);
        if local == "SimpleItem" {
            let name_attr = extract_open_tag_attr_pub(open_body, "Name").unwrap_or_default();
            if name_attr.contains("VideoSource") || name_attr == "Source" {
                if let Some(v) = extract_open_tag_attr_pub(open_body, "Value") {
                    return Some(v);
                }
            }
        }
        cursor += lt + 1 + open_end + 1;
    }
    None
}

fn parse_iso8601_to_unix(s: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(s.trim())
        .ok()
        .map(|d| d.with_timezone(&Utc).timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CREATE_SUBSCRIPTION_OK: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<env:Envelope xmlns:env="http://www.w3.org/2003/05/soap-envelope"
              xmlns:wsa="http://www.w3.org/2005/08/addressing"
              xmlns:wsnt="http://docs.oasis-open.org/wsn/b-2"
              xmlns:tev="http://www.onvif.org/ver10/events/wsdl">
  <env:Body>
    <tev:CreatePullPointSubscriptionResponse>
      <tev:SubscriptionReference>
        <wsa:Address>http://192.168.10.42/onvif/Events/Sub_abc123</wsa:Address>
      </tev:SubscriptionReference>
      <wsnt:CurrentTime>2026-05-17T10:00:00Z</wsnt:CurrentTime>
      <wsnt:TerminationTime>2026-05-17T11:00:00Z</wsnt:TerminationTime>
    </tev:CreatePullPointSubscriptionResponse>
  </env:Body>
</env:Envelope>"#;

    const PULL_MESSAGES_EMPTY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<env:Envelope xmlns:env="http://www.w3.org/2003/05/soap-envelope"
              xmlns:wsnt="http://docs.oasis-open.org/wsn/b-2"
              xmlns:tev="http://www.onvif.org/ver10/events/wsdl">
  <env:Body>
    <tev:PullMessagesResponse>
      <tev:CurrentTime>2026-05-17T10:05:00Z</tev:CurrentTime>
      <tev:TerminationTime>2026-05-17T11:00:00Z</tev:TerminationTime>
    </tev:PullMessagesResponse>
  </env:Body>
</env:Envelope>"#;

    const PULL_MESSAGES_WITH_EVENT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<env:Envelope xmlns:env="http://www.w3.org/2003/05/soap-envelope"
              xmlns:wsnt="http://docs.oasis-open.org/wsn/b-2"
              xmlns:tev="http://www.onvif.org/ver10/events/wsdl"
              xmlns:tt="http://www.onvif.org/ver10/schema"
              xmlns:tns1="http://www.onvif.org/ver10/topics">
  <env:Body>
    <tev:PullMessagesResponse>
      <tev:CurrentTime>2026-05-17T10:05:00Z</tev:CurrentTime>
      <tev:TerminationTime>2026-05-17T11:00:00Z</tev:TerminationTime>
      <wsnt:NotificationMessage>
        <wsnt:Topic Dialect="http://docs.oasis-open.org/wsn/t-1/TopicExpression/Concrete">tns1:RuleEngine/CellMotionDetector/Motion</wsnt:Topic>
        <wsnt:Message>
          <tt:Message UtcTime="2026-05-17T10:04:55Z">
            <tt:Source>
              <tt:SimpleItem Name="VideoSourceConfigurationToken" Value="VSC_1"/>
            </tt:Source>
            <tt:Data>
              <tt:Object ObjectId="42">
                <tt:Appearance>
                  <tt:Shape>
                    <tt:BoundingBox left="0.1" top="0.2" right="0.4" bottom="0.55"/>
                  </tt:Shape>
                  <tt:Class>
                    <tt:Type Likelihood="0.93">Vehicle</tt:Type>
                  </tt:Class>
                </tt:Appearance>
              </tt:Object>
            </tt:Data>
          </tt:Message>
        </wsnt:Message>
      </wsnt:NotificationMessage>
    </tev:PullMessagesResponse>
  </env:Body>
</env:Envelope>"#;

    const UNSUBSCRIBE_OK: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<env:Envelope xmlns:env="http://www.w3.org/2003/05/soap-envelope"
              xmlns:wsnt="http://docs.oasis-open.org/wsn/b-2">
  <env:Body>
    <wsnt:UnsubscribeResponse/>
  </env:Body>
</env:Envelope>"#;

    #[test]
    fn parse_create_subscription_extracts_address_and_termination() {
        let sub = parse_create_subscription_response(CREATE_SUBSCRIPTION_OK).expect("parse");
        assert_eq!(
            sub.reference_uri,
            "http://192.168.10.42/onvif/Events/Sub_abc123"
        );
        // 2026-05-17T11:00:00Z → 1779015600 seconds since epoch.
        assert_eq!(sub.termination_time_unix, 1779015600);
    }

    #[test]
    fn parse_pull_messages_empty_returns_zero_events() {
        let events = parse_pull_messages_response(PULL_MESSAGES_EMPTY).expect("parse");
        assert!(events.is_empty());
    }

    #[test]
    fn parse_pull_messages_extracts_object_and_metadata() {
        let events = parse_pull_messages_response(PULL_MESSAGES_WITH_EVENT).expect("parse");
        assert_eq!(events.len(), 1);
        let ev = &events[0];
        assert_eq!(ev.topic, "tns1:RuleEngine/CellMotionDetector/Motion");
        assert_eq!(ev.source_token.as_deref(), Some("VSC_1"));
        // 2026-05-17T10:04:55Z → 1779012295 seconds since epoch.
        assert_eq!(ev.utc_timestamp, 1779012295);
        assert_eq!(ev.items.len(), 1);
        assert_eq!(ev.items[0].class, "Vehicle");
        assert!((ev.items[0].confidence - 0.93).abs() < 1e-9);
        assert_eq!(ev.items[0].track_id.as_deref(), Some("42"));
    }

    #[test]
    fn parse_pull_messages_malformed_body_is_error() {
        assert!(matches!(
            parse_pull_messages_response("<garbage/>"),
            Err(OnvifError::MalformedResponse(_))
        ));
        // UnsubscribeResponse alone is not a pull response.
        assert!(matches!(
            parse_pull_messages_response(UNSUBSCRIBE_OK),
            Err(OnvifError::MalformedResponse(_))
        ));
    }
}
