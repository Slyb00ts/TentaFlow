// =============================================================================
// File: robot_cloud.rs
// Purpose: Binary CBOR protocol for robot vendor cloud accounts — the admin
//          signs into the vendor account (Unitree today) from the addon install
//          form or its settings, and Core returns the robots bound to it with
//          what a LAN connection needs: serial, per-device key and, when the
//          robot answers LAN discovery, its current IP.
//
//          The password travels once, inside the request, and is never stored
//          or echoed. The dispatcher marks the request sensitive, so its body
//          stays out of observation logs.
//
//          Append-only, and a rename is the one change that breaks every
//          deployed peer while the round-trip tests stay green — ciborium tags
//          by NAME. A field added later MUST carry `#[serde(default)]`.
// Example: MessageBody::RobotCloudBody(RobotCloudPayload::DevicesRequest {
//              provider: "unitree".into(), region: "global".into(),
//              email: "…".into(), password: "…".into() })
// =============================================================================

use serde::{Deserialize, Serialize};

/// One robot bound to the vendor account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RobotCloudDevice {
    pub serial: String,
    /// The name the owner gave the robot in the vendor app.
    pub alias: String,
    pub model: String,
    pub series: String,
    pub online: Option<bool>,
    /// Per-device LAN key (32 hex chars for Unitree). Empty when the robot's
    /// firmware does not use one.
    pub aes_key: String,
    /// IPv4 the robot announced on this node's LAN discovery, `None` when it
    /// did not answer (other network, powered off, discovery blocked).
    pub lan_ip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RobotCloudPayload {
    /// Sign in and list the account's robots. `provider` names the vendor
    /// integration a package declares in `[robot.cloud_account]`; `region` is
    /// the vendor's account region ("global" | "cn" for Unitree).
    DevicesRequest {
        provider: String,
        region: String,
        email: String,
        password: String,
    },
    DevicesResponse {
        devices: Vec<RobotCloudDevice>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_body::MessageBody;

    #[test]
    fn devices_response_roundtrips() {
        let body = MessageBody::RobotCloudBody(RobotCloudPayload::DevicesResponse {
            devices: vec![RobotCloudDevice {
                serial: "B42D2000XXXXXXXX".into(),
                alias: "Go2".into(),
                model: "Go2".into(),
                series: "Air".into(),
                online: Some(true),
                aes_key: "00112233445566778899aabbccddeeff".into(),
                lan_ip: Some("192.168.50.250".into()),
            }],
        });
        let mut buf = Vec::new();
        ciborium::into_writer(&body, &mut buf).unwrap();
        let decoded: MessageBody = ciborium::from_reader(buf.as_slice()).unwrap();
        assert_eq!(decoded, body);
    }
}
