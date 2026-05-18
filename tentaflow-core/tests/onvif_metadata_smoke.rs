// =============================================================================
// File: tests/onvif_metadata_smoke.rs
// Purpose: Drives the F2 P6.a ONVIF Media2 / PullPoint events SOAP clients
//          end-to-end over wiremock. Unit tests in the source modules cover
//          XML parsing in isolation; this suite asserts the reqwest +
//          SOAP-action plumbing is wired correctly for the new calls.
// =============================================================================

#![cfg(feature = "camera")]

use tentaflow_core::services::camera_ingest::onvif_events::{
    create_pull_point_subscription, pull_messages, unsubscribe_pull_point,
};
use tentaflow_core::services::camera_ingest::onvif_media::{
    get_metadata_configurations, OnvifCredentials,
};
use wiremock::matchers::{body_string_contains, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const GET_METADATA_CONFIGS_OK: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<env:Envelope xmlns:env="http://www.w3.org/2003/05/soap-envelope"
              xmlns:tt="http://www.onvif.org/ver10/schema"
              xmlns:tr2="http://www.onvif.org/ver20/media/wsdl">
  <env:Body>
    <tr2:GetMetadataConfigurationsResponse>
      <tr2:Configurations token="MetaCfg_main">
        <tt:Name>Main Metadata</tt:Name>
        <tt:AnalyticsEngineConfiguration token="AEC_42"/>
      </tr2:Configurations>
    </tr2:GetMetadataConfigurationsResponse>
  </env:Body>
</env:Envelope>"#;

const GET_METADATA_CONFIGS_EMPTY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<env:Envelope xmlns:env="http://www.w3.org/2003/05/soap-envelope"
              xmlns:tr2="http://www.onvif.org/ver20/media/wsdl">
  <env:Body>
    <tr2:GetMetadataConfigurationsResponse/>
  </env:Body>
</env:Envelope>"#;

const CREATE_SUBSCRIPTION_OK: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<env:Envelope xmlns:env="http://www.w3.org/2003/05/soap-envelope"
              xmlns:wsa="http://www.w3.org/2005/08/addressing"
              xmlns:wsnt="http://docs.oasis-open.org/wsn/b-2"
              xmlns:tev="http://www.onvif.org/ver10/events/wsdl">
  <env:Body>
    <tev:CreatePullPointSubscriptionResponse>
      <tev:SubscriptionReference>
        <wsa:Address>SUBSCRIPTION_PLACEHOLDER</wsa:Address>
      </tev:SubscriptionReference>
      <wsnt:CurrentTime>2026-05-17T10:00:00Z</wsnt:CurrentTime>
      <wsnt:TerminationTime>2026-05-17T10:30:00Z</wsnt:TerminationTime>
    </tev:CreatePullPointSubscriptionResponse>
  </env:Body>
</env:Envelope>"#;

const PULL_MESSAGES_ONE_EVENT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<env:Envelope xmlns:env="http://www.w3.org/2003/05/soap-envelope"
              xmlns:wsnt="http://docs.oasis-open.org/wsn/b-2"
              xmlns:tev="http://www.onvif.org/ver10/events/wsdl"
              xmlns:tt="http://www.onvif.org/ver10/schema"
              xmlns:tns1="http://www.onvif.org/ver10/topics">
  <env:Body>
    <tev:PullMessagesResponse>
      <tev:CurrentTime>2026-05-17T10:01:00Z</tev:CurrentTime>
      <tev:TerminationTime>2026-05-17T10:30:00Z</tev:TerminationTime>
      <wsnt:NotificationMessage>
        <wsnt:Topic>tns1:RuleEngine/CellMotionDetector/Motion</wsnt:Topic>
        <wsnt:Message>
          <tt:Message UtcTime="2026-05-17T10:00:55Z">
            <tt:Source>
              <tt:SimpleItem Name="VideoSourceConfigurationToken" Value="VSC_1"/>
            </tt:Source>
            <tt:Data>
              <tt:Object ObjectId="7">
                <tt:Appearance>
                  <tt:Shape>
                    <tt:BoundingBox left="0.1" top="0.2" right="0.4" bottom="0.6"/>
                  </tt:Shape>
                  <tt:Class><tt:Type Likelihood="0.81">Person</tt:Type></tt:Class>
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

fn creds() -> OnvifCredentials {
    OnvifCredentials {
        username: "admin".into(),
        password: "secret".into(),
    }
}

#[tokio::test]
async fn get_metadata_configurations_returns_one_config() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/onvif/Media2"))
        .and(header_exists("Content-Type"))
        .and(body_string_contains("GetMetadataConfigurations"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "application/soap+xml")
                .set_body_string(GET_METADATA_CONFIGS_OK),
        )
        .mount(&server)
        .await;

    let url = format!("{}/onvif/Media2", server.uri());
    let cfgs = get_metadata_configurations(&url, &creds(), 5_000)
        .await
        .expect("ok");
    assert_eq!(cfgs.len(), 1);
    assert_eq!(cfgs[0].token, "MetaCfg_main");
    assert_eq!(cfgs[0].analytics_engine_token.as_deref(), Some("AEC_42"));
}

