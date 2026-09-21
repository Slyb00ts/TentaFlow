// =============================================================================
// File: mesh/command_policy.rs
// Purpose: Privilege each mesh command requires from the node that sent it.
//          Trust is transitive (pairing with one node pairs the fleet), so
//          being trusted cannot by itself mean being allowed to reconfigure a
//          node. Commands that change the receiver need an operator node.
// =============================================================================

use tentaflow_protocol::mesh::MeshCommandType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandPrivilege {
    /// Any trusted peer: reads, probes and inference the fleet exists to share.
    Peer,
    /// A node on the receiver's operator list: anything that changes the
    /// receiver or makes it spend resources on the sender's say-so.
    Operator,
    /// The command carries a signed `SessionAssertion`; its handler verifies
    /// who is acting and applies that person's permissions.
    ActorAsserted,
}

/// No wildcard arm: a new command does not compile until it is classified.
pub fn required_privilege(command: &MeshCommandType) -> CommandPrivilege {
    use CommandPrivilege::{ActorAsserted, Operator, Peer};
    use MeshCommandType as C;

    match command {
        C::ContainerStart { .. }
        | C::ContainerStop { .. }
        | C::ContainerRestart { .. }
        | C::ContainerLogs { .. }
        | C::SystemPrune { .. }
        | C::ProvisionCerts { .. }
        | C::AddService { .. }
        | C::NetworkConfig { .. }
        | C::ProfilingStart(..)
        | C::ProfilingStop(..)
        | C::ProfilingDelete(..)
        | C::ProfilingDownload(..)
        | C::ServiceStartRemote { .. }
        | C::ServiceDeleteRemote { .. }
        | C::ServicePinRemote { .. }
        | C::ServicePauseRemote { .. }
        | C::ServiceDeployRemote { .. }
        | C::ServiceUpdateRemote { .. }
        | C::ServiceDeployDistributed { .. }
        | C::ServiceStopDistributed { .. }
        | C::DistributedStartServe { .. }
        | C::EnsureModelLocal { .. }
        | C::PushModelToPeer { .. }
        | C::OauthStart { .. }
        | C::OauthPoll { .. }
        | C::ConfigBundleExport
        | C::MlTrainStart { .. }
        | C::MlTrainCancel { .. }
        | C::MlDatasetChunk { .. }
        | C::MlExport { .. }
        | C::MlArtifactPushTo { .. }
        | C::CameraRecordingPull { .. } => Operator,

        C::CodeStudioOp { .. }
        | C::CodeStudioStreamPull { .. }
        | C::CodeStudioStreamOpen { .. }
        | C::AppRouteOp { .. } => ActorAsserted,

        C::ListContainers
        | C::ListImages
        | C::BandwidthProbe { .. }
        | C::BandwidthProbeCancel
        | C::RoceProbe
        | C::ProfilingSessions(..)
        | C::ProfilingReport(..)
        | C::ProfilingActiveInfo(..)
        | C::DistributedReadiness { .. }
        | C::ModelPresentLocal { .. }
        | C::CodeStudioAssertionKeysPush { .. }
        | C::CodeStudioAssertionKeysGet
        | C::CodeStudioPermissionProbe { .. }
        | C::MlTrainStatus { .. }
        | C::MlExportStatus { .. }
        | C::MlDetect { .. }
        | C::MlChat { .. }
        | C::WebResearch { .. }
        | C::VectorOp { .. }
        | C::CameraRecordingsList { .. }
        // Authorized per acting user on the owner node; see `handle_robot_control`.
        | C::RobotControl { .. } => Peer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_that_reconfigure_a_node_need_an_operator() {
        let commands = [
            MeshCommandType::SystemPrune { volumes: true },
            MeshCommandType::ConfigBundleExport,
            MeshCommandType::DistributedStartServe {
                deployment_cluster_id: "dep".to_string(),
            },
            MeshCommandType::ContainerStop {
                container_id: "c".to_string(),
            },
        ];
        for command in commands {
            assert_eq!(
                required_privilege(&command),
                CommandPrivilege::Operator,
                "{command:?}"
            );
        }
    }

    #[test]
    fn reads_and_probes_stay_open_to_every_trusted_peer() {
        for command in [
            MeshCommandType::ListContainers,
            MeshCommandType::RoceProbe,
            MeshCommandType::CodeStudioAssertionKeysGet,
        ] {
            assert_eq!(
                required_privilege(&command),
                CommandPrivilege::Peer,
                "{command:?}"
            );
        }
    }
}
