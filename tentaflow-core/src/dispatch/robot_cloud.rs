// =============================================================================
// File: dispatch/robot_cloud.rs — robot vendor cloud account handler
// =============================================================================
//
// `MessageBody::RobotCloudBody` lets an admin sign into a robot vendor account
// from the addon install form or the addon settings and get back what a LAN
// connection needs: serial, per-device key and, when the robot answers LAN
// discovery on this node, its current IP. The password is used for one login
// and dropped; nothing here persists it, the access token or the keys — the
// admin decides what to save through the ordinary install/config paths.

use std::net::Ipv4Addr;
use std::time::Duration;

use tentaflow_hardware::unitree::cloud::{self, CloudApiError, CloudAppFamily, CloudRegion};
use tentaflow_hardware::unitree::discovery;
use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::robot_cloud::{RobotCloudDevice, RobotCloudPayload};
use tentaflow_protocol::{MessageBody, ProtocolError, ProtocolErrorCode, SessionAuth};

use super::HandlerContext;

const PROVIDER_UNITREE: &str = "unitree";
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(2);

#[handler(variant = "RobotCloudBody", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn robot_cloud_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::RobotCloudBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected RobotCloudBody")),
    };
    match payload {
        RobotCloudPayload::DevicesRequest {
            provider,
            region,
            email,
            password,
        } => devices(ctx, provider, region, email, password).await,
        RobotCloudPayload::DevicesResponse { .. } => Err(ProtocolError::bad_request(
            "DevicesResponse is a reply, not a request",
        )),
    }
}

async fn devices(
    ctx: &HandlerContext,
    provider: &str,
    region: &str,
    email: &str,
    password: &str,
) -> Result<MessageBody, ProtocolError> {
    if provider != PROVIDER_UNITREE {
        return Err(ProtocolError::bad_request(format!(
            "unknown robot cloud provider '{provider}'"
        )));
    }
    let region = CloudRegion::parse(region).map_err(|e| ProtocolError::bad_request(e.to_string()))?;

    let found = cloud::fetch_bound_devices(region, CloudAppFamily::Go2, email, password)
        .await
        .map_err(cloud_error)?;

    let serials: Vec<String> = found.iter().map(|d| d.serial.clone()).collect();
    let interfaces = discovery_interfaces();
    // Discovery is a convenience on top of the account data: a node that cannot
    // bind the reply port still returns serial + key, just without an IP.
    let lan = tokio::task::spawn_blocking(move || {
        discovery::discover_go2(&serials, &interfaces, DISCOVERY_TIMEOUT)
    })
    .await
    .map_err(|e| ProtocolError::internal(format!("robot discovery task: {e}")))?
    .unwrap_or_else(|e| {
        tracing::warn!("robot LAN discovery unavailable: {e:#}");
        Default::default()
    });

    let devices: Vec<RobotCloudDevice> = found
        .into_iter()
        .map(|d| {
            let lan_ip = lan.get(&d.serial).map(|ip| ip.to_string());
            RobotCloudDevice {
                serial: d.serial,
                alias: d.alias,
                model: d.model,
                series: d.series,
                online: d.online,
                aes_key: d.aes_key,
                lan_ip,
            }
        })
        .collect();

    // Who pulled device keys from which vendor, never the e-mail or a key.
    let user_id = match &ctx.session {
        SessionAuth::UserSession { user_id, .. } => {
            Some(uuid::Uuid::from_bytes(*user_id).to_string())
        }
        _ => None,
    };
    let details = serde_json::json!({
        "provider": provider,
        "devices": devices.len(),
        "discovered": devices.iter().filter(|d| d.lan_ip.is_some()).count(),
    })
    .to_string();
    if let Err(e) = crate::db::repository::log_audit_full(
        &ctx.state.db,
        user_id.as_deref(),
        None,
        "robot_cloud_devices_fetch",
        Some("robot_cloud"),
        Some(provider),
        Some(&details),
        "warning",
        "unclassified",
        None,
        None,
        None,
        Some(ctx.state.local_node_id.as_ref()),
    ) {
        tracing::warn!("audit log failed (robot_cloud_devices_fetch): {e}");
    }

    Ok(MessageBody::RobotCloudBody(RobotCloudPayload::DevicesResponse { devices }))
}

/// A cloud-side refusal (wrong password, unknown e-mail) is the admin's input
/// to fix; anything else means the vendor API could not be reached from here.
fn cloud_error(e: anyhow::Error) -> ProtocolError {
    match e.downcast_ref::<CloudApiError>() {
        Some(api) => ProtocolError::bad_request(if api.message.is_empty() {
            format!("Unitree: {} (code {})", api.action, api.code)
        } else {
            format!("Unitree: {} (code {})", api.message, api.code)
        }),
        None => ProtocolError::new(ProtocolErrorCode::NotAvailable, format!("{e:#}")),
    }
}

/// IPv4 addresses of the interfaces a robot can sit behind: up, and neither
/// loopback nor container bridges nor tunnels (multicast does not cross them).
fn discovery_interfaces() -> Vec<Ipv4Addr> {
    crate::mesh::network_interfaces::list_interfaces()
        .into_iter()
        .filter(|i| i.is_up && matches!(i.kind.as_str(), "ethernet" | "wifi"))
        .flat_map(|i| i.ipv4_addrs.into_iter())
        .filter_map(|a| a.parse::<Ipv4Addr>().ok())
        .collect()
}

// `#[handler]` registers the family name, which no frame carries —
// `variant_name_of` reports the concrete request variant, so it needs its own
// registry entry pointing at the same dispatch wrapper.
::inventory::submit! {
    crate::dispatch::HandlerMeta {
        variant_name: "RobotCloudDevicesRequest",
        since_major: 1,
        since_minor: 0,
        required_auth: crate::dispatch::SessionAuthKind::Admin,
        metric_name: "tentaflow_ws_handler_robot_cloud_devices",
        dispatch_fn: __tentaflow_dispatch_robot_cloud_dispatch,
    }
}