#[tokio::test]
async fn get_metadata_configurations_empty_means_no_analytics() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/onvif/Media2"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "application/soap+xml")
                .set_body_string(GET_METADATA_CONFIGS_EMPTY),
        )
        .mount(&server)
        .await;

    let url = format!("{}/onvif/Media2", server.uri());
    let cfgs = get_metadata_configurations(&url, &creds(), 5_000)
        .await
        .expect("ok");
    assert!(cfgs.is_empty());
}

#[tokio::test]
async fn create_pull_point_subscription_and_pull_event() {
    let server = MockServer::start().await;
    let sub_uri = format!("{}/onvif/Events/Sub_abc", server.uri());
    let create_body = CREATE_SUBSCRIPTION_OK.replace("SUBSCRIPTION_PLACEHOLDER", &sub_uri);

    Mock::given(method("POST"))
        .and(path("/onvif/Events"))
        .and(body_string_contains("CreatePullPointSubscription"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "application/soap+xml")
                .set_body_string(create_body),
        )
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/onvif/Events/Sub_abc"))
        .and(body_string_contains("PullMessages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "application/soap+xml")
                .set_body_string(PULL_MESSAGES_ONE_EVENT),
        )
        .mount(&server)
        .await;

    let events_url = format!("{}/onvif/Events", server.uri());
    let sub = create_pull_point_subscription(&events_url, &creds(), 60, 5_000)
        .await
        .expect("subscribe");
    assert_eq!(sub.reference_uri, sub_uri);
    // Termination 2026-05-17T10:30:00Z parses to a positive unix timestamp
    // — the exact constant is asserted in the unit test; here we only
    // verify the field is populated.
    assert!(sub.termination_time_unix > 0);

    let events = pull_messages(&sub.reference_uri, &creds(), 10, 3_000)
        .await
        .expect("pull");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].topic, "tns1:RuleEngine/CellMotionDetector/Motion");
    assert_eq!(events[0].source_token.as_deref(), Some("VSC_1"));
    assert_eq!(events[0].items.len(), 1);
    assert_eq!(events[0].items[0].class, "Person");
}

#[tokio::test]
async fn unsubscribe_pull_point_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/onvif/Events/Sub_abc"))
        .and(body_string_contains("Unsubscribe"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "application/soap+xml")
                .set_body_string(UNSUBSCRIBE_OK),
        )
        .mount(&server)
        .await;

    let sub_uri = format!("{}/onvif/Events/Sub_abc", server.uri());
    unsubscribe_pull_point(&sub_uri, &creds(), 5_000)
        .await
        .expect("unsubscribe ok");
}
