// ===== File: project_studio/repository.rs — CRUD for Project Studio registries =====
//
// Two data layers: the central registry (`projects.db`, reached through
// `db::pool()`) and per-project content (`project.db`, reached through a
// `DbPool` obtained from `project_db::open`). Core-directory lookups
// (`user_accounts`, `org_memberships`, `agents`) go through
// `crate::db::global_pool()` — separate SQLite files cannot be joined, so
// display names/emails are resolved per id.

use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, bail, Result};
use rusqlite::{params, OptionalExtension};

use super::models::{
    ActivityRecord, ChatRecord, CreatorGrantRecord, IndexedTask, IngestJobRecord, MemberInput,
    MemberRecord, ProjectDeletionAdmission, ProjectImportJournal, ProjectImportNode, ProjectKpis,
    ProjectRecord, ProjectTaskCounts, SourceFileRecord, SourceListItem, SourceRecord, TagRecord,
    TaskEventAlias, TaskIndexEvent, TaskIndexFilter, TaskIndexSnapshot, TaskIndexStatus,
    TaskKeyAlias, TaskLinkAlias, TaskLocation, TaskRelationRoute, TaskTransferJournal,
    TaskTypeRecord,
};
use crate::db::DbPool;
use tentaflow_protocol::project_studio::access::{
    ProjectAccessWire, ProjectArea, ProjectFunctionWire, ProjectPermissionLevel,
};
use tentaflow_protocol::project_studio::{ProjectTreeScope, TaskAggregateSummary};

fn read_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("project_studio db read: {e}")
}

#[cfg(test)]
mod central_index_tests {
    use super::*;

    #[test]
    fn staged_transfer_is_hidden_until_cutover_and_aliases_survive_two_moves() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _ = super::super::db::init(&tmp.path().join("projects.db"));
        std::mem::forget(tmp);
        let org = format!("org-{}", uuid::Uuid::new_v4());
        let ids: Vec<String> = (0..3).map(|_| uuid::Uuid::new_v4().to_string()).collect();
        for (id, prefix) in ids.iter().zip(["AAA", "BBB", "CCC"]) {
            create_project(
                id,
                &org,
                id,
                "",
                "custom",
                "[\"tasks\"]",
                "owner",
                "unused",
                prefix,
                None,
                false,
                false,
                false,
                &[],
            )
            .expect("project");
        }
        let task_id = uuid::Uuid::new_v4().to_string();
        let snapshot = |key: &str, id: &str| TaskIndexSnapshot {
            task_id: id.to_string(),
            task_no: 1,
            task_key: key.to_string(),
            task_type: "technical".into(),
            title: "Move me".into(),
            severity: String::new(),
            priority: "normal".into(),
            status: "todo".into(),
            assigned_to: String::new(),
            due_date: String::new(),
            parent_task_id: None,
            links_json: "[]".into(),
            comment_count: 0,
            created_by: "owner".into(),
            created_at: "2026-10-01T00:00:00Z".into(),
            updated_at: "2026-10-01T00:00:00Z".into(),
            archived_at: None,
            resolution: None,
            resolution_reason: None,
        };
        let event = |revision, key: &str, id: &str| TaskIndexEvent {
            revision,
            task_id: id.to_string(),
            op: "upsert".into(),
            snapshot_json: serde_json::to_string(&snapshot(key, id)).expect("snapshot"),
        };
        upsert_task_index_events(&ids[0], &org, &[event(1, "AAA-1", &task_id)])
            .expect("source projection");
        let filter = TaskIndexFilter {
            task_type: String::new(),
            status: String::new(),
            assigned_to: String::new(),
            search: String::new(),
            severity: String::new(),
            include_archived: false,
            offset: 0,
            limit: 50,
        };
        let visible = || list_indexed_tasks(&ids, &filter).expect("visible tasks").0;
        assert_eq!(
            visible()
                .iter()
                .map(|task| task.project_id.as_str())
                .collect::<Vec<_>>(),
            vec![ids[0].as_str()]
        );

        let transfer = |source: usize, destination: usize| TaskTransferJournal {
            operation_id: uuid::Uuid::new_v4().to_string(),
            org_id: org.clone(),
            source_project_id: ids[source].clone(),
            destination_project_id: ids[destination].clone(),
            actor_user_id: "owner".into(),
            task_ids: vec![task_id.clone()],
            consent_wider_access: true,
            phase: "prepared".into(),
            sha_manifest_json: "[]".into(),
            key_map_json: "{}".into(),
            event_map_json: "[]".into(),
        };
        let first = transfer(0, 1);
        prepare_task_transfer(&first).expect("prepare first move");
        stage_task_transfer_index(&first.operation_id, &[event(0, "BBB-1", &task_id)])
            .expect("stage first move");
        assert_eq!(visible().len(), 1);
        assert_eq!(visible()[0].project_id, ids[0]);
        mark_task_transfer_copied(&first.operation_id, "[]", "{}", "[]").expect("copy first move");
        publish_task_transfer(
            &first.operation_id,
            &[TaskLocation {
                task_id: task_id.clone(),
                org_id: org.clone(),
                project_id: ids[1].clone(),
                current_key: "BBB-1".into(),
            }],
            &[],
            &[],
            &[],
            &[],
        )
        .expect("publish first move");
        publish_task_transfer(&first.operation_id, &[], &[], &[], &[], &[])
            .expect("idempotent publish");
        assert_eq!(visible().len(), 1);
        assert_eq!(visible()[0].project_id, ids[1]);
        assert_eq!(
            resolve_task_key(&org, "AAA-1")
                .expect("old key")
                .expect("location")
                .current_key,
            "BBB-1"
        );
        mark_task_transfer_cleaned(&first.operation_id).expect("clean first move");

        replace_project_task_index(&ids[0], &org, 2, &[event(1, "AAA-1", &task_id)])
            .expect("source rebuild skips moved task");
        assert_eq!(visible().len(), 1);
        assert_eq!(visible()[0].project_id, ids[1]);

        let second = transfer(1, 2);
        prepare_task_transfer(&second).expect("prepare second move");
        stage_task_transfer_index(&second.operation_id, &[event(0, "CCC-1", &task_id)])
            .expect("stage second move");
        assert_eq!(visible()[0].project_id, ids[1]);
        mark_task_transfer_copied(&second.operation_id, "[]", "{}", "[]")
            .expect("copy second move");
        publish_task_transfer(
            &second.operation_id,
            &[TaskLocation {
                task_id: task_id.clone(),
                org_id: org.clone(),
                project_id: ids[2].clone(),
                current_key: "CCC-1".into(),
            }],
            &[],
            &[],
            &[],
            &[],
        )
        .expect("publish second move");
        for key in ["AAA-1", "BBB-1", "CCC-1"] {
            let location = resolve_task_key(&org, key)
                .expect("resolve")
                .expect("location");
            assert_eq!(location.project_id, ids[2]);
            assert_eq!(location.current_key, "CCC-1");
        }
        assert_eq!(visible().len(), 1);
        assert_eq!(visible()[0].project_id, ids[2]);
        assert!(
            upsert_task_index_events(&ids[0], &org, &[event(3, "AAA-1", "new-task")]).is_err(),
            "an old key must not be assigned to another task"
        );
        let delete_id = prepare_project_delete(&org, &ids[0], "owner").expect("delete admission");
        delete_project_rows(&ids[0], &delete_id).expect("delete old project");
        assert!(
            create_project(
                &uuid::Uuid::new_v4().to_string(),
                &org,
                "Replacement",
                "",
                "custom",
                "[]",
                "owner",
                "unused",
                "AAA",
                None,
                false,
                false,
                false,
                &[]
            )
            .is_err(),
            "a deleted project's prefix remains reserved"
        );
        assert_eq!(
            resolve_task_key(&org, "AAA-1")
                .expect("old key after project delete")
                .expect("moved task remains")
                .project_id,
            ids[2]
        );
        mark_task_transfer_cleaned(&second.operation_id).expect("clean second move");
        assert!(pending_task_transfers()
            .expect("fixture transfer journals")
            .iter()
            .all(|operation| operation.operation_id != first.operation_id
                && operation.operation_id != second.operation_id));
    }

    #[test]
    fn import_tree_reserves_names_and_rolls_back_failed_second_node() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _ = super::super::db::init(&tmp.path().join("projects.db"));
        std::mem::forget(tmp);
        let org = format!("org-{}", uuid::Uuid::new_v4());
        let existing = uuid::Uuid::new_v4().to_string();
        create_project(
            &existing,
            &org,
            "Archive",
            "",
            "custom",
            "[\"tasks\"]",
            "owner",
            "unused",
            "ARC",
            None,
            false,
            false,
            false,
            &[],
        )
        .expect("existing project");
        let root_id = uuid::Uuid::new_v4().to_string();
        let child_id = uuid::Uuid::new_v4().to_string();
        let mut functions = super::super::models::default_project_functions();
        let mut custom = functions[0].clone();
        custom.function_id = "incident_manager".into();
        custom.name = "Incident manager".into();
        custom.builtin = false;
        functions.push(custom);
        let node = |project_id: String, parent_id: Option<String>, name: &str, prefix: &str| {
            ProjectImportNode {
                project_id,
                parent_id,
                name: name.into(),
                description: String::new(),
                template: "custom".into(),
                modules_json: "[\"tasks\"]".into(),
                owner_user_id: "owner".into(),
                dir_path: "unused".into(),
                key_prefix: prefix.into(),
                is_private: false,
                inherit_modules: false,
                inherit_task_types: false,
                status: "active".into(),
                lifecycle: "active".into(),
                functions: functions.clone(),
                members: vec![MemberInput {
                    user_id: "member".into(),
                    functions: vec!["incident_manager".into()],
                    project_admin: false,
                    expires_at: None,
                }],
            }
        };
        let original = vec![
            node(root_id.clone(), None, "Archive", "ARC"),
            node(child_id.clone(), Some(root_id.clone()), "Child", "CH"),
        ];
        let operation_id = uuid::Uuid::new_v4().to_string();
        let resolved = prepare_project_tree_import(&operation_id, &org, &"a".repeat(64), &original)
            .expect("prepare import");
        assert_eq!(resolved[0].name, "Archive (2)");
        assert_ne!(resolved[0].key_prefix, "ARC");
        assert_eq!(
            pending_project_tree_imports()
                .expect("pending")
                .iter()
                .find(|item| item.operation_id == operation_id)
                .expect("journal")
                .nodes,
            resolved
        );
        assert!(
            create_project(
                &uuid::Uuid::new_v4().to_string(),
                &org,
                "Archive (2)",
                "",
                "custom",
                "[]",
                "owner",
                "unused",
                "OTHER",
                None,
                false,
                false,
                false,
                &[]
            )
            .is_err(),
            "a prepared name cannot be stolen"
        );
        let pool = super::super::db::pool().expect("central pool");
        pool.write()
            .expect("writer")
            .execute_batch(&format!(
                "CREATE TRIGGER fail_second_import BEFORE INSERT ON projects \
             WHEN NEW.project_id='{}' BEGIN SELECT RAISE(ABORT,'second import node fails'); END;",
                child_id,
            ))
            .expect("trigger");
        assert!(
            publish_project_tree_import(&operation_id, &org, &"a".repeat(64), &resolved).is_err()
        );
        assert!(get_project(&org, &root_id).expect("root query").is_none());
        assert!(get_project(&org, &child_id).expect("child query").is_none());
        pool.write()
            .expect("writer")
            .execute_batch("DROP TRIGGER fail_second_import;")
            .expect("drop trigger");
        publish_project_tree_import(&operation_id, &org, &"a".repeat(64), &resolved)
            .expect("publish complete tree");
        let child = get_project(&org, &child_id)
            .expect("child query")
            .expect("child");
        assert_eq!(child.path, format!("/{root_id}/{child_id}"));
        assert!(list_functions(&child_id)
            .expect("functions")
            .iter()
            .any(|function| function.function_id == "incident_manager"));
        assert_eq!(
            member_access(&child_id, "member")
                .expect("member")
                .expect("grant")
                .functions,
            vec!["incident_manager".to_string()]
        );
        assert!(pending_project_tree_imports()
            .expect("pending after publish")
            .iter()
            .all(|item| item.operation_id != operation_id));
    }

    #[test]
    fn relation_route_follows_target_then_source_without_scanning_other_projects() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _ = super::super::db::init(&tmp.path().join("projects.db"));
        std::mem::forget(tmp);
        let org = format!("org-{}", uuid::Uuid::new_v4());
        let ids: Vec<String> = (0..3).map(|_| uuid::Uuid::new_v4().to_string()).collect();
        for (id, prefix) in ids.iter().zip(["RAA", "RBB", "RCC"]) {
            create_project(
                id,
                &org,
                id,
                "",
                "custom",
                "[\"tasks\"]",
                "owner",
                "unused",
                prefix,
                None,
                false,
                false,
                false,
                &[],
            )
            .expect("project");
        }
        let source_task = uuid::Uuid::new_v4().to_string();
        let target_task = uuid::Uuid::new_v4().to_string();
        let task_event = |id: &str, key: &str, number: u32, revision: i64| TaskIndexEvent {
            revision,
            task_id: id.into(),
            op: "upsert".into(),
            snapshot_json: serde_json::to_string(&TaskIndexSnapshot {
                task_id: id.into(),
                task_no: number,
                task_key: key.into(),
                task_type: "technical".into(),
                title: key.into(),
                severity: String::new(),
                priority: "normal".into(),
                status: "todo".into(),
                assigned_to: String::new(),
                due_date: String::new(),
                parent_task_id: None,
                links_json: "[]".into(),
                comment_count: 0,
                created_by: "owner".into(),
                created_at: "2026-10-01T00:00:00Z".into(),
                updated_at: "2026-10-01T00:00:00Z".into(),
                archived_at: None,
                resolution: None,
                resolution_reason: None,
            })
            .expect("snapshot"),
        };
        upsert_task_index_events(&ids[0], &org, &[task_event(&source_task, "RAA-1", 1, 1)])
            .expect("source task");
        upsert_task_index_events(&ids[1], &org, &[task_event(&target_task, "RBB-1", 1, 1)])
            .expect("target task");
        let relation_id = uuid::Uuid::new_v4().to_string();
        let route = TaskRelationRoute {
            relation_id: relation_id.clone(),
            owning_project_id: ids[0].clone(),
            link_id: 0,
            source_task_id: source_task.clone(),
            target_task_id: target_task.clone(),
            source_project_id: ids[0].clone(),
            target_project_id: ids[1].clone(),
            kind: "fs".into(),
        };
        prepare_task_relation_route(&route).expect("admit relation");
        prepare_task_relation_route(&route).expect("same admission is recoverable");
        assert!(pending_task_relation_admissions()
            .expect("pending relations")
            .iter()
            .any(|item| item.relation_id == relation_id));
        let opposing = TaskRelationRoute {
            relation_id: uuid::Uuid::new_v4().to_string(),
            owning_project_id: ids[1].clone(),
            link_id: 0,
            source_task_id: target_task.clone(),
            target_task_id: source_task.clone(),
            source_project_id: ids[1].clone(),
            target_project_id: ids[0].clone(),
            kind: "ss".into(),
        };
        assert!(
            prepare_task_relation_route(&opposing).is_err(),
            "a pending opposite dependency must be rejected before source write"
        );
        let current = TaskRelationRoute {
            link_id: 1,
            ..route
        };
        publish_task_relation_route(&current).expect("publish relation");
        assert!(
            prepare_task_relation_route(&opposing).is_err(),
            "a published opposite dependency must be rejected"
        );

        let move_target = TaskTransferJournal {
            operation_id: uuid::Uuid::new_v4().to_string(),
            org_id: org.clone(),
            source_project_id: ids[1].clone(),
            destination_project_id: ids[2].clone(),
            actor_user_id: "owner".into(),
            task_ids: vec![target_task.clone()],
            consent_wider_access: true,
            phase: "prepared".into(),
            sha_manifest_json: "[]".into(),
            key_map_json: "{}".into(),
            event_map_json: "[]".into(),
        };
        prepare_task_transfer(&move_target).expect("prepare target move");
        stage_task_transfer_index(
            &move_target.operation_id,
            &[task_event(&target_task, "RCC-1", 1, 0)],
        )
        .expect("stage target");
        mark_task_transfer_copied(&move_target.operation_id, "[]", "{}", "[]")
            .expect("copy target");
        let after_target = TaskRelationRoute {
            target_project_id: ids[2].clone(),
            ..current.clone()
        };
        publish_task_transfer(
            &move_target.operation_id,
            &[TaskLocation {
                task_id: target_task.clone(),
                org_id: org.clone(),
                project_id: ids[2].clone(),
                current_key: "RCC-1".into(),
            }],
            &[],
            &[],
            &[],
            &[after_target.clone()],
        )
        .expect("publish target move");
        assert_eq!(
            relation_routes_for_task(&target_task).expect("incoming routes"),
            vec![after_target.clone()]
        );
        mark_task_transfer_cleaned(&move_target.operation_id).expect("clean target move");

        let move_source = TaskTransferJournal {
            operation_id: uuid::Uuid::new_v4().to_string(),
            org_id: org.clone(),
            source_project_id: ids[0].clone(),
            destination_project_id: ids[1].clone(),
            actor_user_id: "owner".into(),
            task_ids: vec![source_task.clone()],
            consent_wider_access: true,
            phase: "prepared".into(),
            sha_manifest_json: "[]".into(),
            key_map_json: "{}".into(),
            event_map_json: "[]".into(),
        };
        prepare_task_transfer(&move_source).expect("prepare source move");
        stage_task_transfer_index(
            &move_source.operation_id,
            &[task_event(&source_task, "RBB-2", 2, 0)],
        )
        .expect("stage source");
        mark_task_transfer_copied(&move_source.operation_id, "[]", "{}", "[]")
            .expect("copy source");
        let after_source = TaskRelationRoute {
            owning_project_id: ids[1].clone(),
            link_id: 2,
            source_project_id: ids[1].clone(),
            ..after_target
        };
        publish_task_transfer(
            &move_source.operation_id,
            &[TaskLocation {
                task_id: source_task.clone(),
                org_id: org.clone(),
                project_id: ids[1].clone(),
                current_key: "RBB-2".into(),
            }],
            &[],
            &[],
            &[TaskLinkAlias {
                relation_id: relation_id.clone(),
                origin_project_id: ids[0].clone(),
                origin_link_id: 1,
                current_project_id: ids[1].clone(),
                current_link_id: 2,
            }],
            &[after_source.clone()],
        )
        .expect("publish source move");
        assert_eq!(
            relation_routes_for_task(&target_task).expect("incoming after source"),
            vec![after_source]
        );
        let old_link = resolve_task_link(&ids[0], 1)
            .expect("old link resolve")
            .expect("route");
        assert_eq!(
            (old_link.current_project_id, old_link.current_link_id),
            (ids[1].clone(), 2)
        );
        mark_task_transfer_cleaned(&move_source.operation_id).expect("clean source move");
        assert!(pending_task_transfers()
            .expect("fixture transfer journals")
            .iter()
            .all(
                |operation| operation.operation_id != move_target.operation_id
                    && operation.operation_id != move_source.operation_id
            ));
    }
}

fn write_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("project_studio db write: {e}")
}

/// Escapes LIKE wildcards so user input matches literally. Every query using
/// the result must declare `ESCAPE '\'` on the LIKE clause.
fn escape_like(input: &str) -> String {
    input
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

// =============================================================================
// Central registry: projects
// =============================================================================

fn read_project(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectRecord> {
    Ok(ProjectRecord {
        project_id: row.get(0)?,
        org_id: row.get(1)?,
        name: row.get(2)?,
        key_prefix: row.get(11)?,
        description: row.get(3)?,
        status: row.get(4)?,
        template: row.get(5)?,
        modules_json: row.get(6)?,
        owner_user_id: row.get(7)?,
        dir_path: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
        parent_id: row.get(12)?,
        path: row.get(13)?,
        depth: row.get(14)?,
        is_private: row.get(15)?,
        inherit_modules: row.get(16)?,
        inherit_task_types: row.get(17)?,
        module_disabled_json: row.get(18)?,
        lifecycle: row.get(19)?,
        ended_at: row.get(20)?,
    })
}

const PROJECT_COLS: &str = "project_id, org_id, name, description, status, template, \
     modules_json, owner_user_id, dir_path, created_at, updated_at, key_prefix, \
     parent_id, path, depth, is_private, inherit_modules, inherit_task_types, \
     module_disabled_json, lifecycle, ended_at";

fn import_name_reserved(
    conn: &rusqlite::Connection,
    org_id: &str,
    parent_id: Option<&str>,
    name: &str,
) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM project_import_reservations \
         WHERE org_id=?1 AND parent_id IS ?2 AND name=?3)",
        params![org_id, parent_id, name],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

/// Inserts the project row together with the owner membership and any initial
/// members in ONE transaction — a project can never exist without its owner.
#[allow(clippy::too_many_arguments)]
pub fn create_project(
    project_id: &str,
    org_id: &str,
    name: &str,
    description: &str,
    template: &str,
    modules_json: &str,
    owner_user_id: &str,
    dir_path: &str,
    key_prefix: &str,
    parent_id: Option<&str>,
    is_private: bool,
    inherit_modules: bool,
    inherit_task_types: bool,
    members: &[MemberInput],
) -> Result<()> {
    let catalogue = super::models::default_project_functions();
    let members = members
        .iter()
        .map(|member| super::models::validate_member_input(member, &catalogue, chrono::Utc::now()))
        .collect::<Result<Vec<_>>>()?;
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let prefix = super::db::reserve_key_prefix(&tx, org_id, name, key_prefix)?;
    if import_name_reserved(&tx, org_id, parent_id, name)? {
        bail!("project name is reserved by import");
    }
    let (path, depth) = if let Some(parent_id) = parent_id {
        let parent = tx
            .query_row(
                &format!("SELECT {PROJECT_COLS} FROM projects WHERE org_id=?1 AND project_id=?2"),
                params![org_id, parent_id],
                read_project,
            )
            .optional()?
            .ok_or_else(|| anyhow!("parent project not found"))?;
        if parent.status != "active" || parent.lifecycle != "active" {
            bail!("parent project is not active");
        }
        let admission: i64 = tx.query_row(
            "SELECT COUNT(*) FROM project_admissions WHERE project_id=?1",
            [parent_id],
            |row| row.get(0),
        )?;
        if admission > 0 {
            bail!("parent project is preparing to end");
        }
        if parent.depth >= 4 {
            bail!("project tree cannot exceed four levels");
        }
        (format!("{}/{}", parent.path, project_id), parent.depth + 1)
    } else {
        (format!("/{project_id}"), 1)
    };
    tx.execute(
        "INSERT INTO projects (project_id, org_id, name, description, template, \
         modules_json, owner_user_id, dir_path, key_prefix, parent_id, path, depth, \
         is_private, inherit_modules, inherit_task_types) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            project_id,
            org_id,
            name,
            description,
            template,
            modules_json,
            owner_user_id,
            dir_path,
            prefix,
            parent_id,
            path,
            depth,
            is_private,
            inherit_modules,
            inherit_task_types,
        ],
    )?;
    tx.execute(
        "INSERT INTO project_prefix_reservations(org_id,key_prefix,project_id) VALUES (?1,?2,?3)",
        params![org_id, prefix, project_id],
    )?;
    super::db::seed_project_functions(&tx, project_id)?;
    tx.execute(
        "INSERT INTO project_members (project_id, user_id, project_admin, invited_by) \
         VALUES (?1, ?2, 1, ?2)",
        params![project_id, owner_user_id],
    )?;
    for function in &catalogue {
        tx.execute(
            "INSERT INTO project_member_functions(project_id,user_id,function_id) VALUES (?1,?2,?3)",
            params![project_id,owner_user_id,function.function_id],
        )?;
    }
    for member in &members {
        if member.user_id == owner_user_id {
            continue;
        }
        insert_member(&tx, project_id, member, owner_user_id)?;
    }
    tx.commit()?;
    Ok(())
}

pub fn get_project(org_id: &str, project_id: &str) -> Result<Option<ProjectRecord>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        &format!("SELECT {PROJECT_COLS} FROM projects WHERE org_id = ?1 AND project_id = ?2"),
        params![org_id, project_id],
        read_project,
    )
    .optional()
    .map_err(Into::into)
}

pub(crate) fn project_record(project_id: &str) -> Result<Option<ProjectRecord>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        &format!("SELECT {PROJECT_COLS} FROM projects WHERE project_id=?1"),
        [project_id],
        read_project,
    )
    .optional()
    .map_err(Into::into)
}

pub(crate) fn all_project_records() -> Result<Vec<ProjectRecord>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {PROJECT_COLS} FROM projects p WHERE NOT EXISTS (\
         SELECT 1 FROM project_admissions a WHERE a.project_id=p.project_id AND a.kind='delete') \
         ORDER BY p.project_id"
    ))?;
    let rows = stmt.query_map([], read_project)?;
    let projects = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(projects)
}

pub fn list_projects(org_id: &str, include_archived: bool) -> Result<Vec<ProjectRecord>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let sql = if include_archived {
        format!("SELECT {PROJECT_COLS} FROM projects WHERE org_id = ?1 ORDER BY updated_at DESC")
    } else {
        format!(
            "SELECT {PROJECT_COLS} FROM projects WHERE org_id = ?1 AND status = 'active' \
             ORDER BY updated_at DESC"
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![org_id], read_project)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn list_descendants(
    org_id: &str,
    project_id: &str,
    include_self: bool,
) -> Result<Vec<ProjectRecord>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let ancestors = project_ancestry_on(&conn, org_id, project_id)?;
    let root = &ancestors
        .last()
        .ok_or_else(|| anyhow!("project not found"))?
        .path;
    let mut stmt = conn.prepare(&format!(
        "SELECT {PROJECT_COLS} FROM projects WHERE org_id=?1 AND \
         (path=?2 OR substr(path,1,length(?2)+1)=?2 || '/') ORDER BY depth,path"
    ))?;
    let rows = stmt.query_map(params![org_id, root], read_project)?;
    let mut descendants = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    if !include_self {
        descendants.retain(|project| project.project_id != project_id);
    }
    Ok(descendants)
}

pub fn move_project(org_id: &str, project_id: &str, new_parent_id: Option<&str>) -> Result<bool> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let project = project_ancestry_on(&tx, org_id, project_id)?;
    let current = project.last().ok_or_else(|| anyhow!("project not found"))?;
    if current.parent_id.as_deref() == new_parent_id {
        return Ok(false);
    }
    if import_name_reserved(&tx, org_id, new_parent_id, &current.name)? {
        bail!("destination sibling name is reserved by import");
    }
    if project
        .iter()
        .any(|node| node.lifecycle != "active" || node.status != "active")
    {
        bail!("ended or archived project cannot be moved");
    }
    let destination = new_parent_id
        .map(|id| project_ancestry_on(&tx, org_id, id))
        .transpose()?;
    let destination_path = if let Some(path) = destination.as_ref() {
        if path
            .iter()
            .any(|node| node.lifecycle != "active" || node.status != "active")
        {
            bail!("destination parent is not active");
        }
        if path.iter().any(|node| node.project_id == project_id) {
            bail!("project cannot move into its descendant");
        }
        for node in path {
            let blocked: i64 = tx.query_row(
                "SELECT COUNT(*) FROM project_admissions WHERE project_id=?1",
                [&node.project_id],
                |row| row.get(0),
            )?;
            if blocked > 0 {
                bail!("destination parent is preparing to end");
            }
        }
        path.last()
            .ok_or_else(|| anyhow!("destination parent missing"))?
            .path
            .clone()
    } else {
        String::new()
    };
    let new_depth = destination.as_ref().map_or(1, |path| path.len() as u32 + 1);
    let old_path = current.path.clone();
    let subtree: Vec<(String, u32, String)> = {
        let mut stmt = tx.prepare(
            "SELECT project_id,depth,lifecycle FROM projects WHERE org_id=?1 AND \
             (path=?2 OR substr(path,1,length(?2)+1)=?2 || '/') ORDER BY depth,path",
        )?;
        let rows = stmt.query_map(params![org_id, old_path], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        let subtree = rows.collect::<rusqlite::Result<_>>()?;
        subtree
    };
    if subtree
        .iter()
        .any(|(_, _, lifecycle)| lifecycle != "active")
    {
        bail!("ended project cannot be moved");
    }
    for (id, _, _) in &subtree {
        let blocked: i64 = tx.query_row(
            "SELECT COUNT(*) FROM project_admissions WHERE project_id=?1",
            [id],
            |row| row.get(0),
        )?;
        if blocked > 0 {
            bail!("project is preparing to end");
        }
    }
    let moving_project_ids: Vec<&str> = subtree.iter().map(|(id, _, _)| id.as_str()).collect();
    for id in moving_project_ids {
        let transferring: i64 = tx.query_row(
            "SELECT COUNT(*) FROM task_transfer_journal WHERE phase<>'cleaned' \
             AND (source_project_id=?1 OR destination_project_id=?1)",
            [id],
            |row| row.get(0),
        )?;
        if transferring != 0 {
            bail!("project has a pending task transfer");
        }
    }
    let max_subtree_depth = subtree
        .iter()
        .map(|(_, depth, _)| *depth)
        .max()
        .unwrap_or(current.depth);
    if max_subtree_depth - current.depth + new_depth > 4 {
        bail!("project tree cannot exceed four levels");
    }
    let old_private = project
        .iter()
        .rfind(|node| node.is_private)
        .map(|node| node.project_id.as_str());
    if let Some(private_id) = old_private {
        if private_id != project_id
            && !destination
                .as_ref()
                .is_some_and(|path| path.iter().any(|node| node.project_id == private_id))
            && !current.is_private
        {
            bail!("moving outside a private branch would widen access");
        }
    }
    let new_path = if destination_path.is_empty() {
        format!("/{project_id}")
    } else {
        format!("{destination_path}/{project_id}")
    };
    tx.execute(
        "UPDATE projects SET parent_id=?1 WHERE project_id=?2",
        params![new_parent_id, project_id],
    )?;
    tx.execute(
        "UPDATE projects SET path=?1 || substr(path,length(?2)+1), \
         depth=depth+?3, updated_at=datetime('now') WHERE org_id=?4 AND \
         (path=?2 OR substr(path,1,length(?2)+1)=?2 || '/')",
        params![
            new_path,
            old_path,
            new_depth as i64 - current.depth as i64,
            org_id
        ],
    )?;
    tx.commit()?;
    drop(conn);
    let mut catalogue_sources = HashSet::new();
    for (id, _, _) in &subtree {
        catalogue_sources.insert(effective_project_settings(id)?.1);
    }
    for source_id in catalogue_sources {
        let source = project_record(&source_id)?
            .ok_or_else(|| anyhow!("moved project task catalogue source missing"))?;
        let source_pool = super::project_db::open(&source_id)?;
        super::task_index::sync_catalogue(&source, &source_pool)?;
    }
    reconcile_project_mirrors(project_id);
    Ok(true)
}

/// Renames the project. A `UNIQUE(org_id, name)` violation surfaces as
/// `Err` whose message contains "UNIQUE" — the dispatcher maps it to
/// BadRequest.
pub fn update_project_name_desc(
    org_id: &str,
    project_id: &str,
    name: &str,
    description: &str,
    key_prefix: Option<&str>,
) -> Result<bool> {
    let content = super::project_db::open(project_id)?;
    let content_conn = content.write().map_err(write_err)?;
    let content_tx = content_conn.unchecked_transaction()?;
    content_tx.execute(
        "UPDATE settings SET value = value WHERE key = 'project_key_prefix'",
        [],
    )?;
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let existing: Option<(String, Option<String>)> = tx
        .query_row(
            "SELECT key_prefix,parent_id FROM projects WHERE org_id = ?1 AND project_id = ?2",
            params![org_id, project_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((existing, parent_id)) = existing else {
        return Ok(false);
    };
    if import_name_reserved(&tx, org_id, parent_id.as_deref(), name)? {
        bail!("project name is reserved by import");
    }
    let prefix = match key_prefix {
        Some(value) => super::db::normalize_key_prefix(value)
            .ok_or_else(|| anyhow!("key prefix must match [A-Z][A-Z0-9]{{1,7}}"))?,
        None => existing.clone(),
    };
    if prefix != existing {
        let locked: String = content_tx.query_row(
            "SELECT value FROM settings WHERE key = 'project_key_prefix_locked'",
            [],
            |row| row.get(0),
        )?;
        if locked == "1" {
            return Err(anyhow!("project key prefix is locked after the first task"));
        }
        let taken: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM project_prefix_reservations WHERE org_id = ?1 AND key_prefix = ?2 AND project_id <> ?3",
                params![org_id, prefix, project_id],
                |row| row.get(0),
            )
            .optional()?;
        if taken.is_some() {
            return Err(anyhow!("key prefix already exists in organization"));
        }
        let importing: i64 = tx.query_row(
            "SELECT COUNT(*) FROM project_import_reservations WHERE org_id=?1 AND key_prefix=?2",
            params![org_id, prefix],
            |row| row.get(0),
        )?;
        if importing != 0 {
            bail!("key prefix is reserved by project import");
        }
    }
    let n = tx.execute(
        "UPDATE projects SET name = ?1, description = ?2, key_prefix = ?3, updated_at = datetime('now') \
         WHERE org_id = ?4 AND project_id = ?5",
        params![name, description, prefix, org_id, project_id],
    )?;
    if prefix != existing {
        tx.execute(
            "INSERT OR IGNORE INTO project_prefix_reservations(org_id,key_prefix,project_id) VALUES (?1,?2,?3)",
            params![org_id,prefix,project_id],
        )?;
    }
    if prefix != existing {
        content_tx.execute(
            "UPDATE settings SET value = ?1 WHERE key = 'project_key_prefix'",
            params![prefix],
        )?;
        content_tx.execute(
            "UPDATE tasks SET task_key = ?1 || '-' || task_no WHERE task_key <> ?1 || '-' || task_no",
            params![prefix],
        )?;
    }
    tx.commit()?;
    content_tx.commit()?;
    Ok(n > 0)
}

/// Replaces the enabled-module set. Modules only gate which tabs/handlers a
/// project exposes, so switching one off keeps every row it produced — the data
/// reappears untouched once the module is switched back on.
pub fn update_project_modules(org_id: &str, project_id: &str, modules_json: &str) -> Result<bool> {
    let requested: Vec<String> = serde_json::from_str(modules_json)?;
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let ancestry = project_ancestry_on(&tx, org_id, project_id)?;
    let project = ancestry
        .last()
        .ok_or_else(|| anyhow!("project not found"))?;
    if project.status != "active" || project.lifecycle != "active" {
        bail!("project is read-only");
    }
    let n = if project.inherit_modules {
        let parent_modules = effective_modules_on(&ancestry[..ancestry.len() - 1])?;
        if requested
            .iter()
            .any(|module| !parent_modules.contains(module))
        {
            bail!("inherited project cannot enable a module disabled by its parent");
        }
        let disabled: Vec<&String> = parent_modules
            .iter()
            .filter(|module| !requested.contains(module))
            .collect();
        tx.execute(
            "UPDATE projects SET module_disabled_json=?1,updated_at=datetime('now') WHERE org_id=?2 AND project_id=?3",
            params![serde_json::to_string(&disabled)?,org_id,project_id],
        )?
    } else {
        tx.execute(
            "UPDATE projects SET modules_json=?1,module_disabled_json='[]',updated_at=datetime('now') \
             WHERE org_id=?2 AND project_id=?3",
            params![modules_json,org_id,project_id],
        )?
    };
    tx.commit()?;
    drop(conn);
    if n > 0 {
        reconcile_project_mirrors(project_id);
    }
    Ok(n > 0)
}

pub fn save_project_inheritance(
    org_id: &str,
    project_id: &str,
    is_private: bool,
    inherit_modules: bool,
    inherit_task_types: bool,
    modules_json: &str,
    expected_type_source_project_id: &str,
) -> Result<bool> {
    let requested: Vec<String> = serde_json::from_str(modules_json)?;
    let initial = get_project(org_id, project_id)?.ok_or_else(|| anyhow!("project not found"))?;
    let (_, initial_source) = effective_project_settings(project_id)?;
    if initial_source != expected_type_source_project_id {
        bail!("task type source changed; refresh inheritance preview");
    }
    let materialize = initial.inherit_task_types && !inherit_task_types;
    let reinherit = !initial.inherit_task_types && inherit_task_types;
    let catalogue_source_id = if reinherit {
        let parent_id = initial
            .parent_id
            .as_deref()
            .ok_or_else(|| anyhow!("root project cannot inherit task types"))?;
        effective_project_settings(parent_id)?.1
    } else {
        expected_type_source_project_id.to_string()
    };
    let source_pool = if materialize || reinherit {
        Some(super::project_db::open(&catalogue_source_id)?)
    } else {
        None
    };
    let target_pool = if materialize || reinherit {
        Some(super::project_db::open(project_id)?)
    } else {
        None
    };
    let mut source_guard = None;
    let mut target_guard = None;
    if let (Some(source_pool), Some(target_pool)) = (&source_pool, &target_pool) {
        if catalogue_source_id == project_id {
            bail!("task catalogue source cannot be the target during inheritance change");
        }
        if catalogue_source_id.as_str() < project_id {
            source_guard = Some(source_pool.write().map_err(write_err)?);
            target_guard = Some(target_pool.write().map_err(write_err)?);
        } else {
            target_guard = Some(target_pool.write().map_err(write_err)?);
            source_guard = Some(source_pool.write().map_err(write_err)?);
        }
        if materialize {
            super::tasks::materialize_task_type_catalogue(
                source_guard.as_ref().expect("source lock"),
                target_guard.as_ref().expect("target lock"),
            )?;
        } else {
            super::tasks::validate_reinherited_task_types(
                source_guard.as_ref().expect("source lock"),
                target_guard.as_ref().expect("target lock"),
            )?;
        }
    }
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let ancestry = project_ancestry_on(&tx, org_id, project_id)?;
    let project = ancestry
        .last()
        .ok_or_else(|| anyhow!("project not found"))?;
    if project.inherit_task_types != initial.inherit_task_types
        || (project.inherit_task_types && !inherit_task_types) != materialize
    {
        bail!("project inheritance changed; refresh settings");
    }
    if project.status != "active" || project.lifecycle != "active" {
        bail!("project is read-only");
    }
    if project.parent_id.is_none() && (inherit_modules || inherit_task_types) {
        bail!("root project cannot inherit settings");
    }
    let source = ancestry
        .iter()
        .rev()
        .find(|node| !node.inherit_task_types || node.parent_id.is_none())
        .ok_or_else(|| anyhow!("task type source not found"))?;
    if source.project_id != expected_type_source_project_id {
        bail!("task type source changed; refresh inheritance preview");
    }
    if reinherit {
        let parent_id = project
            .parent_id
            .as_deref()
            .ok_or_else(|| anyhow!("root project cannot inherit task types"))?;
        let parent_ancestry = project_ancestry_on(&tx, org_id, parent_id)?;
        let proposed_source = parent_ancestry
            .iter()
            .rev()
            .find(|node| !node.inherit_task_types || node.parent_id.is_none())
            .ok_or_else(|| anyhow!("inherited task type source not found"))?;
        if proposed_source.project_id != catalogue_source_id {
            bail!("inherited task type source changed; refresh settings");
        }
    }
    let disabled_json = if inherit_modules {
        let parent_modules = effective_modules_on(&ancestry[..ancestry.len() - 1])?;
        if requested
            .iter()
            .any(|module| !parent_modules.contains(module))
        {
            bail!("inherited project cannot enable a module disabled by its parent");
        }
        serde_json::to_string(
            &parent_modules
                .iter()
                .filter(|module| !requested.contains(module))
                .collect::<Vec<_>>(),
        )?
    } else {
        "[]".to_string()
    };
    let n = tx.execute(
        "UPDATE projects SET is_private=?1,inherit_modules=?2,inherit_task_types=?3, \
         modules_json=CASE WHEN ?2=0 THEN ?4 ELSE modules_json END, \
         module_disabled_json=?5,updated_at=datetime('now') WHERE org_id=?6 AND project_id=?7",
        params![
            is_private,
            inherit_modules,
            inherit_task_types,
            modules_json,
            disabled_json,
            org_id,
            project_id
        ],
    )?;
    tx.commit()?;
    drop(conn);
    drop(source_guard);
    drop(target_guard);
    if n > 0 {
        if let Some(target_pool) = target_pool.as_ref() {
            let current = project_record(project_id)?
                .ok_or_else(|| anyhow!("project disappeared after inheritance change"))?;
            super::task_index::sync_catalogue(&current, target_pool)?;
        }
        reconcile_project_mirrors(project_id);
    }
    Ok(n > 0)
}

pub fn set_project_archived(
    org_id: &str,
    project_id: &str,
    archived: bool,
    scope: ProjectTreeScope,
    expected_project_ids: &[String],
) -> Result<bool> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let ancestry = project_ancestry_on(&tx, org_id, project_id)?;
    let path = &ancestry
        .last()
        .ok_or_else(|| anyhow!("project not found"))?
        .path;
    let mut stmt = tx.prepare(
        "SELECT project_id FROM projects WHERE org_id=?1 AND \
         (path=?2 OR substr(path,1,length(?2)+1)=?2 || '/') ORDER BY project_id",
    )?;
    let all_ids = stmt
        .query_map(params![org_id, path], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    if scope == ProjectTreeScope::Node && all_ids.len() != 1 {
        bail!("project has child projects; select subtree scope");
    }
    let mut expected = expected_project_ids.to_vec();
    expected.sort();
    expected.dedup();
    let selected = if scope == ProjectTreeScope::Node {
        vec![project_id.to_string()]
    } else {
        all_ids
    };
    if selected != expected {
        bail!("project subtree changed; refresh scope preview");
    }
    let status = if archived { "archived" } else { "active" };
    let mut changed = 0;
    for id in &selected {
        changed += tx.execute(
            "UPDATE projects SET status=?1,updated_at=datetime('now') WHERE org_id=?2 AND project_id=?3 AND status<>?1",
            params![status,org_id,id],
        )?;
    }
    tx.commit()?;
    drop(conn);
    if changed > 0 {
        reconcile_project_mirrors(project_id);
    }
    Ok(changed > 0)
}

pub fn prepare_project_delete(
    org_id: &str,
    project_id: &str,
    actor_user_id: &str,
) -> Result<String> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let project = project_ancestry_on(&tx, org_id, project_id)?;
    let existing: Option<(String, String, String)> = tx
        .query_row(
            "SELECT operation_id,kind,actor_user_id FROM project_admissions WHERE project_id=?1",
            [project_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if let Some((operation_id, kind, actor)) = existing {
        if kind == "delete" && actor == actor_user_id {
            return Ok(operation_id);
        }
        bail!("project has another lifecycle operation");
    }
    let target = project.last().ok_or_else(|| anyhow!("project not found"))?;
    if target.parent_id.is_some() && target.lifecycle != "ended" {
        bail!("subproject must be ended before deletion");
    }
    let children: i64 = tx.query_row(
        "SELECT COUNT(*) FROM projects WHERE parent_id=?1",
        [project_id],
        |row| row.get(0),
    )?;
    if children != 0 {
        bail!("project with children cannot be deleted");
    }
    let transfers: i64 = tx.query_row(
        "SELECT COUNT(*) FROM task_transfer_journal WHERE phase<>'cleaned' \
         AND (source_project_id=?1 OR destination_project_id=?1)",
        [project_id],
        |row| row.get(0),
    )?;
    if transfers != 0 {
        bail!("project has an unfinished task transfer");
    }
    for ancestor in project {
        let preparing: i64 = tx.query_row(
            "SELECT COUNT(*) FROM project_admissions WHERE project_id=?1",
            [&ancestor.project_id],
            |row| row.get(0),
        )?;
        if preparing != 0 {
            bail!("project has another lifecycle operation");
        }
    }
    let operation_id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO project_admissions(project_id,operation_id,kind,actor_user_id,reason) \
         VALUES (?1,?2,'delete',?3,'')",
        params![project_id, operation_id, actor_user_id],
    )?;
    tx.commit()?;
    Ok(operation_id)
}

pub fn abort_project_delete(project_id: &str, operation_id: &str) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "DELETE FROM project_admissions WHERE project_id=?1 AND operation_id=?2 AND kind='delete'",
        params![project_id, operation_id],
    )?;
    Ok(())
}

pub fn pending_project_deletions() -> Result<Vec<ProjectDeletionAdmission>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(
        "SELECT p.project_id,p.org_id,p.name,p.description,p.status,p.template,p.modules_json,\
         p.owner_user_id,p.dir_path,p.created_at,p.updated_at,p.key_prefix,p.parent_id,p.path,\
         p.depth,p.is_private,p.inherit_modules,p.inherit_task_types,p.module_disabled_json,\
         p.lifecycle,p.ended_at,a.operation_id,a.actor_user_id FROM projects p \
         JOIN project_admissions a ON a.project_id=p.project_id WHERE a.kind='delete' \
         ORDER BY a.created_at,a.operation_id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(ProjectDeletionAdmission {
            project: read_project(row)?,
            operation_id: row.get(21)?,
            actor_user_id: row.get(22)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

/// The caller removes project content only after the durable delete admission
/// exists; the fence prevents new children and writes until this commit.
pub fn delete_project_rows(project_id: &str, operation_id: &str) -> Result<()> {
    {
        let pool = super::db::pool()?;
        let conn = pool.write().map_err(write_err)?;
        let tx = conn.unchecked_transaction()?;
        let fenced: i64 = tx.query_row(
            "SELECT COUNT(*) FROM project_admissions WHERE project_id=?1 AND operation_id=?2 AND kind='delete'",
            params![project_id,operation_id], |row| row.get(0),
        )?;
        if fenced != 1 {
            bail!("project delete admission not found");
        }
        let project: Option<(Option<String>, String)> = tx
            .query_row(
                "SELECT parent_id,lifecycle FROM projects WHERE project_id=?1",
                [project_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((parent_id, lifecycle)) = project else {
            bail!("project not found");
        };
        let children: i64 = tx.query_row(
            "SELECT COUNT(*) FROM projects WHERE parent_id=?1",
            [project_id],
            |row| row.get(0),
        )?;
        if children > 0 {
            bail!("project with children cannot be deleted");
        }
        if parent_id.is_some() && lifecycle != "ended" {
            bail!("subproject must be ended before deletion");
        }
        let active_transfer: i64 = tx.query_row(
            "SELECT COUNT(*) FROM task_transfer_journal WHERE phase<>'cleaned' \
             AND (source_project_id=?1 OR destination_project_id=?1)",
            [project_id],
            |row| row.get(0),
        )?;
        if active_transfer > 0 {
            bail!("project has an unfinished task transfer");
        }
        tx.execute("DELETE FROM task_index WHERE project_id=?1", [project_id])?;
        tx.execute(
            "DELETE FROM task_index_cursor WHERE project_id=?1",
            [project_id],
        )?;
        tx.execute(
            "DELETE FROM task_type_names WHERE source_project_id=?1",
            [project_id],
        )?;
        tx.execute(
            "DELETE FROM task_type_catalogue_cursor WHERE source_project_id=?1",
            [project_id],
        )?;
        tx.execute(
            "DELETE FROM task_locations WHERE project_id=?1",
            [project_id],
        )?;
        tx.execute(
            "DELETE FROM project_schedule_hints WHERE project_id=?1",
            [project_id],
        )?;
        tx.execute(
            "DELETE FROM project_members WHERE project_id = ?1",
            params![project_id],
        )?;
        tx.execute(
            "DELETE FROM project_chats WHERE project_id = ?1",
            params![project_id],
        )?;
        tx.execute(
            "DELETE FROM notifications WHERE project_id = ?1",
            params![project_id],
        )?;
        tx.execute(
            "DELETE FROM project_functions WHERE project_id = ?1",
            params![project_id],
        )?;
        tx.execute(
            "DELETE FROM projects WHERE project_id = ?1",
            params![project_id],
        )?;
        tx.execute(
            "DELETE FROM project_admissions WHERE project_id=?1",
            [project_id],
        )?;
        tx.commit()?;
    }
    // The project has no members left, so the mirror reads an empty list and
    // takes back exactly the workspace memberships it granted for it.
    reconcile_project_mirrors(project_id);
    Ok(())
}

pub fn touch_project(project_id: &str) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "UPDATE projects SET updated_at = datetime('now') WHERE project_id = ?1",
        params![project_id],
    )?;
    Ok(())
}

// =============================================================================
// Central registry: members
// =============================================================================

fn list_functions_on(
    conn: &rusqlite::Connection,
    project_id: &str,
) -> Result<Vec<ProjectFunctionWire>> {
    let mut stmt = conn.prepare(
        "SELECT function_id,name,description,builtin,grants_json FROM project_functions \
         WHERE project_id=?1 ORDER BY position,function_id",
    )?;
    let rows = stmt.query_map(params![project_id], |row| {
        let grants_json: String = row.get(4)?;
        let grants = serde_json::from_str(&grants_json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?;
        Ok(ProjectFunctionWire {
            function_id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2)?,
            builtin: row.get(3)?,
            grants,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

pub fn list_functions(project_id: &str) -> Result<Vec<ProjectFunctionWire>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    list_functions_on(&conn, project_id)
}

fn validate_function(function: &ProjectFunctionWire) -> Result<()> {
    let id = function.function_id.as_bytes();
    if id.is_empty()
        || id.len() > 64
        || !id[0].is_ascii_lowercase() && !id[0].is_ascii_digit()
        || !id.iter().all(|value| {
            value.is_ascii_lowercase() || value.is_ascii_digit() || *value == b'_' || *value == b'-'
        })
    {
        bail!("invalid project function identifier");
    }
    if function.name.trim().is_empty() || function.name.chars().count() > 128 {
        bail!("project function name must contain 1 to 128 characters");
    }
    if function.description.chars().count() > 2048 {
        bail!("project function description exceeds 2048 characters");
    }
    let areas = function
        .grants
        .iter()
        .map(|grant| grant.area)
        .collect::<std::collections::HashSet<_>>();
    if areas.len() != tentaflow_protocol::project_studio::access::ProjectArea::ALL.len()
        || function.grants.len() != areas.len()
    {
        bail!("project function requires exactly one grant for every area");
    }
    Ok(())
}

pub(super) fn validate_function_catalogue(functions: &[ProjectFunctionWire]) -> Result<()> {
    let defaults = super::models::default_project_functions();
    if functions.len() < defaults.len() || functions.len() > 64 {
        bail!("invalid project function catalogue size");
    }
    let mut ids = std::collections::HashSet::new();
    for function in functions {
        validate_function(function)?;
        if !ids.insert(&function.function_id) {
            bail!("duplicate project function identifier");
        }
        let builtin = defaults
            .iter()
            .any(|entry| entry.function_id == function.function_id);
        if builtin != function.builtin {
            bail!("builtin project function identity cannot be changed");
        }
    }
    if defaults
        .iter()
        .any(|entry| !ids.contains(&entry.function_id))
    {
        bail!("project function catalogue is missing a builtin function");
    }
    Ok(())
}

pub fn save_function(project_id: &str, function: &ProjectFunctionWire) -> Result<bool> {
    validate_function(function)?;
    {
        let pool = super::db::pool()?;
        let conn = pool.write().map_err(write_err)?;
        let catalogue = list_functions_on(&conn, project_id)?;
        let existing = catalogue
            .iter()
            .find(|entry| entry.function_id == function.function_id);
        if existing.is_none() && catalogue.len() >= 64 {
            bail!("project function catalogue is limited to 64 entries");
        }
        if function.builtin != existing.is_some_and(|entry| entry.builtin) {
            bail!("builtin project function identity cannot be changed");
        }
        conn.execute(
            "INSERT INTO project_functions(project_id,function_id,name,description,builtin,grants_json,position) \
             VALUES (?1,?2,?3,?4,0,?5,COALESCE((SELECT MAX(position)+1 FROM project_functions WHERE project_id=?1),0)) \
             ON CONFLICT(project_id,function_id) DO UPDATE SET name=excluded.name, \
                 description=excluded.description,grants_json=excluded.grants_json",
            params![project_id,function.function_id,function.name.trim(),function.description.trim(),serde_json::to_string(&function.grants)?],
        )?;
    }
    reconcile_project_mirrors(project_id);
    Ok(true)
}

pub fn delete_function(project_id: &str, function_id: &str) -> Result<bool> {
    let removed = {
        let pool = super::db::pool()?;
        let conn = pool.write().map_err(write_err)?;
        let builtin: Option<bool> = conn
            .query_row(
                "SELECT builtin FROM project_functions WHERE project_id=?1 AND function_id=?2",
                params![project_id, function_id],
                |row| row.get(0),
            )
            .optional()?;
        if builtin == Some(true) {
            bail!("builtin project functions cannot be deleted");
        }
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM project_member_functions WHERE project_id=?1 AND function_id=?2",
            params![project_id, function_id],
        )?;
        let removed = tx.execute(
            "DELETE FROM project_functions WHERE project_id=?1 AND function_id=?2",
            params![project_id, function_id],
        )?;
        tx.commit()?;
        removed > 0
    };
    if removed {
        reconcile_project_mirrors(project_id);
    }
    Ok(removed)
}

/// A policy change can revoke inherited grants anywhere below this node.
/// Reconcile after releasing the central writer so Code and ML mirrors both
/// read the committed policy and can open their own storage without inversion.
fn reconcile_project_mirrors(project_id: &str) {
    let ids = match super::db::pool().and_then(|pool| {
        let conn = pool.read().map_err(read_err)?;
        let org_id: Option<String> = conn
            .query_row(
                "SELECT org_id FROM projects WHERE project_id=?1",
                [project_id],
                |row| row.get(0),
            )
            .optional()?;
        drop(conn);
        match org_id {
            Some(org_id) => Ok(list_descendants(&org_id, project_id, true)?
                .into_iter()
                .map(|project| project.project_id)
                .collect::<Vec<_>>()),
            None => Ok(vec![project_id.to_string()]),
        }
    }) {
        Ok(ids) => ids,
        Err(error) => {
            tracing::warn!(
                project_id,
                "project mirror reconciliation failed to enumerate subtree: {error}"
            );
            return;
        }
    };
    for id in ids {
        if let Some(core_db) = crate::db::global_pool() {
            crate::code_studio::project_link::sync_project(&core_db, &id);
        }
        super::ml_link::sync_project_memberships(id);
    }
}

const MEMBER_COLS: &str =
    "m.project_id,m.user_id,m.project_admin,m.expires_at,m.invited_by,m.created_at, \
    COALESCE((SELECT json_group_array(function_id) FROM ( \
        SELECT a.function_id FROM project_member_functions a \
        JOIN project_functions f ON f.project_id=a.project_id AND f.function_id=a.function_id \
        WHERE a.project_id=m.project_id AND a.user_id=m.user_id ORDER BY f.position \
    )), '[]')";

fn read_member(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemberRecord> {
    let functions_json: String = row.get(6)?;
    let functions = serde_json::from_str(&functions_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(MemberRecord {
        project_id: row.get(0)?,
        user_id: row.get(1)?,
        project_admin: row.get(2)?,
        expires_at: row.get(3)?,
        invited_by: row.get(4)?,
        created_at: row.get(5)?,
        functions,
    })
}

pub fn member_access(project_id: &str, user_id: &str) -> Result<Option<MemberRecord>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        &format!(
            "SELECT {MEMBER_COLS} FROM project_members m WHERE m.project_id=?1 AND m.user_id=?2"
        ),
        params![project_id, user_id],
        read_member,
    )
    .optional()
    .map_err(Into::into)
}

pub fn list_members(project_id: &str) -> Result<Vec<MemberRecord>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {MEMBER_COLS} FROM project_members m JOIN projects p ON p.project_id=m.project_id \
         WHERE m.project_id=?1 ORDER BY (m.user_id=p.owner_user_id) DESC,m.created_at,m.user_id"
    ))?;
    let rows = stmt.query_map(params![project_id], read_member)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn member_count(project_id: &str) -> Result<u32> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM project_members WHERE project_id = ?1",
        params![project_id],
        |row| row.get(0),
    )?;
    Ok(n as u32)
}

/// All memberships of one user, keyed by project id — used to compose the
/// project list without one query per project. Expired access is excluded.
pub fn member_accesses_for_user(user_id: &str) -> Result<HashMap<String, MemberRecord>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {MEMBER_COLS} FROM project_members m WHERE m.user_id=?1"
    ))?;
    let rows = stmt.query_map(params![user_id], read_member)?;
    let mut out = HashMap::new();
    for row in rows {
        let member = row?;
        if member.is_active() {
            out.insert(member.project_id.clone(), member);
        }
    }
    Ok(out)
}

/// Adds members, skipping users that already belong to the project. Returns
/// the number of rows actually inserted.
pub fn add_members(project_id: &str, members: &[MemberInput], invited_by: &str) -> Result<u32> {
    let added = {
        let pool = super::db::pool()?;
        let conn = pool.write().map_err(write_err)?;
        let tx = conn.unchecked_transaction()?;
        let catalogue = list_functions_on(&tx, project_id)?;
        let mut added = 0u32;
        for member in members {
            let member =
                super::models::validate_member_input(member, &catalogue, chrono::Utc::now())?;
            added += insert_member(&tx, project_id, &member, invited_by)?;
        }
        tx.commit()?;
        added
    };
    if added > 0 {
        reconcile_project_mirrors(project_id);
    }
    Ok(added)
}

fn insert_member(
    conn: &rusqlite::Connection,
    project_id: &str,
    member: &MemberInput,
    invited_by: &str,
) -> Result<u32> {
    let n = conn.execute(
        "INSERT OR IGNORE INTO project_members(project_id,user_id,project_admin,expires_at,invited_by) \
         VALUES (?1,?2,?3,?4,?5)",
        params![project_id,member.user_id,member.project_admin,member.expires_at,invited_by],
    )?;
    if n > 0 {
        for function_id in &member.functions {
            conn.execute(
                "INSERT INTO project_member_functions(project_id,user_id,function_id) VALUES (?1,?2,?3)",
                params![project_id,member.user_id,function_id],
            )?;
        }
    }
    Ok(n as u32)
}

pub fn set_member_access(
    project_id: &str,
    user_id: &str,
    functions: &[String],
    project_admin: bool,
    expires_at: Option<&str>,
) -> Result<bool> {
    let changed = {
        let pool = super::db::pool()?;
        let conn = pool.write().map_err(write_err)?;
        let tx = conn.unchecked_transaction()?;
        let owner: String = tx.query_row(
            "SELECT owner_user_id FROM projects WHERE project_id=?1",
            params![project_id],
            |row| row.get(0),
        )?;
        if owner == user_id && expires_at.is_some() {
            bail!("project owner membership cannot expire");
        }
        let catalogue = list_functions_on(&tx, project_id)?;
        let member = super::models::validate_member_input(
            &MemberInput {
                user_id: user_id.to_string(),
                functions: functions.to_vec(),
                project_admin,
                expires_at: expires_at.map(str::to_string),
            },
            &catalogue,
            chrono::Utc::now(),
        )?;
        let n = tx.execute(
            "UPDATE project_members SET project_admin=?1,expires_at=?2 WHERE project_id=?3 AND user_id=?4",
            params![member.project_admin,member.expires_at,project_id,user_id],
        )?;
        if n > 0 {
            tx.execute(
                "DELETE FROM project_member_functions WHERE project_id=?1 AND user_id=?2",
                params![project_id, user_id],
            )?;
            for function_id in &member.functions {
                tx.execute(
                    "INSERT INTO project_member_functions(project_id,user_id,function_id) VALUES (?1,?2,?3)",
                    params![project_id,user_id,function_id],
                )?;
            }
        }
        tx.commit()?;
        n > 0
    };
    if changed {
        reconcile_project_mirrors(project_id);
    }
    Ok(changed)
}

pub fn remove_member(project_id: &str, user_id: &str) -> Result<bool> {
    let removed = {
        let pool = super::db::pool()?;
        let conn = pool.write().map_err(write_err)?;
        let owner: Option<String> = conn
            .query_row(
                "SELECT owner_user_id FROM projects WHERE project_id=?1",
                params![project_id],
                |row| row.get(0),
            )
            .optional()?;
        if owner.as_deref() == Some(user_id) {
            bail!("project owner cannot be removed");
        }
        let n = conn.execute(
            "DELETE FROM project_members WHERE project_id = ?1 AND user_id = ?2",
            params![project_id, user_id],
        )?;
        n > 0
    };
    if removed {
        reconcile_project_mirrors(project_id);
    }
    Ok(removed)
}

/// Ownership is independent from function grants; a transfer activates the
/// new owner's administrator membership without conferring confidential grants.
pub fn transfer_ownership(project_id: &str, old_owner: &str, new_owner: &str) -> Result<()> {
    {
        let pool = super::db::pool()?;
        let conn = pool.write().map_err(write_err)?;
        let tx = conn.unchecked_transaction()?;
        let (current_owner, org_id): (String, String) = tx.query_row(
            "SELECT owner_user_id,org_id FROM projects WHERE project_id=?1",
            params![project_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if current_owner != old_owner {
            bail!("project ownership changed");
        }
        let access = project_access_on(
            &tx,
            &org_id,
            project_id,
            new_owner,
            false,
            chrono::Utc::now(),
            None,
        )?;
        if !access.has_access || access.archived || access.ended {
            bail!("new owner has no current project access");
        }
        tx.execute(
            "INSERT INTO project_members(project_id,user_id,project_admin,expires_at,invited_by) \
             VALUES (?1,?2,1,NULL,?3) ON CONFLICT(project_id,user_id) DO UPDATE SET \
             project_admin=1,expires_at=NULL",
            params![project_id, new_owner, old_owner],
        )?;
        tx.execute(
            "UPDATE projects SET owner_user_id = ?1, updated_at = datetime('now') \
             WHERE project_id = ?2",
            params![new_owner, project_id],
        )?;
        tx.commit()?;
    }
    reconcile_project_mirrors(project_id);
    Ok(())
}

// =============================================================================
// Central registry: creator grants
// =============================================================================

pub fn list_creator_grants(org_id: &str) -> Result<Vec<CreatorGrantRecord>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(
        "SELECT user_id, org_id, granted_by, created_at FROM project_creator_grants \
         WHERE org_id = ?1 ORDER BY created_at",
    )?;
    let rows = stmt.query_map(params![org_id], |row| {
        Ok(CreatorGrantRecord {
            user_id: row.get(0)?,
            org_id: row.get(1)?,
            granted_by: row.get(2)?,
            created_at: row.get(3)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn set_creator_grant(
    user_id: &str,
    org_id: &str,
    granted_by: &str,
    granted: bool,
) -> Result<bool> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let n = if granted {
        conn.execute(
            "INSERT OR REPLACE INTO project_creator_grants (user_id, org_id, granted_by) \
             VALUES (?1, ?2, ?3)",
            params![user_id, org_id, granted_by],
        )?
    } else {
        conn.execute(
            "DELETE FROM project_creator_grants WHERE user_id = ?1 AND org_id = ?2",
            params![user_id, org_id],
        )?
    };
    Ok(n > 0)
}

pub fn has_creator_grant(user_id: &str, org_id: &str) -> Result<bool> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM project_creator_grants WHERE user_id = ?1 AND org_id = ?2",
        params![user_id, org_id],
        |row| row.get(0),
    )?;
    Ok(n > 0)
}

// =============================================================================
// Central registry: chats (private per user — every query filters by user_id)
// =============================================================================

fn read_chat(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChatRecord> {
    Ok(ChatRecord {
        chat_id: row.get(0)?,
        project_id: row.get(1)?,
        user_id: row.get(2)?,
        title: row.get(3)?,
        session_id: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

const CHAT_COLS: &str = "chat_id, project_id, user_id, title, session_id, created_at, updated_at";

pub fn list_chats(project_id: &str, user_id: &str) -> Result<Vec<ChatRecord>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {CHAT_COLS} FROM project_chats \
         WHERE project_id = ?1 AND user_id = ?2 ORDER BY updated_at DESC"
    ))?;
    let rows = stmt.query_map(params![project_id, user_id], read_chat)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn get_chat(project_id: &str, chat_id: &str, user_id: &str) -> Result<Option<ChatRecord>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        &format!(
            "SELECT {CHAT_COLS} FROM project_chats \
             WHERE project_id = ?1 AND chat_id = ?2 AND user_id = ?3"
        ),
        params![project_id, chat_id, user_id],
        read_chat,
    )
    .optional()
    .map_err(Into::into)
}

pub fn create_chat(project_id: &str, user_id: &str, title: &str) -> Result<ChatRecord> {
    let chat_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "INSERT INTO project_chats (chat_id, project_id, user_id, title, session_id) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![chat_id, project_id, user_id, title, session_id],
    )?;
    conn.query_row(
        &format!("SELECT {CHAT_COLS} FROM project_chats WHERE chat_id = ?1"),
        params![chat_id],
        read_chat,
    )
    .map_err(Into::into)
}

pub fn rename_chat(project_id: &str, chat_id: &str, user_id: &str, title: &str) -> Result<bool> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "UPDATE project_chats SET title = ?1, updated_at = datetime('now') \
         WHERE project_id = ?2 AND chat_id = ?3 AND user_id = ?4",
        params![title, project_id, chat_id, user_id],
    )?;
    Ok(n > 0)
}

/// Bumps `updated_at` so the chat surfaces at the top of the (updated_at DESC)
/// conversation list after a new turn. Owner-scoped like every chat query.
pub fn touch_chat(project_id: &str, chat_id: &str, user_id: &str) -> Result<bool> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "UPDATE project_chats SET updated_at = datetime('now') \
         WHERE project_id = ?1 AND chat_id = ?2 AND user_id = ?3",
        params![project_id, chat_id, user_id],
    )?;
    Ok(n > 0)
}

pub fn delete_chat(project_id: &str, chat_id: &str, user_id: &str) -> Result<bool> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "DELETE FROM project_chats WHERE project_id = ?1 AND chat_id = ?2 AND user_id = ?3",
        params![project_id, chat_id, user_id],
    )?;
    Ok(n > 0)
}

pub fn count_chats(project_id: &str, user_id: &str) -> Result<u32> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM project_chats WHERE project_id = ?1 AND user_id = ?2",
        params![project_id, user_id],
        |row| row.get(0),
    )?;
    Ok(n as u32)
}

// =============================================================================
// Core-directory lookups (tentaflow.db — separate pool, no cross-file JOINs)
// =============================================================================

/// Resolves display name + email for a set of user ids from the CORE user
/// directory. Missing ids / unavailable core DB are skipped (frontend falls
/// back to the raw UUID). Never panics.
pub fn resolve_user_refs(user_ids: &[String]) -> HashMap<String, (String, String)> {
    let mut out = HashMap::new();
    let Some(core) = crate::db::global_pool() else {
        return out;
    };
    let Ok(conn) = core.read() else {
        return out;
    };
    for id in user_ids {
        let row: Option<(String, String)> = conn
            .query_row(
                "SELECT COALESCE(NULLIF(display_name, ''), NULLIF(username, ''), id), \
                        COALESCE(email, '') \
                 FROM user_accounts WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .ok()
            .flatten();
        if let Some(pair) = row {
            out.insert(id.clone(), pair);
        }
    }
    out
}

/// Active users of the org matching `query` (against username/display
/// name/email), for the member pickers. Existing-member exclusion happens in
/// the dispatcher (memberships live in a different SQLite file).
pub fn list_org_user_candidates(
    org_id: &str,
    query: &str,
    limit: u32,
) -> Result<Vec<(String, String, String)>> {
    let core = crate::db::global_pool().ok_or_else(|| anyhow!("core directory unavailable"))?;
    let conn = core.read().map_err(read_err)?;
    let like = format!("%{}%", escape_like(query.trim()));
    let mut stmt = conn.prepare(
        "SELECT u.id, COALESCE(NULLIF(u.display_name, ''), NULLIF(u.username, ''), u.id), \
                COALESCE(u.email, '') \
         FROM user_accounts u \
         JOIN org_memberships m ON m.user_id = u.id \
         WHERE m.org_id = ?1 AND u.is_active = 1 \
           AND (u.username LIKE ?2 ESCAPE '\\' OR u.display_name LIKE ?2 ESCAPE '\\' \
                OR u.email LIKE ?2 ESCAPE '\\') \
         ORDER BY u.display_name, u.username LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![org_id, like, limit as i64], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn is_org_member(org_id: &str, user_id: &str) -> Result<bool> {
    let core = crate::db::global_pool().ok_or_else(|| anyhow!("core directory unavailable"))?;
    let conn = core.read().map_err(read_err)?;
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM org_memberships WHERE org_id = ?1 AND user_id = ?2",
        params![org_id, user_id],
        |row| row.get(0),
    )?;
    Ok(n > 0)
}

/// Resolves an agent's display name + model label from the core `agents`
/// table for the settings screen. Missing agent → `None` (the binding then
/// falls back to the platform default).
pub fn resolve_agent_label(agent_id: &str) -> Option<(String, String)> {
    let core = crate::db::global_pool()?;
    let conn = core.read().ok()?;
    conn.query_row(
        "SELECT COALESCE(NULLIF(display_name, ''), name), COALESCE(model, '') \
         FROM agents WHERE id = ?1",
        params![agent_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )
    .optional()
    .ok()
    .flatten()
}

// =============================================================================
// Per-project: sources
// =============================================================================

fn read_source(row: &rusqlite::Row<'_>) -> rusqlite::Result<SourceRecord> {
    Ok(SourceRecord {
        source_id: row.get(0)?,
        kind: row.get(1)?,
        name: row.get(2)?,
        status: row.get(3)?,
        config_json: row.get(4)?,
        error: row.get(5)?,
        created_by: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

const SOURCE_COLS: &str =
    "source_id, kind, name, status, config_json, error, created_by, created_at, updated_at";

pub fn create_source(
    pool: &DbPool,
    source_id: &str,
    kind: &str,
    name: &str,
    config_json: &str,
    created_by: &str,
) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "INSERT INTO sources (source_id, kind, name, config_json, created_by) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![source_id, kind, name, config_json, created_by],
    )?;
    Ok(())
}

pub fn get_source(pool: &DbPool, source_id: &str) -> Result<Option<SourceRecord>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        &format!("SELECT {SOURCE_COLS} FROM sources WHERE source_id = ?1"),
        params![source_id],
        read_source,
    )
    .optional()
    .map_err(Into::into)
}

pub fn list_sources(pool: &DbPool) -> Result<Vec<SourceListItem>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {SOURCE_COLS} FROM sources ORDER BY created_at DESC"
    ))?;
    let sources = stmt
        .query_map([], read_source)?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let mut out = Vec::with_capacity(sources.len());
    for record in sources {
        let (file_count, chunk_count): (i64, i64) = conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(chunk_count), 0) FROM source_files \
             WHERE source_id = ?1",
            params![record.source_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let last_job = conn
            .query_row(
                &format!(
                    "SELECT {JOB_COLS} FROM ingest_jobs WHERE source_id = ?1 \
                     ORDER BY started_at DESC, job_id DESC LIMIT 1"
                ),
                params![record.source_id],
                read_job,
            )
            .optional()?;
        out.push(SourceListItem {
            record,
            file_count: file_count as u32,
            chunk_count: chunk_count as u32,
            last_job,
        });
    }
    Ok(out)
}

pub fn update_source_meta(
    pool: &DbPool,
    source_id: &str,
    name: &str,
    config_json: &str,
) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "UPDATE sources SET name = ?1, config_json = ?2, updated_at = datetime('now') \
         WHERE source_id = ?3",
        params![name, config_json, source_id],
    )?;
    Ok(n > 0)
}

pub fn set_source_status(pool: &DbPool, source_id: &str, status: &str, error: &str) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "UPDATE sources SET status = ?1, error = ?2, updated_at = datetime('now') \
         WHERE source_id = ?3",
        params![status, error, source_id],
    )?;
    Ok(())
}

/// Removes the source row together with its files and jobs. Vector/blob
/// cleanup happens in the dispatcher BEFORE this call.
pub fn delete_source_rows(pool: &DbPool, source_id: &str) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM source_files WHERE source_id = ?1",
        params![source_id],
    )?;
    tx.execute(
        "DELETE FROM ingest_jobs WHERE source_id = ?1",
        params![source_id],
    )?;
    let n = tx.execute(
        "DELETE FROM sources WHERE source_id = ?1",
        params![source_id],
    )?;
    tx.commit()?;
    Ok(n > 0)
}

// =============================================================================
// Per-project: source files
// =============================================================================

fn read_file(row: &rusqlite::Row<'_>) -> rusqlite::Result<SourceFileRecord> {
    Ok(SourceFileRecord {
        file_id: row.get(0)?,
        source_id: row.get(1)?,
        path: row.get(2)?,
        sha256: row.get(3)?,
        size_bytes: row.get::<_, i64>(4)? as u64,
        mime: row.get(5)?,
        status: row.get(6)?,
        error: row.get(7)?,
        chunk_count: row.get::<_, i64>(8)? as u32,
        updated_at: row.get(9)?,
    })
}

const FILE_COLS: &str =
    "file_id, source_id, path, sha256, size_bytes, mime, status, error, chunk_count, updated_at";

/// Inserts (or refreshes, on `UNIQUE(source_id, path)` conflict) a file row
/// and resets it to `pending`. Returns the effective `file_id` — a re-upload
/// of the same path keeps the original id so its vectors are replaced, not
/// duplicated.
pub fn upsert_source_file(
    pool: &DbPool,
    source_id: &str,
    path: &str,
    sha256: &str,
    size_bytes: u64,
    mime: &str,
) -> Result<String> {
    let conn = pool.write().map_err(write_err)?;
    let existing: Option<String> = conn
        .query_row(
            "SELECT file_id FROM source_files WHERE source_id = ?1 AND path = ?2",
            params![source_id, path],
            |row| row.get(0),
        )
        .optional()?;
    match existing {
        Some(file_id) => {
            conn.execute(
                "UPDATE source_files SET sha256 = ?1, size_bytes = ?2, mime = ?3, \
                 status = 'pending', error = '', updated_at = datetime('now') \
                 WHERE file_id = ?4",
                params![sha256, size_bytes as i64, mime, file_id],
            )?;
            Ok(file_id)
        }
        None => {
            let file_id = uuid::Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO source_files (file_id, source_id, path, sha256, size_bytes, mime) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![file_id, source_id, path, sha256, size_bytes as i64, mime],
            )?;
            Ok(file_id)
        }
    }
}

pub fn get_source_file(pool: &DbPool, file_id: &str) -> Result<Option<SourceFileRecord>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        &format!("SELECT {FILE_COLS} FROM source_files WHERE file_id = ?1"),
        params![file_id],
        read_file,
    )
    .optional()
    .map_err(Into::into)
}

pub fn list_source_files(
    pool: &DbPool,
    source_id: &str,
    offset: u32,
    limit: u32,
    filter: &str,
) -> Result<(Vec<SourceFileRecord>, u32)> {
    let conn = pool.read().map_err(read_err)?;
    let like = format!("%{}%", escape_like(filter.trim()));
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM source_files WHERE source_id = ?1 AND path LIKE ?2 ESCAPE '\\'",
        params![source_id, like],
        |row| row.get(0),
    )?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {FILE_COLS} FROM source_files WHERE source_id = ?1 AND path LIKE ?2 ESCAPE '\\' \
         ORDER BY path LIMIT ?3 OFFSET ?4"
    ))?;
    let rows = stmt.query_map(
        params![source_id, like, limit as i64, offset as i64],
        read_file,
    )?;
    let files = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok((files, total as u32))
}

/// Files to process in an ingest job: the whole source or a single file.
pub fn files_for_ingest(
    pool: &DbPool,
    source_id: &str,
    only_file: Option<&str>,
) -> Result<Vec<SourceFileRecord>> {
    let conn = pool.read().map_err(read_err)?;
    match only_file {
        Some(file_id) => {
            let mut stmt = conn.prepare(&format!(
                "SELECT {FILE_COLS} FROM source_files WHERE source_id = ?1 AND file_id = ?2"
            ))?;
            let rows = stmt.query_map(params![source_id, file_id], read_file)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into)
        }
        None => {
            let mut stmt = conn.prepare(&format!(
                "SELECT {FILE_COLS} FROM source_files WHERE source_id = ?1 ORDER BY path"
            ))?;
            let rows = stmt.query_map(params![source_id], read_file)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into)
        }
    }
}

pub fn delete_source_file_row(pool: &DbPool, file_id: &str) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "DELETE FROM source_files WHERE file_id = ?1",
        params![file_id],
    )?;
    Ok(n > 0)
}

/// How many rows still reference the blob `files/<sha256>`: source_files rows
/// PLUS attachment entries embedded in `attachments_json` of test cases, run
/// items, run steps and tasks (json_each over the arrays). The blob may only
/// be removed when this drops to zero — counting source_files alone would let
/// the GC and the source-delete paths eat tester screenshots (risk F.6).
pub fn blob_ref_count(pool: &DbPool, sha256: &str) -> Result<u32> {
    let conn = pool.read().map_err(read_err)?;
    let n: i64 = conn.query_row(
        "SELECT (SELECT COUNT(*) FROM source_files WHERE sha256 = ?1) \
              + (SELECT COUNT(*) FROM test_cases t, json_each(t.attachments_json) j \
                 WHERE json_extract(j.value, '$.sha256') = ?1) \
              + (SELECT COUNT(*) FROM test_run_items t, json_each(t.attachments_json) j \
                 WHERE json_extract(j.value, '$.sha256') = ?1) \
              + (SELECT COUNT(*) FROM test_run_steps t, json_each(t.attachments_json) j \
                 WHERE json_extract(j.value, '$.sha256') = ?1) \
              + (SELECT COUNT(*) FROM tasks t, json_each(t.attachments_json) j \
                 WHERE json_extract(j.value, '$.sha256') = ?1) \
              + (SELECT COUNT(*) FROM task_events e, \
                 json_each(CASE WHEN e.kind = 'attachments_json' THEN json_extract(e.before_json,'$') \
                 WHEN e.kind = 'created' THEN json_extract(e.after_json,'$.attachments_json') ELSE '[]' END) j \
                 WHERE json_extract(j.value,'$.sha256') = ?1) \
              + (SELECT COUNT(*) FROM task_events e, \
                 json_each(CASE WHEN e.kind = 'attachments_json' THEN json_extract(e.after_json,'$') ELSE '[]' END) j \
                 WHERE json_extract(j.value,'$.sha256') = ?1)",
        params![sha256],
        |row| row.get(0),
    )?;
    Ok(n as u32)
}

/// Every sha256 referenced anywhere (source_files + the four attachments_json
/// tables), for the files-dir GC — one pass over the tables instead of a
/// per-blob `blob_ref_count` query for each on-disk file.
pub fn referenced_blob_sha256s(pool: &DbPool) -> Result<std::collections::HashSet<String>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(
        "SELECT sha256 FROM source_files \
         UNION \
         SELECT json_extract(j.value, '$.sha256') \
           FROM test_cases t, json_each(t.attachments_json) j \
         UNION \
         SELECT json_extract(j.value, '$.sha256') \
           FROM test_run_items t, json_each(t.attachments_json) j \
         UNION \
         SELECT json_extract(j.value, '$.sha256') \
           FROM test_run_steps t, json_each(t.attachments_json) j \
         UNION \
         SELECT json_extract(j.value, '$.sha256') \
           FROM tasks t, json_each(t.attachments_json) j \
         UNION \
         SELECT json_extract(j.value, '$.sha256') \
           FROM task_events e, json_each(CASE WHEN e.kind = 'attachments_json' \
               THEN json_extract(e.before_json,'$') WHEN e.kind = 'created' \
               THEN json_extract(e.after_json,'$.attachments_json') ELSE '[]' END) j \
         UNION \
         SELECT json_extract(j.value, '$.sha256') \
           FROM task_events e, json_each(CASE WHEN e.kind = 'attachments_json' \
               THEN json_extract(e.after_json,'$') ELSE '[]' END) j",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, Option<String>>(0))?;
    let mut set = std::collections::HashSet::new();
    for row in rows {
        if let Some(sha) = row? {
            set.insert(sha);
        }
    }
    Ok(set)
}

/// Resolves the MIME type recorded for an attachment blob: the first matching
/// attachment entry across the four attachment-bearing tables, falling back to
/// source_files (an attachment may share content with an uploaded source).
pub fn attachment_mime(pool: &DbPool, sha256: &str) -> Result<Option<String>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        "SELECT mime FROM ( \
            SELECT json_extract(j.value, '$.mime') AS mime \
              FROM test_cases t, json_each(t.attachments_json) j \
             WHERE json_extract(j.value, '$.sha256') = ?1 \
            UNION ALL \
            SELECT json_extract(j.value, '$.mime') \
              FROM test_run_items t, json_each(t.attachments_json) j \
             WHERE json_extract(j.value, '$.sha256') = ?1 \
            UNION ALL \
            SELECT json_extract(j.value, '$.mime') \
              FROM test_run_steps t, json_each(t.attachments_json) j \
             WHERE json_extract(j.value, '$.sha256') = ?1 \
            UNION ALL \
            SELECT json_extract(j.value, '$.mime') \
              FROM tasks t, json_each(t.attachments_json) j \
             WHERE json_extract(j.value, '$.sha256') = ?1 \
            UNION ALL \
            SELECT mime FROM source_files WHERE sha256 = ?1 \
         ) WHERE mime IS NOT NULL AND mime <> '' LIMIT 1",
        params![sha256],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(Into::into)
}

// =============================================================================
// Per-project: ingest jobs
// =============================================================================

fn read_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<IngestJobRecord> {
    Ok(IngestJobRecord {
        job_id: row.get(0)?,
        source_id: row.get(1)?,
        status: row.get(2)?,
        files_total: row.get::<_, i64>(3)? as u32,
        files_done: row.get::<_, i64>(4)? as u32,
        chunks_done: row.get::<_, i64>(5)? as u32,
        error: row.get(6)?,
        started_by: row.get(7)?,
        started_at: row.get(8)?,
        finished_at: row.get(9)?,
    })
}

const JOB_COLS: &str = "job_id, source_id, status, files_total, files_done, chunks_done, \
     error, started_by, started_at, finished_at";

pub fn create_ingest_job(
    pool: &DbPool,
    job_id: &str,
    source_id: &str,
    files_total: u32,
    started_by: &str,
) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "INSERT INTO ingest_jobs (job_id, source_id, files_total, started_by) \
         VALUES (?1, ?2, ?3, ?4)",
        params![job_id, source_id, files_total as i64, started_by],
    )?;
    Ok(())
}

pub fn get_ingest_job(pool: &DbPool, job_id: &str) -> Result<Option<IngestJobRecord>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        &format!("SELECT {JOB_COLS} FROM ingest_jobs WHERE job_id = ?1"),
        params![job_id],
        read_job,
    )
    .optional()
    .map_err(Into::into)
}

/// Every job of this project the row still calls `running`, as
/// `(job_id, source_id)`. The source comes along because every caller needs it:
/// closing a job that will never resume also has to close the source it was
/// indexing, and the delete paths cancel by source. Reading it here costs
/// nothing and saves a `get_ingest_job` round trip per job.
pub fn running_jobs(pool: &DbPool) -> Result<Vec<(String, String)>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt =
        conn.prepare("SELECT job_id, source_id FROM ingest_jobs WHERE status = 'running'")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// One batch of per-file progress: the file row and the job counters commit
/// in a SINGLE transaction so a crash never shows a done file without the
/// matching job progress (or vice versa).
pub fn record_file_progress(
    pool: &DbPool,
    job_id: &str,
    file_id: &str,
    file_status: &str,
    file_error: &str,
    chunk_count: u32,
    chunks_done_inc: u32,
) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE source_files SET status = ?1, error = ?2, chunk_count = ?3, \
         updated_at = datetime('now') WHERE file_id = ?4",
        params![file_status, file_error, chunk_count as i64, file_id],
    )?;
    tx.execute(
        "UPDATE ingest_jobs SET files_done = files_done + 1, chunks_done = chunks_done + ?1 \
         WHERE job_id = ?2",
        params![chunks_done_inc as i64, job_id],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn mark_file_indexing(pool: &DbPool, file_id: &str) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "UPDATE source_files SET status = 'indexing', updated_at = datetime('now') \
         WHERE file_id = ?1",
        params![file_id],
    )?;
    Ok(())
}

/// Writes the terminal state of a job that is still `running`, and returns
/// whether it actually wrote — `false` means the row was already terminal and
/// keeps the status it had.
///
/// The guard is in the STATEMENT, not at the call sites, because the window it
/// closes belongs to no single caller: a worker writes this row and only then
/// deletes its queue row, so a crash in between leaves a queue row whose job
/// already succeeded. Startup reconciliation then arrives with "interrupted by
/// restart" and would bury a correct success. One conditional UPDATE makes
/// "the first terminal status wins" a property of the table — no caller can
/// forget it, and no caller needs a read-then-write that a second writer could
/// interleave.
pub fn finish_ingest_job(pool: &DbPool, job_id: &str, status: &str, error: &str) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let written = conn.execute(
        "UPDATE ingest_jobs SET status = ?1, error = ?2, finished_at = datetime('now') \
         WHERE job_id = ?3 AND status = 'running'",
        params![status, error, job_id],
    )?;
    Ok(written > 0)
}

// =============================================================================
// Per-project: activity, settings, tags, KPIs
// =============================================================================

pub fn insert_activity(
    pool: &DbPool,
    actor_user_id: &str,
    actor_kind: &str,
    action: &str,
    object_type: &str,
    object_id: &str,
    details_json: &str,
) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "INSERT INTO activity_log (actor_user_id, actor_kind, action, object_type, \
         object_id, details_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            actor_user_id,
            actor_kind,
            action,
            object_type,
            object_id,
            details_json
        ],
    )?;
    Ok(())
}

/// Keyset pagination newest-first: `before_id = None` starts at the top,
/// otherwise only entries with `id < before_id` are returned.
pub fn list_activity(
    pool: &DbPool,
    before_id: Option<i64>,
    limit: u32,
) -> Result<(Vec<ActivityRecord>, bool)> {
    let conn = pool.read().map_err(read_err)?;
    let fetch = (limit as i64) + 1;
    let mut stmt = conn.prepare(
        "SELECT id, actor_user_id, actor_kind, action, object_type, object_id, \
         details_json, created_at FROM activity_log \
         WHERE (?1 IS NULL OR id < ?1) ORDER BY id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![before_id, fetch], |row| {
        Ok(ActivityRecord {
            id: row.get(0)?,
            actor_user_id: row.get(1)?,
            actor_kind: row.get(2)?,
            action: row.get(3)?,
            object_type: row.get(4)?,
            object_id: row.get(5)?,
            details_json: row.get(6)?,
            created_at: row.get(7)?,
        })
    })?;
    let mut entries = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    let has_more = entries.len() as i64 > limit as i64;
    entries.truncate(limit as usize);
    Ok((entries, has_more))
}

pub fn get_setting(pool: &DbPool, key: &str) -> Result<Option<String>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        params![key],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(Into::into)
}

pub fn set_setting(pool: &DbPool, key: &str, value: &str) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn list_tags(pool: &DbPool) -> Result<Vec<TagRecord>> {
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare("SELECT tag_id, name FROM tags ORDER BY name COLLATE NOCASE")?;
    let rows = stmt.query_map([], |row| {
        Ok(TagRecord {
            tag_id: row.get(0)?,
            name: row.get(1)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Creates or renames a tag. A `UNIQUE COLLATE NOCASE` clash surfaces as an
/// `Err` with "UNIQUE" in the message (dispatcher maps it to BadRequest).
pub fn upsert_tag(
    pool: &DbPool,
    tag_id: Option<&str>,
    name: &str,
    created_by: &str,
) -> Result<String> {
    let conn = pool.write().map_err(write_err)?;
    match tag_id {
        Some(id) => {
            let n = conn.execute(
                "UPDATE tags SET name = ?1 WHERE tag_id = ?2",
                params![name, id],
            )?;
            if n == 0 {
                bail!("tag not found");
            }
            Ok(id.to_string())
        }
        None => {
            let id = uuid::Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO tags (tag_id, name, created_by) VALUES (?1, ?2, ?3)",
                params![id, name, created_by],
            )?;
            Ok(id)
        }
    }
}

pub fn delete_tag(pool: &DbPool, tag_id: &str) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute("DELETE FROM tags WHERE tag_id = ?1", params![tag_id])?;
    Ok(n > 0)
}

pub fn project_kpis(pool: &DbPool) -> Result<ProjectKpis> {
    let conn = pool.read().map_err(read_err)?;
    let (sources_total, sources_ready): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(status = 'ready'), 0) FROM sources",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let (files_total, chunks_total): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(chunk_count), 0) FROM source_files",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let open_jobs: i64 = conn.query_row(
        "SELECT COUNT(*) FROM ingest_jobs WHERE status = 'running'",
        [],
        |row| row.get(0),
    )?;
    Ok(ProjectKpis {
        sources_total: sources_total as u32,
        sources_ready: sources_ready as u32,
        files_total: files_total as u32,
        chunks_total: chunks_total as u32,
        open_ingest_jobs: open_jobs as u32,
    })
}

/// F2 KPI counters for the overview screen. Pending agent output is excluded
/// from case counters (same visibility rule as every case query);
/// `my_run_items_pending` counts items of RUNNING runs assigned to the caller
/// or claimable from the pool.
pub fn project_f2_kpis(pool: &DbPool, user_id: &str) -> Result<super::models::ProjectF2Kpis> {
    let conn = pool.read().map_err(read_err)?;
    let (cases_total, cases_approved): (i64, i64) = conn.query_row(
        &format!(
            "SELECT COUNT(*), COALESCE(SUM(status = 'approved'), 0) FROM test_cases WHERE {}",
            super::tests::VISIBLE_CASES_PREDICATE
        ),
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let suites_total: i64 =
        conn.query_row("SELECT COUNT(*) FROM test_suites", [], |row| row.get(0))?;
    let runs_open: i64 = conn.query_row(
        "SELECT COUNT(*) FROM test_runs WHERE status = 'running'",
        [],
        |row| row.get(0),
    )?;
    let my_pending: i64 = conn.query_row(
        "SELECT COUNT(*) FROM test_run_items i \
         JOIN test_runs r ON r.run_id = i.run_id AND r.status = 'running' \
         WHERE i.status = 'pending' AND (i.assigned_to = ?1 OR i.assigned_to = '')",
        params![user_id],
        |row| row.get(0),
    )?;
    let (tasks_open, defects_open): (i64, i64) = conn.query_row(
        "SELECT COALESCE(SUM(task_type <> 'defect'), 0), COALESCE(SUM(task_type = 'defect'), 0) \
         FROM tasks WHERE status <> 'done' AND archived_at IS NULL",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let generations_running: i64 = conn.query_row(
        "SELECT COUNT(*) FROM generation_runs WHERE status = 'running'",
        [],
        |row| row.get(0),
    )?;
    Ok(super::models::ProjectF2Kpis {
        cases_total: cases_total as u32,
        cases_approved: cases_approved as u32,
        suites_total: suites_total as u32,
        runs_open: runs_open as u32,
        my_run_items_pending: my_pending as u32,
        tasks_open: tasks_open as u32,
        defects_open: defects_open as u32,
        generations_running: generations_running as u32,
    })
}

/// F3 KPI counters (environments + open automated runs) for the overview.
pub fn project_f3_kpis(pool: &DbPool) -> Result<super::models::ProjectF3Kpis> {
    let conn = pool.read().map_err(read_err)?;
    let (approved, pending): (i64, i64) = conn.query_row(
        "SELECT COALESCE(SUM(approval_status = 'approved'), 0), \
                COALESCE(SUM(approval_status = 'pending'), 0) FROM environments",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let auto_runs_open: i64 = conn.query_row(
        "SELECT COUNT(*) FROM test_runs WHERE status = 'running' \
         AND run_id IN (SELECT run_id FROM auto_run_meta)",
        [],
        |row| row.get(0),
    )?;
    Ok(super::models::ProjectF3Kpis {
        environments_approved: approved as u32,
        environments_pending: pending as u32,
        auto_runs_open: auto_runs_open as u32,
    })
}

/// Stores the encrypted access token of a git source (input-only on the wire).
pub fn set_source_secret(pool: &DbPool, source_id: &str, secret_enc: &str) -> Result<bool> {
    let conn = pool.write().map_err(write_err)?;
    let n = conn.execute(
        "UPDATE sources SET secret_enc = ?1, updated_at = datetime('now') WHERE source_id = ?2",
        params![secret_enc, source_id],
    )?;
    Ok(n > 0)
}

/// Encrypted access token of a source; `""` when none is stored.
pub fn get_source_secret_enc(pool: &DbPool, source_id: &str) -> Result<String> {
    let conn = pool.read().map_err(read_err)?;
    Ok(conn
        .query_row(
            "SELECT secret_enc FROM sources WHERE source_id = ?1",
            params![source_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .unwrap_or_default())
}

/// Sets the language of a case. Separate statement on purpose: F2's
/// `CaseContentInput` predates code cases and carries no language field, and
/// widening it would churn every manual-test call site.
pub fn set_case_language(pool: &DbPool, case_id: &str, language: &str) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "UPDATE test_cases SET language = ?1 WHERE case_id = ?2",
        params![language, case_id],
    )?;
    Ok(())
}

/// Replaces the file rows of a code source with the freshly collected tree and
/// returns the ids of the files that need (re-)embedding plus the ids of the
/// rows that were dropped (their vectors are deleted by the caller). ONE
/// transaction, so a crash never leaves the file list half-rewritten.
pub fn sync_tree_files(
    pool: &DbPool,
    source_id: &str,
    files: &[super::ingest::CollectedFile],
    delta: &super::ingest::TreeDelta,
) -> Result<(Vec<String>, Vec<String>)> {
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let mut removed_ids = Vec::with_capacity(delta.removed.len());
    for path in &delta.removed {
        let file_id: Option<String> = tx
            .query_row(
                "SELECT file_id FROM source_files WHERE source_id = ?1 AND path = ?2",
                params![source_id, path],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(file_id) = file_id {
            tx.execute(
                "DELETE FROM source_files WHERE file_id = ?1",
                params![file_id],
            )?;
            removed_ids.push(file_id);
        }
    }
    let touched: std::collections::HashSet<&str> = delta
        .added
        .iter()
        .chain(delta.changed.iter())
        .map(|p| p.as_str())
        .collect();
    let mut work_ids = Vec::with_capacity(touched.len());
    for file in files {
        if !touched.contains(file.rel_path.as_str()) {
            continue;
        }
        let existing: Option<String> = tx
            .query_row(
                "SELECT file_id FROM source_files WHERE source_id = ?1 AND path = ?2",
                params![source_id, file.rel_path],
                |row| row.get(0),
            )
            .optional()?;
        let file_id = match existing {
            Some(file_id) => {
                tx.execute(
                    "UPDATE source_files SET sha256 = ?1, size_bytes = ?2, mime = ?3, \
                     status = 'pending', error = '', updated_at = datetime('now') \
                     WHERE file_id = ?4",
                    params![file.sha256, file.size_bytes as i64, file.mime, file_id],
                )?;
                file_id
            }
            None => {
                let file_id = uuid::Uuid::new_v4().to_string();
                tx.execute(
                    "INSERT INTO source_files (file_id, source_id, path, sha256, size_bytes, mime) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        file_id,
                        source_id,
                        file.rel_path,
                        file.sha256,
                        file.size_bytes as i64,
                        file.mime
                    ],
                )?;
                file_id
            }
        };
        work_ids.push(file_id);
    }
    tx.commit()?;
    Ok((work_ids, removed_ids))
}

/// Read-only source/file counters for the project LIST screen. Opens the
/// project.db file directly (read-only, no pool) so listing many projects
/// does not churn the LRU pool cache; a missing/unreadable file counts as
/// zero (freshly created or already deleted project).
pub fn read_source_counts(dir_path: &str) -> (u32, u32) {
    let db_path = std::path::Path::new(dir_path).join("project.db");
    let Ok(conn) =
        rusqlite::Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return (0, 0);
    };
    conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(status = 'ready'), 0) FROM sources",
        [],
        |row| Ok((row.get::<_, i64>(0)? as u32, row.get::<_, i64>(1)? as u32)),
    )
    .unwrap_or((0, 0))
}

// =============================================================================
// Access evaluation shared by all request and background consumers
// =============================================================================

pub fn project_access(
    project: &ProjectRecord,
    user_id: &str,
    app_admin: bool,
) -> Result<ProjectAccessWire> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let snapshot = conn.unchecked_transaction()?;
    let access = project_access_on(
        &snapshot,
        &project.org_id,
        &project.project_id,
        user_id,
        app_admin,
        chrono::Utc::now(),
        None,
    )?;
    snapshot.commit()?;
    Ok(access)
}

pub fn project_access_after_member_removal(
    project: &ProjectRecord,
    user_id: &str,
    removed_project_id: &str,
) -> Result<ProjectAccessWire> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let snapshot = conn.unchecked_transaction()?;
    let access = project_access_on(
        &snapshot,
        &project.org_id,
        &project.project_id,
        user_id,
        false,
        chrono::Utc::now(),
        Some(removed_project_id),
    )?;
    snapshot.commit()?;
    Ok(access)
}

fn project_ancestry_on(
    conn: &rusqlite::Connection,
    org_id: &str,
    project_id: &str,
) -> Result<Vec<ProjectRecord>> {
    let target = conn
        .query_row(
            &format!("SELECT {PROJECT_COLS} FROM projects WHERE org_id=?1 AND project_id=?2"),
            params![org_id, project_id],
            read_project,
        )
        .optional()?
        .ok_or_else(|| anyhow!("project not found"))?;
    let ids: Vec<&str> = target.path.split('/').filter(|id| !id.is_empty()).collect();
    if ids.is_empty() || ids.len() > 4 || ids.last() != Some(&project_id) {
        bail!("invalid project path");
    }
    let mut ancestors = Vec::with_capacity(ids.len());
    for (index, id) in ids.iter().enumerate() {
        let row = if *id == project_id {
            target.clone()
        } else {
            conn.query_row(
                &format!("SELECT {PROJECT_COLS} FROM projects WHERE org_id=?1 AND project_id=?2"),
                params![org_id, id],
                read_project,
            )
            .optional()?
            .ok_or_else(|| anyhow!("project ancestry is incomplete"))?
        };
        if row.depth as usize != index + 1
            || row.parent_id.as_deref()
                != ancestors
                    .last()
                    .map(|parent: &ProjectRecord| parent.project_id.as_str())
            || row.path != format!("/{}", ids[..=index].join("/"))
        {
            bail!("project ancestry is inconsistent");
        }
        ancestors.push(row);
    }
    Ok(ancestors)
}

pub fn project_ancestry(org_id: &str, project_id: &str) -> Result<Vec<ProjectRecord>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let snapshot = conn.unchecked_transaction()?;
    let ancestors = project_ancestry_on(&snapshot, org_id, project_id)?;
    snapshot.commit()?;
    Ok(ancestors)
}

fn effective_modules_on(ancestors: &[ProjectRecord]) -> Result<Vec<String>> {
    let mut modules = Vec::new();
    for (index, project) in ancestors.iter().enumerate() {
        if index == 0 || !project.inherit_modules {
            modules = serde_json::from_str(&project.modules_json)?;
        } else {
            let disabled: HashSet<String> = serde_json::from_str(&project.module_disabled_json)?;
            modules.retain(|module| !disabled.contains(module));
        }
    }
    Ok(modules)
}

pub fn effective_project_settings(project_id: &str) -> Result<(Vec<String>, String)> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let snapshot = conn.unchecked_transaction()?;
    let org_id: String = snapshot.query_row(
        "SELECT org_id FROM projects WHERE project_id=?1",
        [project_id],
        |row| row.get(0),
    )?;
    let ancestors = project_ancestry_on(&snapshot, &org_id, project_id)?;
    let modules = effective_modules_on(&ancestors)?;
    let source = ancestors
        .iter()
        .rev()
        .find(|project| !project.inherit_task_types || project.parent_id.is_none())
        .ok_or_else(|| anyhow!("task type source not found"))?;
    let source_id = source.project_id.clone();
    snapshot.commit()?;
    Ok((modules, source_id))
}

fn project_access_on(
    conn: &rusqlite::Connection,
    org_id: &str,
    project_id: &str,
    user_id: &str,
    app_admin: bool,
    now: chrono::DateTime<chrono::Utc>,
    removed_project_id: Option<&str>,
) -> Result<ProjectAccessWire> {
    let ancestors = project_ancestry_on(conn, org_id, project_id)?;
    let mut target = ancestors
        .last()
        .ok_or_else(|| anyhow!("project not found"))?
        .clone();
    target.modules_json = serde_json::to_string(&effective_modules_on(&ancestors)?)?;
    if ancestors.iter().any(|project| project.status != "active") {
        target.status = "archived".to_string();
    }
    if ancestors.iter().any(|project| project.lifecycle == "ended") {
        target.lifecycle = "ended".to_string();
    }
    let root_owner = ancestors[0].owner_user_id == user_id;
    let private_start = ancestors
        .iter()
        .rposition(|project| project.is_private)
        .unwrap_or(0);
    let mut effective_functions = HashSet::new();
    let mut catalogue = HashMap::<String, ProjectFunctionWire>::new();
    let mut effective_member: Option<MemberRecord> = None;
    for project in &ancestors[private_start..] {
        if removed_project_id == Some(project.project_id.as_str()) {
            continue;
        }
        let member = conn.query_row(
            &format!("SELECT {MEMBER_COLS} FROM project_members m WHERE m.project_id=?1 AND m.user_id=?2"),
            params![project.project_id,user_id], read_member,
        ).optional()?;
        let Some(member) = member.filter(|member| member.is_active_at(now)) else {
            continue;
        };
        let functions = list_functions_on(conn, &project.project_id)?;
        for function in functions
            .into_iter()
            .filter(|entry| member.functions.contains(&entry.function_id))
        {
            effective_functions.insert(function.function_id.clone());
            if let Some(existing) = catalogue.get_mut(&function.function_id) {
                for grant in function.grants {
                    if let Some(current) = existing
                        .grants
                        .iter_mut()
                        .find(|entry| entry.area == grant.area)
                    {
                        current.level = current.level.max(grant.level);
                    } else {
                        existing.grants.push(grant);
                    }
                }
            } else {
                catalogue.insert(function.function_id.clone(), function);
            }
        }
        if let Some(current) = effective_member.as_mut() {
            current.project_admin |= member.project_admin;
            current.expires_at = None;
        } else {
            effective_member = Some(MemberRecord {
                project_id: project_id.to_string(),
                ..member
            });
        }
    }
    if root_owner {
        if let Some(member) = effective_member.as_mut() {
            member.project_admin = true;
        } else {
            effective_member = Some(MemberRecord {
                project_id: project_id.to_string(),
                user_id: user_id.to_string(),
                functions: Vec::new(),
                project_admin: true,
                expires_at: None,
                invited_by: user_id.to_string(),
                created_at: String::new(),
            });
        }
    }
    let mut effective_functions: Vec<String> = effective_functions.into_iter().collect();
    effective_functions.sort();
    if let Some(member) = effective_member.as_mut() {
        member.functions = effective_functions;
        member.project_id = project_id.to_string();
    }
    let catalogue: Vec<ProjectFunctionWire> = catalogue.into_values().collect();
    Ok(super::models::evaluate_project_access(
        &target,
        effective_member.as_ref(),
        &catalogue,
        app_admin,
        now,
        root_owner,
    ))
}

#[derive(Debug, Clone)]
pub struct EffectivePrincipal {
    pub user_id: String,
    pub origin_project_ids: Vec<String>,
    pub access: ProjectAccessWire,
}

pub fn effective_principals(
    project_id: &str,
    area: ProjectArea,
    minimum: ProjectPermissionLevel,
) -> Result<Vec<EffectivePrincipal>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let snapshot = conn.unchecked_transaction()?;
    let result = effective_principals_on(&snapshot, project_id, area, minimum, chrono::Utc::now())?;
    snapshot.commit()?;
    Ok(result)
}

fn effective_principals_on(
    conn: &rusqlite::Connection,
    project_id: &str,
    area: ProjectArea,
    minimum: ProjectPermissionLevel,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<EffectivePrincipal>> {
    let org_id: String = conn.query_row(
        "SELECT org_id FROM projects WHERE project_id=?1",
        [project_id],
        |row| row.get(0),
    )?;
    let ancestors = project_ancestry_on(conn, &org_id, project_id)?;
    let private_start = ancestors
        .iter()
        .rposition(|project| project.is_private)
        .unwrap_or(0);
    let mut users = HashMap::<String, Vec<String>>::new();
    for project in &ancestors[private_start..] {
        let mut stmt = conn.prepare(&format!(
            "SELECT {MEMBER_COLS} FROM project_members m WHERE m.project_id=?1"
        ))?;
        for member in stmt.query_map([&project.project_id], read_member)? {
            let member = member?;
            if member.is_active_at(now) {
                users
                    .entry(member.user_id)
                    .or_default()
                    .push(project.project_id.clone());
            }
        }
    }
    let root = &ancestors[0];
    let origins = users.entry(root.owner_user_id.clone()).or_default();
    if !origins.contains(&root.project_id) {
        origins.push(root.project_id.clone());
    }
    let mut result = Vec::new();
    for (user_id, origin_project_ids) in users {
        let access = project_access_on(conn, &org_id, project_id, &user_id, false, now, None)?;
        if access.allows(area, minimum) {
            result.push(EffectivePrincipal {
                user_id,
                origin_project_ids,
                access,
            });
        }
    }
    result.sort_by(|left, right| left.user_id.cmp(&right.user_id));
    Ok(result)
}

pub fn effective_project_ids(
    org_id: &str,
    user_id: &str,
    app_admin: bool,
    area: ProjectArea,
    minimum: ProjectPermissionLevel,
    root_project_id: Option<&str>,
    include_ended: bool,
) -> Result<Vec<String>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let snapshot = conn.unchecked_transaction()?;
    let root_path = root_project_id
        .map(|id| project_ancestry_on(&snapshot, org_id, id))
        .transpose()?
        .and_then(|path| path.last().map(|row| row.path.clone()));
    let mut stmt = snapshot.prepare(
        "SELECT project_id,path,lifecycle,status FROM projects WHERE org_id=?1 ORDER BY path",
    )?;
    let rows = stmt.query_map([org_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let candidates = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    let now = chrono::Utc::now();
    let mut ids = Vec::new();
    for (id, path, lifecycle, status) in candidates {
        if status != "active" || (!include_ended && lifecycle != "active") {
            continue;
        }
        if let Some(root) = &root_path {
            if path != *root && !path.starts_with(&format!("{root}/")) {
                continue;
            }
        }
        let access = project_access_on(&snapshot, org_id, &id, user_id, app_admin, now, None)?;
        if access.allows(area, minimum) && (include_ended || !access.ended) {
            ids.push(id);
        }
    }
    drop(stmt);
    snapshot.commit()?;
    Ok(ids)
}

pub fn project_write_admission(project_id: &str) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let org_id: String = conn.query_row(
        "SELECT org_id FROM projects WHERE project_id=?1",
        [project_id],
        |row| row.get(0),
    )?;
    for project in project_ancestry_on(&conn, &org_id, project_id)? {
        if project.status != "active" || project.lifecycle != "active" {
            bail!("project is read-only");
        }
        let preparing: i64 = conn.query_row(
            "SELECT COUNT(*) FROM project_admissions WHERE project_id=?1",
            [&project.project_id],
            |row| row.get(0),
        )?;
        if preparing != 0 {
            bail!("project is preparing to end");
        }
    }
    Ok(())
}

pub fn task_write_admission(project_id: &str, task_id: &str) -> Result<()> {
    project_write_admission(project_id)?;
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let canonical: Option<String> = conn
        .query_row(
            "SELECT project_id FROM task_locations WHERE task_id=?1",
            [task_id],
            |row| row.get(0),
        )
        .optional()?;
    if canonical.is_some_and(|owner| owner != project_id) {
        bail!("task has moved to another project");
    }
    let fenced: i64 = conn.query_row(
        "SELECT COUNT(*) FROM task_transfer_fences WHERE task_id=?1",
        [task_id],
        |row| row.get(0),
    )?;
    if fenced > 0 {
        bail!("task is being transferred");
    }
    Ok(())
}

pub fn replace_projected_task_type_names(
    source_project_id: &str,
    revision: i64,
    types: &[TaskTypeRecord],
) -> Result<bool> {
    if revision < 1 || types.is_empty() {
        bail!("task type catalogue snapshot is empty or unversioned");
    }
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let applied: Option<i64> = tx
        .query_row(
            "SELECT revision FROM task_type_catalogue_cursor WHERE source_project_id=?1",
            [source_project_id],
            |row| row.get(0),
        )
        .optional()?;
    if applied.is_some_and(|current| current >= revision) {
        return Ok(false);
    }
    tx.execute(
        "DELETE FROM task_type_names WHERE source_project_id=?1",
        [source_project_id],
    )?;
    for kind in types {
        tx.execute(
            "INSERT INTO task_type_names(source_project_id,type_id,name) VALUES (?1,?2,?3)",
            params![source_project_id, kind.type_id, kind.name],
        )?;
    }
    tx.execute(
        "INSERT INTO task_type_catalogue_cursor(source_project_id,revision) VALUES (?1,?2) \
         ON CONFLICT(source_project_id) DO UPDATE SET revision=excluded.revision",
        params![source_project_id, revision],
    )?;
    tx.commit()?;
    Ok(true)
}

pub fn projected_task_type_names(source_project_id: &str) -> Result<HashMap<String, String>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let has_snapshot: i64 = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_type_catalogue_cursor WHERE source_project_id=?1)",
        [source_project_id],
        |row| row.get(0),
    )?;
    if has_snapshot != 1 {
        bail!("task type catalogue has not been projected");
    }
    let mut stmt = conn.prepare(
        "SELECT type_id,name FROM task_type_names WHERE source_project_id=?1 ORDER BY type_id",
    )?;
    let rows = stmt.query_map([source_project_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let types = rows.collect::<rusqlite::Result<HashMap<_, _>>>()?;
    Ok(types)
}

fn apply_task_index_event(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
    org_id: &str,
    event: &TaskIndexEvent,
) -> Result<()> {
    match event.op.as_str() {
        "delete" => {
            tx.execute(
                "DELETE FROM task_index WHERE project_id=?1 AND task_id=?2",
                params![project_id, event.task_id],
            )?;
            tx.execute(
                "DELETE FROM task_locations WHERE task_id=?1 AND project_id=?2",
                params![event.task_id, project_id],
            )?;
        }
        "upsert" => {
            let task: TaskIndexSnapshot = serde_json::from_str(&event.snapshot_json)?;
            if task.task_id != event.task_id || task.task_key.is_empty() {
                bail!("task index snapshot identity does not match event");
            }
            let _: Vec<serde_json::Value> = serde_json::from_str(&task.links_json)?;
            let conflicting_alias: Option<String> = tx
                .query_row(
                    "SELECT task_id FROM task_key_aliases WHERE org_id=?1 AND alias_key=?2",
                    params![org_id, task.task_key],
                    |row| row.get(0),
                )
                .optional()?;
            if conflicting_alias.is_some_and(|id| id != task.task_id) {
                bail!("task key is permanently reserved by another task");
            }
            tx.execute(
                "INSERT INTO task_index(project_id,task_id,org_id,revision,task_key,task_no,task_type,\
                 title,severity,priority,status,status_category,assigned_to,due_date,parent_task_id,\
                 links_json,comment_count,created_by,created_at,updated_at,archived_at,resolution,resolution_reason) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23) \
                 ON CONFLICT(project_id,task_id) DO UPDATE SET revision=excluded.revision,\
                 task_key=excluded.task_key,task_no=excluded.task_no,task_type=excluded.task_type,\
                 title=excluded.title,severity=excluded.severity,priority=excluded.priority,\
                 status=excluded.status,status_category=excluded.status_category,\
                 assigned_to=excluded.assigned_to,due_date=excluded.due_date,\
                 parent_task_id=excluded.parent_task_id,links_json=excluded.links_json,\
                 comment_count=excluded.comment_count,\
                 created_by=excluded.created_by,created_at=excluded.created_at,\
                 updated_at=excluded.updated_at,archived_at=excluded.archived_at,\
                 resolution=excluded.resolution,resolution_reason=excluded.resolution_reason",
                params![
                    project_id, task.task_id, org_id, event.revision, task.task_key, task.task_no,
                    task.task_type, task.title, task.severity, task.priority, task.status,
                    if task.status == "done" { "done" } else { "open" }, task.assigned_to,
                    task.due_date, task.parent_task_id,task.links_json,task.comment_count, task.created_by,
                    task.created_at, task.updated_at, task.archived_at,task.resolution,
                    task.resolution_reason,
                ],
            )?;
            let current: Option<String> = tx
                .query_row(
                    "SELECT project_id FROM task_locations WHERE task_id=?1",
                    [&event.task_id],
                    |row| row.get(0),
                )
                .optional()?;
            if current.is_none() {
                tx.execute(
                    "INSERT INTO task_locations(task_id,org_id,project_id,current_key) VALUES (?1,?2,?3,?4)",
                    params![event.task_id,org_id,project_id,task.task_key],
                )?;
            } else if current.as_deref() == Some(project_id) {
                tx.execute(
                    "UPDATE task_locations SET current_key=?1 WHERE task_id=?2",
                    params![task.task_key, event.task_id],
                )?;
            }
            if current.is_none() || current.as_deref() == Some(project_id) {
                tx.execute(
                    "INSERT INTO task_key_aliases(org_id,alias_key,task_id,target_project_id,current_key) \
                     VALUES (?1,?2,?3,?4,?2) ON CONFLICT(org_id,alias_key) DO UPDATE SET \
                     target_project_id=excluded.target_project_id,current_key=excluded.current_key \
                     WHERE task_key_aliases.task_id=excluded.task_id",
                    params![org_id,task.task_key,event.task_id,project_id],
                )?;
            }
        }
        _ => bail!("unknown task index operation"),
    }
    Ok(())
}

pub fn upsert_task_index_events(
    project_id: &str,
    org_id: &str,
    events: &[TaskIndexEvent],
) -> Result<i64> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let actual_org: String = tx.query_row(
        "SELECT org_id FROM projects WHERE project_id=?1",
        [project_id],
        |row| row.get(0),
    )?;
    if actual_org != org_id {
        bail!("task index organization mismatch");
    }
    let mut cursor: i64 = tx
        .query_row(
            "SELECT last_revision FROM task_index_cursor WHERE project_id=?1",
            [project_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);
    for event in events {
        if event.revision <= cursor {
            continue;
        }
        if event.revision != cursor + 1 {
            bail!("task index revision gap");
        }
        let current: Option<String> = tx
            .query_row(
                "SELECT project_id FROM task_locations WHERE task_id=?1",
                [&event.task_id],
                |row| row.get(0),
            )
            .optional()?;
        if event.op == "delete" || current.is_none() || current.as_deref() == Some(project_id) {
            apply_task_index_event(&tx, project_id, org_id, event)?;
        }
        cursor = event.revision;
    }
    tx.execute(
        "INSERT INTO task_index_cursor(project_id,last_revision,observed_source_revision,indexed_at,observed_at) \
         VALUES (?1,?2,?2,datetime('now'),datetime('now')) \
         ON CONFLICT(project_id) DO UPDATE SET last_revision=excluded.last_revision,\
         observed_source_revision=MAX(task_index_cursor.observed_source_revision,excluded.observed_source_revision),\
         indexed_at=excluded.indexed_at,observed_at=excluded.observed_at",
        params![project_id,cursor],
    )?;
    tx.commit()?;
    Ok(cursor)
}

pub fn replace_project_task_index(
    project_id: &str,
    org_id: &str,
    watermark: i64,
    snapshots: &[TaskIndexEvent],
) -> Result<()> {
    if watermark < 0 {
        bail!("negative task index watermark");
    }
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let actual_org: String = tx.query_row(
        "SELECT org_id FROM projects WHERE project_id=?1",
        [project_id],
        |row| row.get(0),
    )?;
    if actual_org != org_id {
        bail!("task index organization mismatch");
    }
    let pending_copy: i64 = tx.query_row(
        "SELECT COUNT(*) FROM task_transfer_journal WHERE phase IN ('prepared','copied') \
         AND (source_project_id=?1 OR destination_project_id=?1)",
        [project_id],
        |row| row.get(0),
    )?;
    if pending_copy != 0 {
        bail!("task index rebuild waits for transfer publication");
    }
    let cursor: i64 = tx
        .query_row(
            "SELECT last_revision FROM task_index_cursor WHERE project_id=?1",
            [project_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);
    if watermark < cursor {
        bail!("task index rebuild is older than applied revision");
    }
    tx.execute("DELETE FROM task_index WHERE project_id=?1", [project_id])?;
    tx.execute(
        "DELETE FROM task_locations WHERE project_id=?1",
        [project_id],
    )?;
    for snapshot in snapshots {
        if snapshot.op != "upsert" || snapshot.revision > watermark {
            bail!("invalid task index rebuild snapshot");
        }
        let current: Option<String> = tx
            .query_row(
                "SELECT project_id FROM task_locations WHERE task_id=?1",
                [&snapshot.task_id],
                |row| row.get(0),
            )
            .optional()?;
        if current.is_none() || current.as_deref() == Some(project_id) {
            apply_task_index_event(&tx, project_id, org_id, snapshot)?;
        }
    }
    tx.execute(
        "INSERT INTO task_index_cursor(project_id,last_revision,observed_source_revision,indexed_at,observed_at) \
         VALUES (?1,?2,?2,datetime('now'),datetime('now')) \
         ON CONFLICT(project_id) DO UPDATE SET last_revision=excluded.last_revision,\
         observed_source_revision=MAX(task_index_cursor.observed_source_revision,excluded.observed_source_revision),\
         indexed_at=excluded.indexed_at,observed_at=excluded.observed_at",
        params![project_id,watermark],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn record_task_index_observation(project_id: &str, source_revision: i64) -> Result<()> {
    if source_revision < 0 {
        bail!("negative task source revision");
    }
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "INSERT INTO task_index_cursor(project_id,last_revision,observed_source_revision,observed_at) \
         VALUES (?1,0,?2,datetime('now')) \
         ON CONFLICT(project_id) DO UPDATE SET observed_source_revision=\
         MAX(task_index_cursor.observed_source_revision,excluded.observed_source_revision),\
         observed_at=excluded.observed_at",
        params![project_id,source_revision],
    )?;
    Ok(())
}

pub fn task_index_status(project_id: &str, source_revision: i64) -> Result<TaskIndexStatus> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let (applied,observed,indexed_at): (i64,i64,Option<String>) = conn.query_row(
        "SELECT last_revision,observed_source_revision,indexed_at FROM task_index_cursor WHERE project_id=?1",
        [project_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
    ).optional()?.unwrap_or((0,0,None));
    let source_revision = source_revision.max(observed);
    Ok(TaskIndexStatus {
        project_id: project_id.to_string(),
        source_revision,
        applied_revision: applied,
        lag: (source_revision - applied).max(0),
        indexed_at,
    })
}

fn task_index_conditions(
    allowed_project_ids: &[String],
    filter: &TaskIndexFilter,
    include_status: bool,
) -> (String, Vec<rusqlite::types::Value>) {
    use rusqlite::types::Value;
    let mut conditions = vec![format!(
        "i.project_id IN ({}) AND EXISTS (SELECT 1 FROM task_locations l WHERE l.task_id=i.task_id AND l.project_id=i.project_id)",
        vec!["?"; allowed_project_ids.len()].join(",")
    )];
    let mut values: Vec<Value> = allowed_project_ids
        .iter()
        .cloned()
        .map(Value::Text)
        .collect();
    if !filter.include_archived {
        conditions.push("i.archived_at IS NULL".to_string());
    }
    for (column, value) in [
        ("i.task_type", &filter.task_type),
        ("i.assigned_to", &filter.assigned_to),
        ("i.severity", &filter.severity),
    ] {
        if !value.is_empty() {
            conditions.push(format!("{column}=?"));
            values.push(Value::Text(value.clone()));
        }
    }
    if include_status && !filter.status.is_empty() {
        conditions.push("i.status=?".to_string());
        values.push(Value::Text(filter.status.clone()));
    }
    if !filter.search.trim().is_empty() {
        conditions
            .push("(i.title LIKE ? ESCAPE '\\' OR i.task_key LIKE ? ESCAPE '\\')".to_string());
        let search = Value::Text(format!("%{}%", escape_like(filter.search.trim())));
        values.push(search.clone());
        values.push(search);
    }
    (conditions.join(" AND "), values)
}

pub fn list_indexed_tasks(
    allowed_project_ids: &[String],
    filter: &TaskIndexFilter,
) -> Result<(Vec<IndexedTask>, u32)> {
    use rusqlite::types::Value;
    if allowed_project_ids.is_empty() {
        return Ok((Vec::new(), 0));
    }
    if filter.limit == 0 || filter.limit > 200 {
        bail!("task index page limit must be 1..=200");
    }
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let (where_sql, mut values) = task_index_conditions(allowed_project_ids, filter, true);
    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM task_index i WHERE {where_sql}"),
        rusqlite::params_from_iter(values.iter()),
        |row| row.get(0),
    )?;
    let mut stmt = conn.prepare(&format!(
        "SELECT i.project_id,p.name,i.org_id,i.revision,i.task_id,i.task_no,i.task_key,\
         i.task_type,i.title,i.severity,i.priority,i.status,i.assigned_to,i.due_date,\
         i.parent_task_id,i.links_json,i.comment_count,i.created_by,i.created_at,i.updated_at,i.archived_at,\
         i.resolution,i.resolution_reason \
         FROM task_index i JOIN projects p ON p.project_id=i.project_id \
         WHERE {where_sql} ORDER BY i.updated_at DESC,i.project_id,i.task_id LIMIT ? OFFSET ?"
    ))?;
    values.push(Value::Integer(i64::from(filter.limit)));
    values.push(Value::Integer(i64::from(filter.offset)));
    let rows = stmt.query_map(rusqlite::params_from_iter(values.iter()), |row| {
        Ok(IndexedTask {
            project_id: row.get(0)?,
            project_name: row.get(1)?,
            org_id: row.get(2)?,
            revision: row.get(3)?,
            snapshot: TaskIndexSnapshot {
                task_id: row.get(4)?,
                task_no: row.get(5)?,
                task_key: row.get(6)?,
                task_type: row.get(7)?,
                title: row.get(8)?,
                severity: row.get(9)?,
                priority: row.get(10)?,
                status: row.get(11)?,
                assigned_to: row.get(12)?,
                due_date: row.get(13)?,
                parent_task_id: row.get(14)?,
                links_json: row.get(15)?,
                comment_count: row.get(16)?,
                created_by: row.get(17)?,
                created_at: row.get(18)?,
                updated_at: row.get(19)?,
                archived_at: row.get(20)?,
                resolution: row.get(21)?,
                resolution_reason: row.get(22)?,
            },
        })
    })?;
    let tasks = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((tasks, u32::try_from(total)?))
}

pub fn task_aggregate_summary(
    allowed_project_ids: &[String],
    root_project_id: &str,
    filter: &TaskIndexFilter,
) -> Result<TaskAggregateSummary> {
    if allowed_project_ids.is_empty() {
        return Ok(TaskAggregateSummary::default());
    }
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let (where_sql, values) = task_index_conditions(allowed_project_ids, filter, false);
    let counts: (i64, i64, i64, i64, i64, i64) = conn.query_row(
        &format!(
            "SELECT COUNT(*) FILTER (WHERE i.status='todo'),\
             COUNT(*) FILTER (WHERE i.status='in_progress'),\
             COUNT(*) FILTER (WHERE i.status='review'),\
             COUNT(*) FILTER (WHERE i.status='done'),\
             COUNT(*) FILTER (WHERE i.project_id=?),\
             COUNT(*) FILTER (WHERE i.project_id<>?) \
             FROM task_index i WHERE {where_sql}"
        ),
        rusqlite::params_from_iter(
            [
                rusqlite::types::Value::Text(root_project_id.to_string()),
                rusqlite::types::Value::Text(root_project_id.to_string()),
            ]
            .into_iter()
            .chain(values.iter().cloned()),
        ),
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        },
    )?;
    let mut source_revision = 0i64;
    let mut applied_revision = 0i64;
    let mut indexed_at: Option<String> = None;
    let mut all_indexed = true;
    for project_id in allowed_project_ids {
        let cursor: Option<(i64,i64,Option<String>)> = conn.query_row(
            "SELECT observed_source_revision,last_revision,indexed_at FROM task_index_cursor WHERE project_id=?1",
            [project_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        ).optional()?;
        if let Some((source, applied, at)) = cursor {
            source_revision += source;
            applied_revision += applied;
            if at.is_none() {
                all_indexed = false;
            }
            indexed_at = match (indexed_at, at) {
                (Some(left), Some(right)) => Some(left.min(right)),
                (Some(_), None) | (None, None) => None,
                (None, Some(right)) => Some(right),
            };
        } else {
            all_indexed = false;
        }
    }
    if !all_indexed {
        indexed_at = None;
    }
    Ok(TaskAggregateSummary {
        todo: u32::try_from(counts.0)?,
        in_progress: u32::try_from(counts.1)?,
        review: u32::try_from(counts.2)?,
        done: u32::try_from(counts.3)?,
        own_total: u32::try_from(counts.4)?,
        descendant_total: u32::try_from(counts.5)?,
        source_revision,
        applied_revision,
        indexed_at,
    })
}

pub fn project_task_counts(
    allowed_project_ids: &[String],
    display_project_ids: &[String],
    user_id: &str,
) -> Result<HashMap<String, ProjectTaskCounts>> {
    if display_project_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let mut paths = HashMap::new();
    for project_id in display_project_ids {
        let path: String = conn.query_row(
            "SELECT path FROM projects WHERE project_id=?1",
            [project_id],
            |row| row.get(0),
        )?;
        paths.insert(project_id.as_str(), path);
    }
    let mut result: HashMap<String, ProjectTaskCounts> = display_project_ids
        .iter()
        .cloned()
        .map(|id| (id, ProjectTaskCounts::default()))
        .collect();
    if allowed_project_ids.is_empty() {
        return Ok(result);
    }
    for project_id in allowed_project_ids {
        if !paths.contains_key(project_id.as_str()) {
            let path: String = conn.query_row(
                "SELECT path FROM projects WHERE project_id=?1",
                [project_id],
                |row| row.get(0),
            )?;
            paths.insert(project_id.as_str(), path);
        }
    }
    let placeholders = vec!["?"; allowed_project_ids.len()].join(",");
    let mut stmt = conn.prepare(&format!(
        "SELECT project_id,COUNT(*),\
         COUNT(*) FILTER (WHERE assigned_to=?),\
         COUNT(*) FILTER (WHERE due_date<>'' AND due_date<date('now')) \
         FROM task_index i WHERE project_id IN ({placeholders}) \
         AND status_category='open' AND archived_at IS NULL \
         AND EXISTS (SELECT 1 FROM task_locations l WHERE l.task_id=i.task_id AND l.project_id=i.project_id) \
         GROUP BY project_id"
    ))?;
    let values = std::iter::once(rusqlite::types::Value::Text(user_id.to_string())).chain(
        allowed_project_ids
            .iter()
            .cloned()
            .map(rusqlite::types::Value::Text),
    );
    let rows = stmt.query_map(rusqlite::params_from_iter(values), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    let direct = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    for (project_id, open, my_open, overdue) in &direct {
        if let Some(own) = result.get_mut(project_id) {
            own.own_open = u32::try_from(*open)?;
            own.my_open = u32::try_from(*my_open)?;
            own.overdue = u32::try_from(*overdue)?;
        }
    }
    for (project_id, open, my_open, overdue) in &direct {
        let Some(path) = paths.get(project_id.as_str()) else {
            continue;
        };
        for (ancestor_id, ancestor_path) in display_project_ids
            .iter()
            .filter_map(|id| paths.get(id.as_str()).map(|path| (id, path)))
        {
            if ancestor_id != project_id && path.starts_with(&format!("{ancestor_path}/")) {
                let ancestor = result
                    .get_mut(ancestor_id)
                    .expect("display project has counts");
                ancestor.descendant_open += u32::try_from(*open)?;
                ancestor.my_open += u32::try_from(*my_open)?;
                ancestor.overdue += u32::try_from(*overdue)?;
            }
        }
    }
    Ok(result)
}

pub fn task_location(task_id: &str) -> Result<Option<TaskLocation>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        "SELECT task_id,org_id,project_id,current_key FROM task_locations WHERE task_id=?1",
        [task_id],
        |row| {
            Ok(TaskLocation {
                task_id: row.get(0)?,
                org_id: row.get(1)?,
                project_id: row.get(2)?,
                current_key: row.get(3)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

pub fn resolve_task_key(org_id: &str, key: &str) -> Result<Option<TaskLocation>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        "SELECT task_id,org_id,project_id,current_key FROM task_locations \
         WHERE org_id=?1 AND current_key=?2 UNION ALL \
         SELECT l.task_id,l.org_id,l.project_id,l.current_key \
         FROM task_key_aliases a JOIN task_locations l ON l.task_id=a.task_id \
         WHERE a.org_id=?1 AND a.alias_key=?2 LIMIT 1",
        params![org_id, key],
        |row| {
            Ok(TaskLocation {
                task_id: row.get(0)?,
                org_id: row.get(1)?,
                project_id: row.get(2)?,
                current_key: row.get(3)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

pub fn task_key_reserved(org_id: &str, key: &str) -> Result<bool> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let reserved: i64 = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_key_aliases WHERE org_id=?1 AND alias_key=?2) \
         OR EXISTS(SELECT 1 FROM task_locations WHERE org_id=?1 AND current_key=?2)",
        params![org_id, key],
        |row| row.get(0),
    )?;
    Ok(reserved != 0)
}

pub fn resolve_task_event(
    task_id: &str,
    origin_project_id: &str,
    origin_event_id: i64,
) -> Result<Option<(String, i64)>> {
    if origin_event_id <= 0 {
        return Ok(None);
    }
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let alias = conn
        .query_row(
            "SELECT a.current_project_id,a.current_event_id FROM task_event_aliases a \
         JOIN task_locations l ON l.task_id=a.task_id AND l.project_id=a.current_project_id \
         WHERE a.task_id=?1 AND a.origin_project_id=?2 AND a.origin_event_id=?3",
            params![task_id, origin_project_id, origin_event_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if alias.is_some() {
        return Ok(alias);
    }
    let current: Option<String> = conn
        .query_row(
            "SELECT project_id FROM task_locations WHERE task_id=?1",
            [task_id],
            |row| row.get(0),
        )
        .optional()?;
    drop(conn);
    if current.as_deref() != Some(origin_project_id) {
        return Ok(None);
    }
    let project_pool = super::project_db::open(origin_project_id)?;
    let actual: i64 = project_pool.read().map_err(read_err)?.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_events WHERE event_id=?1 AND task_id=?2)",
        params![origin_event_id, task_id],
        |row| row.get(0),
    )?;
    if actual != 1 {
        return Ok(None);
    }
    let current = task_location(task_id)?;
    if current
        .as_ref()
        .map(|location| location.project_id.as_str())
        != Some(origin_project_id)
    {
        return Ok(None);
    }
    Ok(Some((origin_project_id.to_string(), origin_event_id)))
}

pub fn resolve_task_link(
    origin_project_id: &str,
    origin_link_id: i64,
) -> Result<Option<TaskLinkAlias>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        "SELECT a.relation_id,a.origin_project_id,a.origin_link_id,r.owning_project_id,r.link_id \
         FROM task_link_aliases a JOIN task_relation_routes r ON r.relation_id=a.relation_id \
         WHERE a.origin_project_id=?1 AND a.origin_link_id=?2 \
         AND NOT EXISTS (SELECT 1 FROM task_relation_deletions d WHERE d.relation_id=r.relation_id)",
        params![origin_project_id, origin_link_id],
        |row| {
            Ok(TaskLinkAlias {
                relation_id: row.get(0)?,
                origin_project_id: row.get(1)?,
                origin_link_id: row.get(2)?,
                current_project_id: row.get(3)?,
                current_link_id: row.get(4)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn read_relation_route(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRelationRoute> {
    Ok(TaskRelationRoute {
        relation_id: row.get(0)?,
        owning_project_id: row.get(1)?,
        link_id: row.get(2)?,
        source_task_id: row.get(3)?,
        target_task_id: row.get(4)?,
        source_project_id: row.get(5)?,
        target_project_id: row.get(6)?,
        kind: row.get(7)?,
    })
}

pub fn relation_routes_for_task(task_id: &str) -> Result<Vec<TaskRelationRoute>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(
        "SELECT r.relation_id,r.owning_project_id,r.link_id,r.source_task_id,r.target_task_id,\
         r.source_project_id,r.target_project_id,r.kind FROM task_relation_routes r \
         WHERE (r.source_task_id=?1 OR r.target_task_id=?1) \
         AND NOT EXISTS (SELECT 1 FROM task_relation_deletions d WHERE d.relation_id=r.relation_id) \
         ORDER BY r.relation_id",
    )?;
    let rows = stmt.query_map([task_id], read_relation_route)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

pub fn relation_routes_owned_by(project_id: &str) -> Result<Vec<TaskRelationRoute>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(
        "SELECT relation_id,owning_project_id,link_id,source_task_id,target_task_id,\
         source_project_id,target_project_id,kind FROM task_relation_routes \
         WHERE owning_project_id=?1 ORDER BY relation_id",
    )?;
    let rows = stmt.query_map([project_id], read_relation_route)?;
    let routes = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(routes)
}

pub fn prepare_task_relation_route(route: &TaskRelationRoute) -> Result<()> {
    if uuid::Uuid::parse_str(&route.relation_id).is_err()
        || route.source_task_id == route.target_task_id
        || route.link_id != 0
        || route.owning_project_id != route.source_project_id
        || !["related", "duplicate", "fs", "ss", "ff", "sf"].contains(&route.kind.as_str())
    {
        bail!("invalid task relation admission");
    }
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let existing: Option<(String, String, String, String, String, String)> = tx
        .query_row(
            "SELECT owning_project_id,source_task_id,target_task_id,source_project_id,\
         target_project_id,kind FROM task_relation_admissions WHERE relation_id=?1",
            [&route.relation_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing
            == (
                route.owning_project_id.clone(),
                route.source_task_id.clone(),
                route.target_task_id.clone(),
                route.source_project_id.clone(),
                route.target_project_id.clone(),
                route.kind.clone(),
            )
        {
            return Ok(());
        }
        bail!("task relation admission identity changed");
    }
    let mut relation_org: Option<String> = None;
    for (task_id, project_id) in [
        (&route.source_task_id, &route.source_project_id),
        (&route.target_task_id, &route.target_project_id),
    ] {
        let actual: Option<(String, String)> = tx
            .query_row(
                "SELECT project_id,org_id FROM task_locations WHERE task_id=?1",
                [task_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((current_project, current_org)) = actual else {
            bail!("task relation endpoint missing");
        };
        if current_project != project_id.as_str()
            || relation_org.as_ref().is_some_and(|org| org != &current_org)
        {
            bail!("task relation endpoint moved or belongs to another organization");
        }
        relation_org = Some(current_org);
        let fenced: i64 = tx.query_row(
            "SELECT COUNT(*) FROM task_transfer_fences WHERE task_id=?1",
            [task_id],
            |row| row.get(0),
        )?;
        if fenced != 0 {
            bail!("task relation endpoint is being transferred");
        }
    }
    let duplicate: i64 = tx.query_row(
        "SELECT COUNT(*) FROM (\
         SELECT source_task_id,target_task_id,kind FROM task_relation_routes \
         UNION ALL SELECT source_task_id,target_task_id,kind FROM task_relation_admissions) \
         WHERE kind=?1 AND ((source_task_id=?2 AND target_task_id=?3) OR \
         (?4 AND source_task_id=?3 AND target_task_id=?2))",
        params![
            route.kind,
            route.source_task_id,
            route.target_task_id,
            matches!(route.kind.as_str(), "related" | "duplicate")
        ],
        |row| row.get(0),
    )?;
    if duplicate != 0 {
        bail!("task relation already exists");
    }
    if !matches!(route.kind.as_str(), "related" | "duplicate") {
        let cycle: i64 = tx.query_row(
            "WITH RECURSIVE edges(source_task_id,target_task_id) AS (\
             SELECT source_task_id,target_task_id FROM task_relation_routes \
             WHERE kind IN ('fs','ss','ff','sf') UNION ALL \
             SELECT source_task_id,target_task_id FROM task_relation_admissions \
             WHERE kind IN ('fs','ss','ff','sf')), \
             reachable(task_id) AS (SELECT ?1 UNION SELECT e.target_task_id FROM edges e \
             JOIN reachable r ON e.source_task_id=r.task_id) \
             SELECT EXISTS(SELECT 1 FROM reachable WHERE task_id=?2)",
            params![route.target_task_id, route.source_task_id],
            |row| row.get(0),
        )?;
        if cycle != 0 {
            bail!("task dependency would create a cycle");
        }
    }
    tx.execute(
        "INSERT INTO task_relation_admissions(relation_id,owning_project_id,source_task_id,\
         target_task_id,source_project_id,target_project_id,kind) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            route.relation_id,
            route.owning_project_id,
            route.source_task_id,
            route.target_task_id,
            route.source_project_id,
            route.target_project_id,
            route.kind
        ],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn publish_task_relation_route(route: &TaskRelationRoute) -> Result<()> {
    if route.link_id <= 0 {
        bail!("task relation link id is required");
    }
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let admission: Option<(String, String, String, String, String, String)> = tx
        .query_row(
            "SELECT owning_project_id,source_task_id,target_task_id,source_project_id,\
         target_project_id,kind FROM task_relation_admissions WHERE relation_id=?1",
            [&route.relation_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some(admission) = admission else {
        let existing: Option<TaskRelationRoute> = tx.query_row(
            "SELECT relation_id,owning_project_id,link_id,source_task_id,target_task_id,\
             source_project_id,target_project_id,kind FROM task_relation_routes WHERE relation_id=?1",
            [&route.relation_id],read_relation_route,
        ).optional()?;
        if existing.as_ref() == Some(route) {
            return Ok(());
        }
        bail!("task relation admission missing or changed");
    };
    if admission
        != (
            route.owning_project_id.clone(),
            route.source_task_id.clone(),
            route.target_task_id.clone(),
            route.source_project_id.clone(),
            route.target_project_id.clone(),
            route.kind.clone(),
        )
    {
        bail!("task relation differs from admission");
    }
    for (task_id, project_id) in [
        (&route.source_task_id, &route.source_project_id),
        (&route.target_task_id, &route.target_project_id),
    ] {
        let actual: String = tx.query_row(
            "SELECT project_id FROM task_locations WHERE task_id=?1",
            [task_id],
            |row| row.get(0),
        )?;
        if actual != project_id.as_str() {
            bail!("task relation endpoint moved before publication");
        }
    }
    tx.execute(
        "INSERT INTO task_relation_routes(relation_id,owning_project_id,link_id,source_task_id,\
         target_task_id,source_project_id,target_project_id,kind) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            route.relation_id,
            route.owning_project_id,
            route.link_id,
            route.source_task_id,
            route.target_task_id,
            route.source_project_id,
            route.target_project_id,
            route.kind
        ],
    )?;
    tx.execute(
        "INSERT INTO task_link_aliases(relation_id,origin_project_id,origin_link_id,\
         current_project_id,current_link_id) VALUES (?1,?2,?3,?2,?3)",
        params![route.relation_id, route.owning_project_id, route.link_id],
    )?;
    tx.execute(
        "DELETE FROM task_relation_admissions WHERE relation_id=?1",
        [&route.relation_id],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn abort_task_relation_route(relation_id: &str) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "DELETE FROM task_relation_admissions WHERE relation_id=?1",
        [relation_id],
    )?;
    Ok(())
}

pub fn prepare_task_relation_deletion(route: &TaskRelationRoute, actor: &str) -> Result<()> {
    if actor.is_empty() || route.link_id <= 0 {
        bail!("task relation deletion needs a committed route and actor");
    }
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let current: TaskRelationRoute = tx.query_row(
        "SELECT relation_id,owning_project_id,link_id,source_task_id,target_task_id,\
         source_project_id,target_project_id,kind FROM task_relation_routes WHERE relation_id=?1",
        [&route.relation_id],
        read_relation_route,
    )?;
    if current != *route {
        bail!("task relation changed before deletion");
    }
    let prior: Option<String> = tx
        .query_row(
            "SELECT actor_user_id FROM task_relation_deletions WHERE relation_id=?1",
            [&route.relation_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(prior) = prior {
        if prior == actor {
            return Ok(());
        }
        bail!("task relation deletion already admitted for another actor");
    }
    tx.execute(
        "INSERT INTO task_relation_deletions(relation_id,actor_user_id) VALUES (?1,?2)",
        params![route.relation_id, actor],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn abort_task_relation_deletion(relation_id: &str) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "DELETE FROM task_relation_deletions WHERE relation_id=?1",
        [relation_id],
    )?;
    Ok(())
}

pub fn finish_task_relation_deletion(relation_id: &str) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let admitted: i64 = tx.query_row(
        "SELECT COUNT(*) FROM task_relation_deletions WHERE relation_id=?1",
        [relation_id],
        |row| row.get(0),
    )?;
    if admitted != 1 {
        bail!("task relation deletion admission missing");
    }
    tx.execute(
        "DELETE FROM task_relation_deletions WHERE relation_id=?1",
        [relation_id],
    )?;
    let removed = tx.execute(
        "DELETE FROM task_relation_routes WHERE relation_id=?1",
        [relation_id],
    )?;
    if removed != 1 {
        bail!("task relation route disappeared before deletion completed");
    }
    tx.commit()?;
    Ok(())
}

pub fn pending_task_relation_deletions(project_id: &str) -> Result<Vec<TaskRelationRoute>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(
        "SELECT r.relation_id,r.owning_project_id,r.link_id,r.source_task_id,r.target_task_id,\
         r.source_project_id,r.target_project_id,r.kind FROM task_relation_deletions d \
         JOIN task_relation_routes r ON r.relation_id=d.relation_id \
         WHERE r.owning_project_id=?1 ORDER BY d.created_at,d.relation_id",
    )?;
    let rows = stmt.query_map([project_id], read_relation_route)?;
    let routes = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(routes)
}

pub fn pending_task_relation_admissions() -> Result<Vec<TaskRelationRoute>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(
        "SELECT relation_id,owning_project_id,0,source_task_id,target_task_id,\
         source_project_id,target_project_id,kind FROM task_relation_admissions ORDER BY created_at,relation_id",
    )?;
    let rows = stmt.query_map([], read_relation_route)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn read_transfer(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskTransferJournal> {
    let task_ids_json: String = row.get(5)?;
    let task_ids = serde_json::from_str(&task_ids_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(TaskTransferJournal {
        operation_id: row.get(0)?,
        org_id: row.get(1)?,
        source_project_id: row.get(2)?,
        destination_project_id: row.get(3)?,
        actor_user_id: row.get(4)?,
        task_ids,
        consent_wider_access: row.get(6)?,
        phase: row.get(7)?,
        sha_manifest_json: row.get(8)?,
        key_map_json: row.get(9)?,
        event_map_json: row.get(10)?,
    })
}

fn transfer_on(conn: &rusqlite::Connection, operation_id: &str) -> Result<TaskTransferJournal> {
    conn.query_row(
        "SELECT operation_id,org_id,source_project_id,destination_project_id,actor_user_id,\
         task_ids_json,consent_wider_access,phase,sha_manifest_json,key_map_json,event_map_json \
         FROM task_transfer_journal WHERE operation_id=?1",
        [operation_id],
        read_transfer,
    )
    .optional()?
    .ok_or_else(|| anyhow!("task transfer not found"))
}

fn require_transfer_actor_on(
    conn: &rusqlite::Connection,
    operation: &TaskTransferJournal,
) -> Result<()> {
    let now = chrono::Utc::now();
    for project_id in [
        &operation.source_project_id,
        &operation.destination_project_id,
    ] {
        let access = project_access_on(
            conn,
            &operation.org_id,
            project_id,
            &operation.actor_user_id,
            false,
            now.clone(),
            None,
        )?;
        if !access.is_owner || !access.allows(ProjectArea::Tasks, ProjectPermissionLevel::Write) {
            bail!("task transfer actor no longer owns writable Tasks access");
        }
    }
    Ok(())
}

fn transfer_readers_on(
    conn: &rusqlite::Connection,
    project_id: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<HashSet<String>> {
    let mut readers = HashSet::new();
    for area in [ProjectArea::Tasks, ProjectArea::Board] {
        for principal in
            effective_principals_on(conn, project_id, area, ProjectPermissionLevel::Read, now)?
        {
            readers.insert(principal.user_id);
        }
    }
    Ok(readers)
}

fn require_transfer_readers_on(
    conn: &rusqlite::Connection,
    operation: &TaskTransferJournal,
    snapshot_project_id: &str,
) -> Result<()> {
    let now = chrono::Utc::now();
    let destination = transfer_readers_on(conn, &operation.destination_project_id, now)?;
    if !operation.consent_wider_access {
        let source = transfer_readers_on(conn, &operation.source_project_id, now)?;
        if !destination.is_subset(&source) {
            bail!("task transfer would widen current task readers without consent");
        }
    }
    for task_id in &operation.task_ids {
        let assigned_to: String = conn.query_row(
            "SELECT assigned_to FROM task_index WHERE project_id=?1 AND task_id=?2",
            params![snapshot_project_id, task_id],
            |row| row.get(0),
        )?;
        if !assigned_to.is_empty() && !destination.contains(&assigned_to) {
            bail!("task assignee lacks current destination task read access");
        }
    }
    Ok(())
}

pub fn prepare_task_transfer(operation: &TaskTransferJournal) -> Result<()> {
    if operation.source_project_id == operation.destination_project_id
        || operation.task_ids.is_empty()
    {
        bail!("task transfer needs distinct projects and at least one task");
    }
    let unique: HashSet<&str> = operation.task_ids.iter().map(String::as_str).collect();
    if unique.len() != operation.task_ids.len() {
        bail!("duplicate transfer task");
    }
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    require_transfer_actor_on(&tx, operation)?;
    let existing_id: Option<String> = tx
        .query_row(
            "SELECT operation_id FROM task_transfer_journal WHERE operation_id=?1",
            [&operation.operation_id],
            |row| row.get(0),
        )
        .optional()?;
    if existing_id.is_some() {
        let existing = transfer_on(&tx, &operation.operation_id)?;
        if existing.org_id == operation.org_id
            && existing.source_project_id == operation.source_project_id
            && existing.destination_project_id == operation.destination_project_id
            && existing.task_ids == operation.task_ids
        {
            return Ok(());
        }
        bail!("task transfer operation id conflicts with existing journal");
    }
    for project_id in [
        &operation.source_project_id,
        &operation.destination_project_id,
    ] {
        let ancestors = project_ancestry_on(&tx, &operation.org_id, project_id)?;
        if ancestors
            .iter()
            .any(|project| project.status != "active" || project.lifecycle != "active")
        {
            bail!("task transfer project is not writable");
        }
        for ancestor in ancestors {
            let preparing: i64 = tx.query_row(
                "SELECT COUNT(*) FROM project_admissions WHERE project_id=?1",
                [&ancestor.project_id],
                |row| row.get(0),
            )?;
            if preparing > 0 {
                bail!("task transfer project is preparing for lifecycle change");
            }
        }
    }
    for task_id in &operation.task_ids {
        let current: Option<String> = tx
            .query_row(
                "SELECT project_id FROM task_locations WHERE task_id=?1",
                [task_id],
                |row| row.get(0),
            )
            .optional()?;
        if current.as_deref() != Some(&operation.source_project_id) {
            bail!("task location changed before transfer");
        }
        let pending_relation: i64 = tx.query_row(
            "SELECT COUNT(*) FROM task_relation_admissions \
             WHERE source_task_id=?1 OR target_task_id=?1",
            [task_id],
            |row| row.get(0),
        )?;
        if pending_relation != 0 {
            bail!("task has a pending relation mutation");
        }
        let pending_deletion: i64 = tx.query_row(
            "SELECT COUNT(*) FROM task_relation_deletions d JOIN task_relation_routes r \
             ON r.relation_id=d.relation_id WHERE r.source_task_id=?1 OR r.target_task_id=?1",
            [task_id],
            |row| row.get(0),
        )?;
        if pending_deletion != 0 {
            bail!("task has a pending relation deletion");
        }
    }
    require_transfer_readers_on(&tx, operation, &operation.source_project_id)?;
    tx.execute(
        "INSERT INTO task_transfer_journal(operation_id,org_id,source_project_id,\
         destination_project_id,actor_user_id,task_ids_json,consent_wider_access,phase) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,'prepared')",
        params![
            operation.operation_id,
            operation.org_id,
            operation.source_project_id,
            operation.destination_project_id,
            operation.actor_user_id,
            serde_json::to_string(&operation.task_ids)?,
            operation.consent_wider_access
        ],
    )?;
    for task_id in &operation.task_ids {
        tx.execute(
            "INSERT INTO task_transfer_fences(task_id,operation_id,source_project_id) VALUES (?1,?2,?3)",
            params![task_id,operation.operation_id,operation.source_project_id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn mark_task_transfer_copied(
    operation_id: &str,
    sha_manifest_json: &str,
    key_map_json: &str,
    event_map_json: &str,
) -> Result<()> {
    let _: serde_json::Value = serde_json::from_str(sha_manifest_json)?;
    let _: serde_json::Value = serde_json::from_str(key_map_json)?;
    let _: serde_json::Value = serde_json::from_str(event_map_json)?;
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let operation = transfer_on(&tx, operation_id)?;
    if operation.phase == "copied" {
        if operation.sha_manifest_json != sha_manifest_json
            || operation.key_map_json != key_map_json
            || operation.event_map_json != event_map_json
        {
            bail!("task transfer copy manifest changed");
        }
        return Ok(());
    }
    if operation.phase != "prepared" {
        bail!("task transfer is not prepared");
    }
    tx.execute(
        "UPDATE task_transfer_journal SET phase='copied',sha_manifest_json=?1,key_map_json=?2,\
         event_map_json=?3,updated_at=datetime('now') WHERE operation_id=?4",
        params![
            sha_manifest_json,
            key_map_json,
            event_map_json,
            operation_id
        ],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn abort_task_transfer(operation_id: &str) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let operation = tx
        .query_row(
            "SELECT operation_id,org_id,source_project_id,destination_project_id,actor_user_id,\
         task_ids_json,consent_wider_access,phase,sha_manifest_json,key_map_json,event_map_json \
         FROM task_transfer_journal WHERE operation_id=?1",
            [operation_id],
            read_transfer,
        )
        .optional()?;
    let Some(operation) = operation else {
        return Ok(());
    };
    if operation.phase != "prepared" {
        bail!("copied or published task transfer cannot be aborted");
    }
    for task_id in &operation.task_ids {
        tx.execute(
            "DELETE FROM task_index WHERE project_id=?1 AND task_id=?2",
            params![operation.destination_project_id, task_id],
        )?;
    }
    tx.execute(
        "DELETE FROM task_transfer_fences WHERE operation_id=?1",
        [operation_id],
    )?;
    tx.execute(
        "DELETE FROM task_transfer_journal WHERE operation_id=?1",
        [operation_id],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn stage_task_transfer_index(operation_id: &str, snapshots: &[TaskIndexEvent]) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let operation = transfer_on(&tx, operation_id)?;
    if !matches!(operation.phase.as_str(), "prepared" | "copied") {
        bail!("task transfer cannot stage index in this phase");
    }
    let expected: HashSet<&str> = operation.task_ids.iter().map(String::as_str).collect();
    let actual: HashSet<&str> = snapshots
        .iter()
        .map(|snapshot| snapshot.task_id.as_str())
        .collect();
    if expected != actual || snapshots.len() != actual.len() {
        bail!("task transfer staging set differs from journal");
    }
    for snapshot in snapshots {
        if snapshot.op != "upsert" {
            bail!("task transfer staging requires snapshots");
        }
        let current: String = tx.query_row(
            "SELECT project_id FROM task_locations WHERE task_id=?1",
            [&snapshot.task_id],
            |row| row.get(0),
        )?;
        if current != operation.source_project_id {
            bail!("task transfer source changed before staging");
        }
        apply_task_index_event(
            &tx,
            &operation.destination_project_id,
            &operation.org_id,
            snapshot,
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn publish_task_transfer(
    operation_id: &str,
    new_locations: &[TaskLocation],
    aliases: &[TaskKeyAlias],
    event_aliases: &[TaskEventAlias],
    link_aliases: &[TaskLinkAlias],
    relation_routes: &[TaskRelationRoute],
) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let operation = transfer_on(&tx, operation_id)?;
    if operation.phase == "published" || operation.phase == "cleaned" {
        return Ok(());
    }
    if operation.phase != "copied" {
        bail!("task transfer copy is not complete");
    }
    require_transfer_actor_on(&tx, &operation)?;
    let source_path = project_ancestry_on(&tx, &operation.org_id, &operation.source_project_id)?;
    let destination_path =
        project_ancestry_on(&tx, &operation.org_id, &operation.destination_project_id)?;
    for project in source_path.iter().chain(destination_path.iter()) {
        if project.status != "active" || project.lifecycle != "active" {
            bail!("task transfer project became read-only");
        }
        let admitting: i64 = tx.query_row(
            "SELECT COUNT(*) FROM project_admissions WHERE project_id=?1",
            [&project.project_id],
            |row| row.get(0),
        )?;
        if admitting != 0 {
            bail!("task transfer project has lifecycle admission");
        }
    }
    let expected: HashSet<&str> = operation.task_ids.iter().map(String::as_str).collect();
    let actual: HashSet<&str> = new_locations
        .iter()
        .map(|loc| loc.task_id.as_str())
        .collect();
    if expected != actual || new_locations.len() != actual.len() {
        bail!("task transfer location set differs from prepared task set");
    }
    require_transfer_readers_on(&tx, &operation, &operation.destination_project_id)?;
    let mut prior_routes = HashMap::<String, TaskRelationRoute>::new();
    for task_id in &operation.task_ids {
        let mut stmt = tx.prepare(
            "SELECT relation_id,owning_project_id,link_id,source_task_id,target_task_id,\
             source_project_id,target_project_id,kind FROM task_relation_routes \
             WHERE source_task_id=?1 OR target_task_id=?1",
        )?;
        let rows = stmt.query_map([task_id], read_relation_route)?;
        for row in rows {
            let route = row?;
            prior_routes.insert(route.relation_id.clone(), route);
        }
    }
    let supplied_routes: HashSet<&str> = relation_routes
        .iter()
        .map(|route| route.relation_id.as_str())
        .collect();
    let required_routes: HashSet<&str> = prior_routes.keys().map(String::as_str).collect();
    if supplied_routes != required_routes || supplied_routes.len() != relation_routes.len() {
        bail!("task transfer relation set differs from current routes");
    }
    let destination_prefix: String = tx.query_row(
        "SELECT key_prefix FROM projects WHERE org_id=?1 AND project_id=?2",
        params![operation.org_id, operation.destination_project_id],
        |row| row.get(0),
    )?;
    for location in new_locations {
        if location.org_id != operation.org_id
            || location.project_id != operation.destination_project_id
            || location.current_key.is_empty()
        {
            bail!("invalid task transfer destination identity");
        }
        let current: TaskLocation = tx.query_row(
            "SELECT task_id,org_id,project_id,current_key FROM task_locations WHERE task_id=?1",
            [&location.task_id],
            |row| {
                Ok(TaskLocation {
                    task_id: row.get(0)?,
                    org_id: row.get(1)?,
                    project_id: row.get(2)?,
                    current_key: row.get(3)?,
                })
            },
        )?;
        if current.org_id != operation.org_id || current.project_id != operation.source_project_id {
            bail!("task transfer source location changed");
        }
        let staged: Option<(String, u32)> = tx
            .query_row(
                "SELECT task_key,task_no FROM task_index WHERE project_id=?1 AND task_id=?2",
                params![operation.destination_project_id, location.task_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if staged.as_ref().map(|(key, _)| key) != Some(&location.current_key)
            || staged.as_ref().is_none_or(|(_, number)| {
                location.current_key != format!("{destination_prefix}-{number}")
            })
        {
            bail!("task transfer destination index is not staged");
        }
        let reserved: Option<String> = tx
            .query_row(
                "SELECT task_id FROM task_key_aliases WHERE org_id=?1 AND alias_key=?2",
                params![operation.org_id, location.current_key],
                |row| row.get(0),
            )
            .optional()?;
        if reserved.is_some_and(|id| id != location.task_id) {
            bail!("task transfer destination key is reserved");
        }
        tx.execute(
            "UPDATE task_locations SET project_id=?1,current_key=?2 WHERE task_id=?3",
            params![location.project_id, location.current_key, location.task_id],
        )?;
        tx.execute(
            "UPDATE task_key_aliases SET target_project_id=?1,current_key=?2 WHERE task_id=?3",
            params![location.project_id, location.current_key, location.task_id],
        )?;
        tx.execute(
            "INSERT INTO task_key_aliases(org_id,alias_key,task_id,target_project_id,current_key) \
             VALUES (?1,?2,?3,?4,?2) ON CONFLICT(org_id,alias_key) DO UPDATE SET \
             target_project_id=excluded.target_project_id,current_key=excluded.current_key \
             WHERE task_key_aliases.task_id=excluded.task_id",
            params![
                operation.org_id,
                location.current_key,
                location.task_id,
                location.project_id
            ],
        )?;
        tx.execute(
            "INSERT INTO task_key_aliases(org_id,alias_key,task_id,target_project_id,current_key) \
             VALUES (?1,?2,?3,?4,?5) ON CONFLICT(org_id,alias_key) DO UPDATE SET \
             target_project_id=excluded.target_project_id,current_key=excluded.current_key \
             WHERE task_key_aliases.task_id=excluded.task_id",
            params![
                operation.org_id,
                current.current_key,
                location.task_id,
                location.project_id,
                location.current_key
            ],
        )?;
    }
    for alias in aliases {
        if alias.org_id != operation.org_id || !expected.contains(alias.task_id.as_str()) {
            bail!("task key alias outside transfer");
        }
        let location = new_locations
            .iter()
            .find(|loc| loc.task_id == alias.task_id)
            .ok_or_else(|| anyhow!("task alias has no destination"))?;
        if alias.target_project_id != location.project_id
            || alias.current_key != location.current_key
        {
            bail!("task key alias destination mismatch");
        }
        let reserved: Option<String> = tx
            .query_row(
                "SELECT task_id FROM task_key_aliases WHERE org_id=?1 AND alias_key=?2",
                params![alias.org_id, alias.alias_key],
                |row| row.get(0),
            )
            .optional()?;
        if reserved.is_some_and(|id| id != alias.task_id) {
            bail!("task key alias belongs to another task");
        }
        let live_key: Option<String> = tx
            .query_row(
                "SELECT task_id FROM task_locations WHERE org_id=?1 AND current_key=?2",
                params![alias.org_id, alias.alias_key],
                |row| row.get(0),
            )
            .optional()?;
        if live_key.is_some_and(|id| id != alias.task_id) {
            bail!("task key alias conflicts with another current task");
        }
        tx.execute(
            "INSERT INTO task_key_aliases(org_id,alias_key,task_id,target_project_id,current_key) \
             VALUES (?1,?2,?3,?4,?5) ON CONFLICT(org_id,alias_key) DO UPDATE SET \
             target_project_id=excluded.target_project_id,current_key=excluded.current_key \
             WHERE task_key_aliases.task_id=excluded.task_id",
            params![
                alias.org_id,
                alias.alias_key,
                alias.task_id,
                alias.target_project_id,
                alias.current_key
            ],
        )?;
    }
    for route in relation_routes {
        let old = prior_routes
            .get(&route.relation_id)
            .ok_or_else(|| anyhow!("task relation route missing"))?;
        if route.source_task_id != old.source_task_id
            || route.target_task_id != old.target_task_id
            || route.kind != old.kind
            || route.link_id <= 0
            || route.owning_project_id != route.source_project_id
        {
            bail!("task relation identity changed during transfer");
        }
        for (task_id, project_id) in [
            (&route.source_task_id, &route.source_project_id),
            (&route.target_task_id, &route.target_project_id),
        ] {
            let current: String = tx.query_row(
                "SELECT project_id FROM task_locations WHERE task_id=?1",
                [task_id],
                |row| row.get(0),
            )?;
            if current != project_id.as_str() {
                bail!("task relation endpoint location mismatch");
            }
        }
        tx.execute(
            "UPDATE task_relation_routes SET owning_project_id=?1,link_id=?2,\
             source_project_id=?3,target_project_id=?4 WHERE relation_id=?5",
            params![
                route.owning_project_id,
                route.link_id,
                route.source_project_id,
                route.target_project_id,
                route.relation_id
            ],
        )?;
        tx.execute(
            "UPDATE task_link_aliases SET current_project_id=?1,current_link_id=?2 WHERE relation_id=?3",
            params![route.owning_project_id,route.link_id,route.relation_id],
        )?;
    }
    for alias in event_aliases {
        if !expected.contains(alias.task_id.as_str())
            || alias.current_project_id != operation.destination_project_id
        {
            bail!("task event alias outside transfer");
        }
        tx.execute(
            "INSERT INTO task_event_aliases(task_id,origin_project_id,origin_event_id,\
             current_project_id,current_event_id) VALUES (?1,?2,?3,?4,?5) \
             ON CONFLICT(task_id,origin_project_id,origin_event_id) DO UPDATE SET \
             current_project_id=excluded.current_project_id,current_event_id=excluded.current_event_id",
            params![alias.task_id,alias.origin_project_id,alias.origin_event_id,
                    alias.current_project_id,alias.current_event_id],
        )?;
        tx.execute(
            "UPDATE task_event_aliases SET current_project_id=?1,current_event_id=?2 \
             WHERE task_id=?3 AND current_project_id=?4 AND current_event_id=?5",
            params![
                alias.current_project_id,
                alias.current_event_id,
                alias.task_id,
                alias.origin_project_id,
                alias.origin_event_id
            ],
        )?;
    }
    for alias in link_aliases {
        let route = relation_routes
            .iter()
            .find(|route| route.relation_id == alias.relation_id)
            .ok_or_else(|| anyhow!("task link alias outside transfer"))?;
        if alias.current_project_id != route.owning_project_id
            || alias.current_link_id != route.link_id
        {
            bail!("task link alias destination mismatch");
        }
        tx.execute(
            "INSERT INTO task_link_aliases(relation_id,origin_project_id,origin_link_id,\
             current_project_id,current_link_id) VALUES (?1,?2,?3,?4,?5) \
             ON CONFLICT(origin_project_id,origin_link_id) DO UPDATE SET \
             relation_id=excluded.relation_id,current_project_id=excluded.current_project_id,\
             current_link_id=excluded.current_link_id",
            params![
                alias.relation_id,
                alias.origin_project_id,
                alias.origin_link_id,
                alias.current_project_id,
                alias.current_link_id
            ],
        )?;
        tx.execute(
            "UPDATE task_link_aliases SET current_project_id=?1,current_link_id=?2 \
             WHERE relation_id=?3 AND current_project_id=?4 AND current_link_id=?5",
            params![
                alias.current_project_id,
                alias.current_link_id,
                alias.relation_id,
                alias.origin_project_id,
                alias.origin_link_id
            ],
        )?;
    }
    tx.execute(
        "UPDATE task_transfer_journal SET phase='published',updated_at=datetime('now') WHERE operation_id=?1",
        [operation_id],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn mark_task_transfer_cleaned(operation_id: &str) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let operation = transfer_on(&tx, operation_id)?;
    if operation.phase == "cleaned" {
        return Ok(());
    }
    if operation.phase != "published" {
        bail!("task transfer is not published");
    }
    tx.execute(
        "DELETE FROM task_transfer_fences WHERE operation_id=?1",
        [operation_id],
    )?;
    tx.execute(
        "UPDATE task_transfer_journal SET phase='cleaned',updated_at=datetime('now') WHERE operation_id=?1",
        [operation_id],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn pending_task_transfers() -> Result<Vec<TaskTransferJournal>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(
        "SELECT operation_id,org_id,source_project_id,destination_project_id,actor_user_id,\
         task_ids_json,consent_wider_access,phase,sha_manifest_json,key_map_json,event_map_json \
         FROM task_transfer_journal WHERE phase<>'cleaned' ORDER BY created_at,operation_id",
    )?;
    let rows = stmt.query_map([], read_transfer)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn plan_import_nodes(
    conn: &rusqlite::Connection,
    org_id: &str,
    nodes: &[ProjectImportNode],
) -> Result<Vec<(usize, String, u32)>> {
    if nodes.is_empty() {
        bail!("project import tree is empty");
    }
    let ids: HashSet<&str> = nodes.iter().map(|node| node.project_id.as_str()).collect();
    if ids.len() != nodes.len() {
        bail!("duplicate imported project id");
    }
    let mut prefixes = HashSet::new();
    for node in nodes {
        if node.project_id.is_empty()
            || node.name.trim().is_empty()
            || node.owner_user_id.is_empty()
            || node.dir_path.is_empty()
            || !super::VALID_TEMPLATES.contains(&node.template.as_str())
            || !matches!(node.status.as_str(), "active" | "archived")
            || !matches!(node.lifecycle.as_str(), "active" | "ended")
            || (node.parent_id.is_none() && (node.inherit_modules || node.inherit_task_types))
        {
            bail!("invalid imported project metadata");
        }
        let prefix = super::db::normalize_key_prefix(&node.key_prefix)
            .ok_or_else(|| anyhow!("invalid imported project key prefix"))?;
        if prefix != node.key_prefix || !prefixes.insert(prefix) {
            bail!("duplicate or noncanonical imported key prefix");
        }
        let _: Vec<String> = serde_json::from_str(&node.modules_json)?;
        let existing: i64 = conn.query_row(
            "SELECT COUNT(*) FROM projects WHERE project_id=?1",
            [&node.project_id],
            |row| row.get(0),
        )?;
        if existing != 0 {
            bail!("imported project id already exists");
        }
        validate_function_catalogue(&node.functions)?;
        for member in &node.members {
            super::models::validate_member_input(member, &node.functions, chrono::Utc::now())?;
        }
    }
    let mut planned = Vec::<(usize, String, u32)>::new();
    let mut pending: HashSet<usize> = (0..nodes.len()).collect();
    while !pending.is_empty() {
        let old_len = pending.len();
        for index in pending.clone() {
            let node = &nodes[index];
            let parent = match &node.parent_id {
                None => Some((String::new(),0u32,"active".to_string(),"active".to_string())),
                Some(parent_id) if ids.contains(parent_id.as_str()) => {
                    planned.iter().find(|(i,_,_)| nodes[*i].project_id == *parent_id)
                        .map(|(_,path,depth)| {
                            let parent = nodes.iter().find(|candidate| candidate.project_id == *parent_id)
                                .expect("import parent is present");
                            (path.clone(),*depth,parent.status.clone(),parent.lifecycle.clone())
                        })
                }
                Some(parent_id) => conn.query_row(
                    "SELECT path,depth,status,lifecycle FROM projects WHERE org_id=?1 AND project_id=?2",
                    params![org_id,parent_id],
                    |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
                ).optional()?.map(|parent| -> Result<_> {
                    let preparing: i64 = conn.query_row(
                        "SELECT COUNT(*) FROM project_admissions WHERE project_id=?1",
                        [parent_id],|row| row.get(0),
                    )?;
                    if preparing != 0 { bail!("project import parent has a lifecycle admission"); }
                    Ok(parent)
                }).transpose()?,
            };
            let Some((parent_path, parent_depth, parent_status, parent_lifecycle)) = parent else {
                continue;
            };
            if parent_depth >= 4 {
                bail!("project import exceeds four tree levels");
            }
            if node.status == "active"
                && node.lifecycle == "active"
                && (parent_status != "active" || parent_lifecycle != "active")
            {
                bail!("active imported child requires active parent");
            }
            let path = format!("{parent_path}/{}", node.project_id);
            planned.push((index, path, parent_depth + 1));
            pending.remove(&index);
        }
        if pending.len() == old_len {
            bail!("imported project tree has missing parent or cycle");
        }
    }
    Ok(planned)
}

pub fn prepare_project_tree_import(
    operation_id: &str,
    org_id: &str,
    manifest_sha256: &str,
    nodes: &[ProjectImportNode],
) -> Result<Vec<ProjectImportNode>> {
    if operation_id.is_empty()
        || manifest_sha256.len() != 64
        || !manifest_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("invalid project import identity");
    }
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let ids: HashSet<&str> = nodes.iter().map(|node| node.project_id.as_str()).collect();
    if ids.len() != nodes.len() {
        bail!("duplicate imported project id");
    }
    let mut resolved = Vec::with_capacity(nodes.len());
    for input in nodes {
        let mut node = input.clone();
        let base: String = node.name.trim().chars().take(200).collect();
        if base.is_empty() {
            bail!("imported project name is required");
        }
        let mut chosen_name = None;
        for number in 1..=1_000_000 {
            let suffix = if number == 1 {
                String::new()
            } else {
                format!(" ({number})")
            };
            let stem: String = base.chars().take(200 - suffix.chars().count()).collect();
            let candidate = format!("{stem}{suffix}");
            let taken: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM projects WHERE org_id=?1 AND parent_id IS ?2 AND name=?3 \
                 UNION ALL SELECT 1 FROM project_import_reservations \
                 WHERE org_id=?1 AND parent_id IS ?2 AND name=?3)",
                params![org_id,node.parent_id,candidate],|row| row.get(0),
            )?;
            if !taken {
                chosen_name = Some(candidate);
                break;
            }
        }
        node.name = chosen_name.ok_or_else(|| anyhow!("project import name space exhausted"))?;
        let requested = node.key_prefix.trim();
        if !requested.is_empty() && super::db::normalize_key_prefix(requested).is_none() {
            bail!("invalid imported project key prefix");
        }
        let requested = requested.to_ascii_uppercase();
        let prefix_taken: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM project_prefix_reservations \
             WHERE org_id=?1 AND key_prefix=?2 UNION ALL SELECT 1 FROM project_import_reservations \
             WHERE org_id=?1 AND key_prefix=?2)",
            params![org_id, requested],
            |row| row.get(0),
        )?;
        node.key_prefix = super::db::reserve_key_prefix(
            &tx,
            org_id,
            &node.name,
            if prefix_taken { "" } else { &requested },
        )?;
        tx.execute(
            "INSERT INTO project_import_reservations(operation_id,org_id,project_id,\
             key_prefix,name,parent_id) VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                operation_id,
                org_id,
                node.project_id,
                node.key_prefix,
                node.name,
                node.parent_id
            ],
        )?;
        resolved.push(node);
    }
    let _ = plan_import_nodes(&tx, org_id, &resolved)?;
    tx.execute(
        "INSERT INTO project_import_journal(operation_id,org_id,manifest_sha256,nodes_json,phase) \
         VALUES (?1,?2,?3,?4,'prepared')",
        params![
            operation_id,
            org_id,
            manifest_sha256,
            serde_json::to_string(&resolved)?
        ],
    )?;
    tx.commit()?;
    Ok(resolved)
}

pub fn publish_project_tree_import(
    operation_id: &str,
    org_id: &str,
    manifest_sha256: &str,
    nodes: &[ProjectImportNode],
) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let journal: (String,String,String,String) = tx.query_row(
        "SELECT org_id,manifest_sha256,phase,nodes_json FROM project_import_journal WHERE operation_id=?1",
        [operation_id],|row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
    )?;
    if journal.0 != org_id || journal.1 != manifest_sha256 {
        bail!("project import journal identity changed");
    }
    if journal.2 == "published" {
        return Ok(());
    }
    if journal.2 != "prepared" {
        bail!("project import is not prepared");
    }
    let durable_nodes: Vec<ProjectImportNode> = serde_json::from_str(&journal.3)?;
    if durable_nodes != nodes {
        bail!("project import nodes differ from durable journal");
    }
    let reservations: Vec<(String, String, String, Option<String>)> = {
        let mut stmt = tx.prepare(
            "SELECT project_id,key_prefix,name,parent_id FROM project_import_reservations \
             WHERE operation_id=?1 ORDER BY project_id",
        )?;
        let rows = stmt.query_map([operation_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        let reservations = rows.collect::<rusqlite::Result<_>>()?;
        reservations
    };
    if reservations.len() != nodes.len() {
        bail!("project import reservation count changed");
    }
    for node in nodes {
        if !reservations.iter().any(|(id, prefix, name, parent)| {
            id == &node.project_id
                && prefix == &node.key_prefix
                && name == &node.name
                && parent == &node.parent_id
        }) {
            bail!("project import node differs from durable reservation");
        }
    }
    let planned = plan_import_nodes(&tx, org_id, nodes)?;
    for (index, path, depth) in planned {
        let node = &nodes[index];
        let disabled_modules: Vec<String> = if node.inherit_modules {
            let parent_id = node
                .parent_id
                .as_ref()
                .ok_or_else(|| anyhow!("inherited import node has no parent"))?;
            let parent = project_ancestry_on(&tx, org_id, parent_id)?;
            let available = effective_modules_on(&parent)?;
            let selected: Vec<String> = serde_json::from_str(&node.modules_json)?;
            if selected.iter().any(|module| !available.contains(module)) {
                bail!("inherited import module is unavailable in parent");
            }
            available
                .into_iter()
                .filter(|module| !selected.contains(module))
                .collect()
        } else {
            Vec::new()
        };
        tx.execute(
            "INSERT INTO projects(project_id,org_id,name,description,status,template,modules_json,\
             owner_user_id,dir_path,key_prefix,parent_id,path,depth,is_private,inherit_modules,\
             inherit_task_types,module_disabled_json,lifecycle,ended_at) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,\
             CASE WHEN ?18='ended' THEN datetime('now') ELSE NULL END)",
            params![
                node.project_id,
                org_id,
                node.name,
                node.description,
                node.status,
                node.template,
                node.modules_json,
                node.owner_user_id,
                node.dir_path,
                node.key_prefix,
                node.parent_id,
                path,
                depth,
                node.is_private,
                node.inherit_modules,
                node.inherit_task_types,
                serde_json::to_string(&disabled_modules)?,
                node.lifecycle
            ],
        )?;
        tx.execute(
            "INSERT INTO project_prefix_reservations(org_id,key_prefix,project_id) VALUES (?1,?2,?3)",
            params![org_id,node.key_prefix,node.project_id],
        )?;
        for (position, function) in node.functions.iter().enumerate() {
            tx.execute(
                "INSERT INTO project_functions(project_id,function_id,name,description,builtin,grants_json,position) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![node.project_id,function.function_id,function.name,function.description,
                        function.builtin,serde_json::to_string(&function.grants)?,position as i64],
            )?;
        }
        tx.execute(
            "INSERT INTO project_members(project_id,user_id,project_admin,invited_by) VALUES (?1,?2,1,?2)",
            params![node.project_id,node.owner_user_id],
        )?;
        for function in &node.functions {
            tx.execute(
                "INSERT INTO project_member_functions(project_id,user_id,function_id) VALUES (?1,?2,?3)",
                params![node.project_id,node.owner_user_id,function.function_id],
            )?;
        }
        for member in &node.members {
            if member.user_id != node.owner_user_id {
                insert_member(&tx, &node.project_id, member, &node.owner_user_id)?;
            }
        }
    }
    tx.execute(
        "UPDATE project_import_journal SET phase='published',updated_at=datetime('now') \
         WHERE operation_id=?1",
        [operation_id],
    )?;
    tx.execute(
        "DELETE FROM project_import_reservations WHERE operation_id=?1",
        [operation_id],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn abort_project_tree_import(operation_id: &str) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let phase: Option<String> = tx
        .query_row(
            "SELECT phase FROM project_import_journal WHERE operation_id=?1",
            [operation_id],
            |row| row.get(0),
        )
        .optional()?;
    if phase.as_deref() == Some("published") {
        bail!("published project import cannot be aborted");
    }
    tx.execute(
        "DELETE FROM project_import_reservations WHERE operation_id=?1",
        [operation_id],
    )?;
    tx.execute(
        "DELETE FROM project_import_journal WHERE operation_id=?1",
        [operation_id],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn pending_project_tree_imports() -> Result<Vec<ProjectImportJournal>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare(
        "SELECT operation_id,org_id,manifest_sha256,nodes_json FROM project_import_journal \
         WHERE phase='prepared' ORDER BY created_at,operation_id",
    )?;
    let rows = stmt.query_map([], |row| {
        let nodes_json: String = row.get(3)?;
        let nodes = serde_json::from_str(&nodes_json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?;
        Ok(ProjectImportJournal {
            operation_id: row.get(0)?,
            org_id: row.get(1)?,
            manifest_sha256: row.get(2)?,
            nodes,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

pub fn prepare_project_end(
    org_id: &str,
    project_id: &str,
    actor_user_id: &str,
    reason: &str,
) -> Result<String> {
    if reason.trim().is_empty() {
        bail!("project end reason is required");
    }
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let ancestors = project_ancestry_on(&tx, org_id, project_id)?;
    if ancestors
        .iter()
        .any(|project| project.status != "active" || project.lifecycle != "active")
    {
        bail!("project is not active");
    }
    for project in &ancestors {
        let preparing: i64 = tx.query_row(
            "SELECT COUNT(*) FROM project_admissions WHERE project_id=?1",
            [&project.project_id],
            |row| row.get(0),
        )?;
        if preparing > 0 {
            bail!("project or ancestor is preparing to end");
        }
    }
    let active_children: i64 = tx.query_row(
        "SELECT COUNT(*) FROM projects WHERE parent_id=?1 AND status='active' AND lifecycle='active'",
        [project_id], |row| row.get(0),
    )?;
    if active_children > 0 {
        bail!("active child projects must be moved or ended first");
    }
    let transferring: i64 = tx.query_row(
        "SELECT COUNT(*) FROM task_transfer_journal WHERE phase<>'cleaned' \
         AND (source_project_id=?1 OR destination_project_id=?1)",
        [project_id],
        |row| row.get(0),
    )?;
    if transferring != 0 {
        bail!("project has an unfinished task transfer");
    }
    let operation_id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO project_admissions(project_id,operation_id,kind,actor_user_id,reason) \
         VALUES (?1,?2,'end',?3,?4)",
        params![project_id, operation_id, actor_user_id, reason.trim()],
    )?;
    tx.commit()?;
    Ok(operation_id)
}

pub fn finalize_project_end(
    org_id: &str,
    project_id: &str,
    operation_id: &str,
    content_pool: &DbPool,
) -> Result<bool> {
    let content_conn = content_pool.write().map_err(write_err)?;
    let content_tx = content_conn.unchecked_transaction()?;
    content_tx.execute(
        "UPDATE settings SET value=value WHERE key='project_key_prefix'",
        [],
    )?;
    let open_tasks: i64 = content_tx.query_row(
        "SELECT COUNT(*) FROM tasks WHERE status<>'done' AND archived_at IS NULL",
        [],
        |row| row.get(0),
    )?;
    if open_tasks > 0 {
        bail!("open tasks must be transferred or resolved before ending project");
    }
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let admission: Option<(String,String)> = tx.query_row(
        "SELECT actor_user_id,reason FROM project_admissions WHERE project_id=?1 AND operation_id=?2 AND kind='end'",
        params![project_id,operation_id], |row| Ok((row.get(0)?,row.get(1)?)),
    ).optional()?;
    let Some((actor_user_id, reason)) = admission else {
        bail!("project end admission not found");
    };
    let active_children: i64 = tx.query_row(
        "SELECT COUNT(*) FROM projects WHERE parent_id=?1 AND status='active' AND lifecycle='active'",
        [project_id], |row| row.get(0),
    )?;
    if active_children > 0 {
        bail!("active child projects block ending project");
    }
    let changed = tx.execute(
        "UPDATE projects SET lifecycle='ended',ended_at=datetime('now'),updated_at=datetime('now') \
         WHERE org_id=?1 AND project_id=?2 AND status='active' AND lifecycle='active'",
        params![org_id,project_id],
    )?;
    if changed != 1 {
        bail!("project is not active");
    }
    tx.execute(
        "INSERT INTO project_lifecycle_events(project_id,operation_id,actor_user_id,kind,reason) \
         VALUES (?1,?2,?3,'ended',?4)",
        params![project_id, operation_id, actor_user_id, reason],
    )?;
    tx.execute(
        "DELETE FROM project_admissions WHERE project_id=?1",
        [project_id],
    )?;
    tx.commit()?;
    content_tx.commit()?;
    drop(conn);
    drop(content_conn);
    reconcile_project_mirrors(project_id);
    Ok(true)
}

pub fn abort_project_end(org_id: &str, project_id: &str, operation_id: &str) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "DELETE FROM project_admissions WHERE project_id=?1 AND operation_id=?2 \
         AND EXISTS(SELECT 1 FROM projects WHERE project_id=?1 AND org_id=?3 AND lifecycle='active')",
        params![project_id,operation_id,org_id],
    )?;
    Ok(())
}

pub fn resume_project(
    org_id: &str,
    project_id: &str,
    actor_user_id: &str,
    reason: &str,
) -> Result<bool> {
    if reason.trim().is_empty() {
        bail!("project resume reason is required");
    }
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    let ancestors = project_ancestry_on(&tx, org_id, project_id)?;
    let project = ancestors
        .last()
        .ok_or_else(|| anyhow!("project not found"))?;
    if project.lifecycle != "ended" {
        return Ok(false);
    }
    if ancestors[..ancestors.len() - 1]
        .iter()
        .any(|node| node.lifecycle != "active" || node.status != "active")
    {
        bail!("ancestor project must be active before resume");
    }
    let changed = tx.execute(
        "UPDATE projects SET lifecycle='active',ended_at=NULL,updated_at=datetime('now') \
         WHERE org_id=?1 AND project_id=?2 AND lifecycle='ended'",
        params![org_id, project_id],
    )?;
    if changed == 1 {
        tx.execute(
            "INSERT INTO project_lifecycle_events(project_id,operation_id,actor_user_id,kind,reason) \
             VALUES (?1,?2,?3,'resumed',?4)",
            params![project_id,uuid::Uuid::new_v4().to_string(),actor_user_id,reason.trim()],
        )?;
    }
    tx.commit()?;
    drop(conn);
    if changed == 1 {
        reconcile_project_mirrors(project_id);
    }
    Ok(changed == 1)
}

pub fn replace_ml_grants(
    project_id: &str,
    link_id: &str,
    ml_project_id: &str,
    user_ids: &[String],
) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM project_ml_grants WHERE project_id=?1 AND link_id=?2",
        params![project_id, link_id],
    )?;
    for user_id in user_ids {
        tx.execute(
            "INSERT OR IGNORE INTO project_ml_grants(ml_project_id,user_id,project_id,link_id) VALUES(?1,?2,?3,?4)",
            params![ml_project_id,user_id,project_id,link_id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn ml_grant_origins(ml_project_id: &str, user_id: &str) -> Result<Vec<(String, String)>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(read_err)?;
    let mut stmt = conn.prepare("SELECT project_id,link_id FROM project_ml_grants WHERE ml_project_id=?1 AND user_id=?2 ORDER BY project_id,link_id")?;
    let rows = stmt.query_map(params![ml_project_id, user_id], |row| {
        Ok((row.get(0)?, row.get(1)?))
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

pub fn remove_ml_grant(ml_project_id: &str, user_id: &str) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "DELETE FROM project_ml_grants WHERE ml_project_id=?1 AND user_id=?2",
        params![ml_project_id, user_id],
    )?;
    Ok(())
}

pub fn clear_ml_grants(project_id: &str, link_id: &str) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "DELETE FROM project_ml_grants WHERE project_id=?1 AND link_id=?2",
        params![project_id, link_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    fn pool() -> DbPool {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("proj");
        std::fs::create_dir_all(&dir).expect("dir");
        let (pool, _) = super::super::project_db::open_pool_at(&dir).expect("open");
        std::mem::forget(tmp);
        pool
    }

    fn file(path: &str, sha: &str) -> super::super::ingest::CollectedFile {
        super::super::ingest::CollectedFile {
            rel_path: path.to_string(),
            sha256: sha.to_string(),
            size_bytes: 10,
            mime: "text/plain".to_string(),
        }
    }

    /// (c) A git refresh re-embeds ONLY the delta: an unchanged file keeps its
    /// row (and its file_id, so its vectors are reused), a changed one is
    /// returned for re-ingest, a new one is inserted and a vanished one is
    /// deleted with its id handed back for vector cleanup.
    #[test]
    fn sync_tree_files_returns_only_the_delta() {
        let pool = pool();
        create_source(&pool, "s1", "git", "repo", "{}", "u1").expect("source");

        let initial = vec![file("a.rs", "aaa"), file("b.rs", "bbb")];
        let full = super::super::ingest::TreeDelta {
            added: vec!["a.rs".to_string(), "b.rs".to_string()],
            ..Default::default()
        };
        let (work, removed) = sync_tree_files(&pool, "s1", &initial, &full).expect("initial sync");
        assert_eq!(work.len(), 2);
        assert!(removed.is_empty());
        let stored = super::super::git_source::stored_file_hashes(&pool, "s1").expect("hashes");
        assert_eq!(stored.get("a.rs").map(String::as_str), Some("aaa"));

        // b.rs changed, c.rs is new, a.rs is untouched.
        let current = vec![
            file("a.rs", "aaa"),
            file("b.rs", "BBB2"),
            file("c.rs", "ccc"),
        ];
        let delta = super::super::ingest::diff_tree(&stored, &current);
        assert_eq!(delta.added, vec!["c.rs".to_string()]);
        assert_eq!(delta.changed, vec!["b.rs".to_string()]);
        assert!(delta.removed.is_empty());

        let unchanged_id = {
            let conn = pool.read().expect("read");
            conn.query_row(
                "SELECT file_id FROM source_files WHERE source_id = 's1' AND path = 'a.rs'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("a.rs id")
        };
        let (work, removed) = sync_tree_files(&pool, "s1", &current, &delta).expect("delta sync");
        assert_eq!(
            work.len(),
            2,
            "only the changed + added files are re-ingested"
        );
        assert!(removed.is_empty());
        assert!(
            !work.contains(&unchanged_id),
            "an unchanged file must not be re-embedded"
        );

        // Now b.rs disappears from the tree.
        let stored = super::super::git_source::stored_file_hashes(&pool, "s1").expect("hashes");
        let current = vec![file("a.rs", "aaa"), file("c.rs", "ccc")];
        let delta = super::super::ingest::diff_tree(&stored, &current);
        assert_eq!(delta.removed, vec!["b.rs".to_string()]);
        let (work, removed) = sync_tree_files(&pool, "s1", &current, &delta).expect("removal sync");
        assert!(
            work.is_empty(),
            "nothing changed, so nothing is re-embedded"
        );
        assert_eq!(
            removed.len(),
            1,
            "the vanished file id feeds vector cleanup"
        );
        let stored = super::super::git_source::stored_file_hashes(&pool, "s1").expect("hashes");
        assert_eq!(stored.len(), 2);
        assert!(!stored.contains_key("b.rs"));
    }
}

#[cfg(test)]
mod key_prefix_tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn concurrent_uncached_open_prefix_edit_and_first_task_agree() {
        let registry_dir = tempfile::tempdir().expect("registry directory");
        let _ = super::super::db::init(&registry_dir.path().join("projects.db"));
        std::mem::forget(registry_dir);

        let project_dir = tempfile::tempdir().expect("project directory");
        let project_id = uuid::Uuid::new_v4().to_string();
        let org_id = format!("org-{}", uuid::Uuid::new_v4());
        create_project(
            &project_id,
            &org_id,
            &project_id,
            "",
            "custom",
            "[\"tasks\"]",
            "owner",
            project_dir.path().to_str().expect("path"),
            "OLD",
            None,
            false,
            false,
            false,
            &[],
        )
        .expect("project");
        let (seed_pool, _) =
            super::super::project_db::open_pool_at(project_dir.path()).expect("schema");
        drop(seed_pool);
        let barrier = Arc::new(Barrier::new(3));
        let open_id = project_id.clone();
        let open_barrier = barrier.clone();
        let opener = std::thread::spawn(move || {
            open_barrier.wait();
            super::super::project_db::open(&open_id).expect("uncached open");
        });
        let edit_id = project_id.clone();
        let edit_org = org_id.clone();
        let edit_barrier = barrier.clone();
        let editor = std::thread::spawn(move || {
            edit_barrier.wait();
            update_project_name_desc(&edit_org, &edit_id, &edit_id, "", Some("NEW"))
        });
        let task_id = project_id.clone();
        let task_barrier = barrier.clone();
        let creator = std::thread::spawn(move || {
            task_barrier.wait();
            let pool = super::super::project_db::open(&task_id).expect("project pool");
            super::super::tasks::create_task(
                &pool,
                &super::super::tasks::TaskInput {
                    task_type: "feature",
                    title: "First task",
                    description_md: "",
                    severity: "",
                    priority: "medium",
                    status: "todo",
                    assigned_to: "",
                    due_date: "",
                    parent_task_id: None,
                    links_json: "[]",
                    attachments_json: "[]",
                },
                "owner",
            )
            .expect("task")
        });
        opener.join().expect("open thread");
        let edit = editor.join().expect("edit thread");
        let task = creator.join().expect("task thread");
        let central = get_project(&org_id, &project_id)
            .expect("project query")
            .expect("project row");
        let pool = super::super::project_db::open(&project_id).expect("project pool");
        let local_prefix: String = pool
            .read()
            .expect("read")
            .query_row(
                "SELECT value FROM settings WHERE key = 'project_key_prefix'",
                [],
                |row| row.get(0),
            )
            .expect("local prefix");
        assert_eq!(central.key_prefix, local_prefix);
        assert_eq!(task.task_key, format!("{}-1", central.key_prefix));
        assert_eq!(edit.is_ok(), central.key_prefix == "NEW");
        super::super::project_db::close(&project_id);
    }

    #[test]
    fn task_kpis_count_all_non_defect_types_and_exclude_done_or_archived() {
        let project_dir = tempfile::tempdir().expect("project directory");
        let (pool, _) =
            super::super::project_db::open_pool_at(project_dir.path()).expect("project schema");
        super::super::tasks::save_task_type(
            &pool,
            "incident",
            "Incident",
            "Response work",
            70,
            true,
        )
        .expect("custom type");
        let create = |task_type: &str, title: &str, status: &str| {
            super::super::tasks::create_task(
                &pool,
                &super::super::tasks::TaskInput {
                    task_type,
                    title,
                    description_md: "",
                    severity: if task_type == "defect" { "high" } else { "" },
                    priority: "medium",
                    status,
                    assigned_to: "",
                    due_date: "",
                    parent_task_id: None,
                    links_json: "[]",
                    attachments_json: "[]",
                },
                "owner",
            )
            .expect("task")
        };
        create("technical", "Technical", "todo");
        create("feature", "Feature", "review");
        create("incident", "Incident", "in_progress");
        create("defect", "Open defect", "todo");
        create("defect", "Done defect", "done");
        let archived = create("feature", "Archived feature", "todo");
        super::super::tasks::set_task_archived(&pool, &archived.task_id, true, "owner")
            .expect("archive")
            .expect("task");
        let kpis = project_f2_kpis(&pool, "owner").expect("KPI query");
        assert_eq!(kpis.tasks_open, 3);
        assert_eq!(kpis.defects_open, 1);
    }
}

#[cfg(test)]
mod code_studio_mirror_tests {
    use super::*;
    use crate::code_studio::models::{
        AutonomyMode, EgressEnforcement, ExecMode, NewWorkspace, WorkspaceRole, WorkspaceStatus,
    };
    use crate::code_studio::project_link;
    use crate::code_studio::repository as code_repo;

    const ORG: &str = "org-code-mirror";

    /// The mirror spans two databases: Project Studio's registry holds the
    /// members, the core one holds the workspaces, the links and the mirrored
    /// memberships. Both are process-wide `OnceLock`s, so every test works on
    /// freshly generated ids instead of a private database.
    fn core_registry() -> DbPool {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _ = super::super::db::init(&tmp.path().join("projects.db"));
        // The registry pool outlives this call, so the directory must not be
        // reclaimed with the handle.
        std::mem::forget(tmp);
        let state = crate::dispatch::state::AppState::for_test();
        drop(state);
        crate::db::global_pool().expect("core pool initialised by AppState::for_test")
    }

    fn workspace(db: &DbPool, id: &str) {
        code_repo::create_workspace(
            db,
            &NewWorkspace {
                id: id.to_string(),
                org_id: ORG.into(),
                owner_user_id: "u-ws-owner".into(),
                name: id.to_string(),
                slug: id.to_string(),
                node_id: "node-1".into(),
                exec_mode: ExecMode::TrustedNative,
                container_image: None,
                egress_enforcement: EgressEnforcement::Unrestricted,
                repo_kind: "git".into(),
                repo_url: None,
                repo_auth_kind: None,
                secret_ref: None,
                ssh_host_fingerprint: None,
                default_branch: Some("main".into()),
                target_branch: None,
                autonomy_ceiling: AutonomyMode::Normal,
                egress_policy: "org_approved".into(),
                index_enabled: false,
                quota_disk_bytes: None,
                quota_sessions: None,
            },
        )
        .expect("create workspace");
        code_repo::set_status(db, id, WorkspaceStatus::Active, None).expect("activate workspace");
    }

    /// Workspace role of `user_id`, but ONLY when the row carries this
    /// project's mirror stamp — a manual grant reads as `None` here, which is
    /// exactly what "the mirror owns this row" has to mean.
    fn mirrored(
        db: &DbPool,
        workspace_id: &str,
        project_id: &str,
        user_id: &str,
    ) -> Option<String> {
        let origin = project_link::mirror_origin(project_id);
        code_repo::list_members(db, workspace_id)
            .expect("workspace members")
            .into_iter()
            .find(|member| member.user_id == user_id && member.added_by == origin)
            .map(|member| member.role)
    }

    fn project_with_owner(project_id: &str, owner: &str) {
        create_project(
            project_id,
            ORG,
            &format!("Projekt {project_id}"),
            "",
            "tests",
            "[\"knowledge\"]",
            owner,
            "/tmp/none",
            "",
            None,
            false,
            false,
            false,
            &[],
        )
        .expect("create project");
    }

    #[test]
    fn function_catalogue_and_membership_mutations_change_real_access() {
        use tentaflow_protocol::project_studio::access::{
            ProjectArea, ProjectPermissionLevel as Level,
        };
        let _guard = crate::code_studio::paths::test_data_dir_guard();
        let _core = core_registry();
        let project_id = format!("access-{}", uuid::Uuid::new_v4());
        let owner = format!("owner-{}", uuid::Uuid::new_v4());
        let user = format!("member-{}", uuid::Uuid::new_v4());
        create_project(
            &project_id,
            ORG,
            &project_id,
            "",
            "custom",
            "[\"knowledge\",\"tests\",\"tasks\"]",
            &owner,
            "unused",
            "",
            None,
            false,
            false,
            false,
            &[],
        )
        .expect("project");
        let project = get_project(ORG, &project_id).expect("get").expect("row");
        add_members(
            &project_id,
            &[MemberInput {
                user_id: user.clone(),
                functions: vec![],
                project_admin: false,
                expires_at: None,
            }],
            &owner,
        )
        .expect("member");
        let initial = project_access(&project, &user, false).expect("access");
        assert!(initial.has_access && initial.can_create_tasks);
        assert!(!initial.allows(ProjectArea::Tests, Level::Read));
        let mut custom = list_functions(&project_id)
            .expect("catalogue")
            .into_iter()
            .find(|entry| entry.function_id == "tester")
            .expect("tester");
        custom.function_id = "reviewer".to_string();
        custom.name = "Reviewer".to_string();
        custom.builtin = false;
        custom
            .grants
            .iter_mut()
            .find(|entry| entry.area == ProjectArea::Tests)
            .expect("tests")
            .level = Level::Write;
        save_function(&project_id, &custom).expect("function");
        set_member_access(&project_id, &user, &["reviewer".to_string()], false, None)
            .expect("assign");
        assert!(project_access(&project, &user, false)
            .expect("access")
            .allows(ProjectArea::Tests, Level::Write));
        assert!(
            set_member_access(&project_id, &user, &["unknown".to_string()], false, None).is_err()
        );
        assert!(delete_function(&project_id, "tester").is_err());
        custom
            .grants
            .iter_mut()
            .find(|entry| entry.area == ProjectArea::Tests)
            .expect("tests")
            .level = Level::Read;
        save_function(&project_id, &custom).expect("change matrix");
        assert!(!project_access(&project, &user, false)
            .expect("access")
            .allows(ProjectArea::Tests, Level::Write));
        delete_function(&project_id, "reviewer").expect("delete function");
        assert!(member_access(&project_id, &user)
            .expect("member")
            .expect("row")
            .functions
            .is_empty());
        set_member_access(&project_id, &owner, &["observer".to_string()], false, None)
            .expect("ownership independent");
        assert!(
            !member_access(&project_id, &owner)
                .expect("owner membership")
                .expect("owner row")
                .project_admin,
            "owner's stored local grant remains separate from ownership"
        );
        let owned = project_access(&project, &owner, false).expect("owner access");
        assert!(owned.is_owner && owned.project_admin);
        assert!(remove_member(&project_id, &owner).is_err());
        assert!(set_member_access(
            &project_id,
            &owner,
            &[],
            false,
            Some("2099-01-01T00:00:00Z")
        )
        .is_err());
        set_member_access(
            &project_id,
            &user,
            &["tester".to_string()],
            true,
            Some("2099-01-01T00:00:00Z"),
        )
        .expect("temporary member");
        {
            let pool = super::super::db::pool().expect("registry");
            let conn = pool.write().expect("write");
            conn.execute("UPDATE project_members SET expires_at='2000-01-01T00:00:00Z' WHERE project_id=?1 AND user_id=?2",params![project_id,user]).expect("expiration");
        }
        assert!(
            !project_access(&project, &user, false)
                .expect("expired access")
                .has_access
        );
        assert!(!member_accesses_for_user(&user)
            .expect("memberships")
            .contains_key(&project_id));
        assert!(transfer_ownership(&project_id, &owner, &user).is_err());
        set_member_access(&project_id, &user, &["tester".to_string()], false, None)
            .expect("reactivate");
        transfer_ownership(&project_id, &owner, &user).expect("transfer");
        let updated = get_project(ORG, &project_id).expect("get").expect("row");
        assert_eq!(updated.owner_user_id, user);
        let new_owner = member_access(&project_id, &user)
            .expect("member")
            .expect("row");
        assert!(new_owner.project_admin);
        assert_eq!(new_owner.functions, vec!["tester"]);
        assert!(new_owner.expires_at.is_none());
    }

    /// Every membership mutation carries current repository access into the linked
    /// workspace: a new member is granted, a promotion is applied to the same
    /// row, and losing project membership loses workspace access.
    #[test]
    fn membership_mutations_reach_a_linked_workspace() {
        // `create_project` opens a database under the Data category, and that
        // override is process-global: without the shared guard a Code Studio
        // test running in parallel moves this test's paths out from under it.
        // It passes alone and fails in a full run, which is the worst shape a
        // test can have.
        let _guard = crate::code_studio::paths::test_data_dir_guard();
        let core = core_registry();
        let project_id = format!("cs-mirror-{}", uuid::Uuid::new_v4());
        let workspace_id = format!("ws-{}", uuid::Uuid::new_v4());
        let owner = format!("owner-{}", uuid::Uuid::new_v4());
        let member = format!("member-{}", uuid::Uuid::new_v4());

        workspace(&core, &workspace_id);
        project_with_owner(&project_id, &owner);
        project_link::link(&core, ORG, &workspace_id, &project_id, &owner).expect("link");
        assert!(
            mirrored(&core, &workspace_id, &project_id, &owner).is_none(),
            "linking alone must not grant anything"
        );

        assert_eq!(
            add_members(
                &project_id,
                &[MemberInput {
                    user_id: member.clone(),
                    functions: vec!["tester".to_string()],
                    project_admin: false,
                    expires_at: None
                }],
                &owner
            )
            .expect("add member"),
            1
        );
        assert_eq!(
            mirrored(&core, &workspace_id, &project_id, &member).as_deref(),
            Some("viewer"),
            "adding a project member did not reach the workspace"
        );
        assert_eq!(
            mirrored(&core, &workspace_id, &project_id, &owner).as_deref(),
            Some("editor"),
            "the pass converges on the whole member list, not just the mutated user"
        );

        assert!(set_member_access(
            &project_id,
            &member,
            &["developer".to_string()],
            false,
            None
        )
        .expect("update functions"));
        assert_eq!(
            mirrored(&core, &workspace_id, &project_id, &member).as_deref(),
            Some("editor"),
            "a function change did not reach the workspace"
        );

        assert!(remove_member(&project_id, &member).expect("remove"));
        assert!(
            mirrored(&core, &workspace_id, &project_id, &member).is_none(),
            "removing a project member left workspace access behind"
        );
        assert_eq!(
            mirrored(&core, &workspace_id, &project_id, &owner).as_deref(),
            Some("editor"),
            "removing one member revoked another"
        );
        assert_eq!(
            code_repo::role_of(&core, &workspace_id, "u-ws-owner").expect("workspace owner"),
            Some(WorkspaceRole::Owner),
            "the mirror touched the workspace owner"
        );
    }

    /// An unlinked workspace never sees the project, a membership granted
    /// inside Code Studio is never taken over, and deleting the project takes
    /// back exactly what the mirror granted.
    #[test]
    fn the_mirror_only_touches_its_own_grants() {
        let _guard = crate::code_studio::paths::test_data_dir_guard();
        let core = core_registry();
        let project_id = format!("cs-mirror-{}", uuid::Uuid::new_v4());
        let linked = format!("ws-{}", uuid::Uuid::new_v4());
        let unlinked = format!("ws-{}", uuid::Uuid::new_v4());
        let owner = format!("owner-{}", uuid::Uuid::new_v4());
        let by_hand = format!("hand-{}", uuid::Uuid::new_v4());

        workspace(&core, &linked);
        workspace(&core, &unlinked);
        project_with_owner(&project_id, &owner);
        project_link::link(&core, ORG, &linked, &project_id, &owner).expect("link");
        code_repo::upsert_member(
            &core,
            &linked,
            &by_hand,
            WorkspaceRole::Editor,
            "u-ws-owner",
        )
        .expect("manual grant");

        assert_eq!(
            add_members(
                &project_id,
                &[MemberInput {
                    user_id: by_hand.clone(),
                    functions: vec!["tester".to_string()],
                    project_admin: false,
                    expires_at: None
                }],
                &owner
            )
            .expect("add member"),
            1
        );
        assert!(
            mirrored(&core, &linked, &project_id, &by_hand).is_none(),
            "the mirror took over a membership it did not create"
        );
        assert_eq!(
            code_repo::role_of(&core, &linked, &by_hand).expect("manual role"),
            Some(WorkspaceRole::Editor),
            "the mirror downgraded a manual grant"
        );
        assert!(
            code_repo::role_of(&core, &unlinked, &owner)
                .expect("unlinked role")
                .is_none(),
            "a workspace without a link received a mirrored membership"
        );

        assert_eq!(
            mirrored(&core, &linked, &project_id, &owner).as_deref(),
            Some("editor")
        );
        let operation_id =
            prepare_project_delete(ORG, &project_id, &owner).expect("prepare delete project");
        delete_project_rows(&project_id, &operation_id).expect("delete project");
        assert!(
            mirrored(&core, &linked, &project_id, &owner).is_none(),
            "deleting the project left its mirrored memberships behind"
        );
        assert_eq!(
            code_repo::role_of(&core, &linked, &by_hand).expect("manual role"),
            Some(WorkspaceRole::Editor),
            "deleting the project removed a membership the mirror never granted"
        );
    }
}

#[cfg(test)]
mod catalogue_race_tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn inherited_type_deactivation_precedes_detach_and_child_task_save() {
        let central_dir = tempfile::tempdir().expect("central directory");
        let _ = super::super::db::init(&central_dir.path().join("projects.db"));
        std::mem::forget(central_dir);
        let content = tempfile::tempdir().expect("content directory");
        let org = uuid::Uuid::new_v4().to_string();
        let parent_id = uuid::Uuid::new_v4().to_string();
        let child_id = uuid::Uuid::new_v4().to_string();
        for (id, parent, prefix) in [
            (&parent_id, None, "PAR"),
            (&child_id, Some(parent_id.as_str()), "CHD"),
        ] {
            let dir = content.path().join(id);
            std::fs::create_dir_all(&dir).expect("content directory");
            create_project(
                id,
                &org,
                if parent.is_some() { "Child" } else { "Parent" },
                "",
                "custom",
                "[\"tasks\"]",
                "owner",
                dir.to_str().expect("path"),
                prefix,
                parent,
                false,
                false,
                parent.is_some(),
                &[],
            )
            .expect("project");
        }
        let parent_pool = super::super::project_db::open(&parent_id).expect("parent pool");
        let child_pool = super::super::project_db::open(&child_id).expect("child pool");
        super::super::tasks::save_task_type(
            &parent_pool,
            "custom_work",
            "Custom work",
            "Live parent catalogue",
            70,
            true,
        )
        .expect("active inherited type");
        let parent_writer = parent_pool.write().expect("hold parent writer");
        let barrier = Arc::new(Barrier::new(3));
        std::thread::scope(|scope| {
            let create_barrier = barrier.clone();
            let create_pool = &child_pool;
            let create = scope.spawn(move || {
                create_barrier.wait();
                super::super::tasks::create_task(
                    create_pool,
                    &super::super::tasks::TaskInput {
                        task_type: "custom_work",
                        title: "Must observe inactive type",
                        description_md: "",
                        severity: "",
                        priority: "medium",
                        status: "todo",
                        assigned_to: "",
                        due_date: "",
                        parent_task_id: None,
                        links_json: "[]",
                        attachments_json: "[]",
                    },
                    "owner",
                )
            });
            let detach_barrier = barrier.clone();
            let detach_org = &org;
            let detach_child = &child_id;
            let detach_parent = &parent_id;
            let detach = scope.spawn(move || {
                detach_barrier.wait();
                save_project_inheritance(
                    detach_org,
                    detach_child,
                    false,
                    false,
                    false,
                    "[\"tasks\"]",
                    detach_parent,
                )
            });
            barrier.wait();
            parent_writer
                .execute(
                    "UPDATE task_types SET active=0 WHERE type_id='custom_work'",
                    [],
                )
                .expect("deactivate while ordered source writer is held");
            drop(parent_writer);
            assert!(create.join().expect("task writer thread").is_err());
            assert_eq!(detach.join().expect("detach thread").expect("detach"), true);
        });
        let child_type = super::super::tasks::list_task_types(&child_pool)
            .expect("detached catalogue")
            .into_iter()
            .find(|item| item.type_id == "custom_work")
            .expect("copied type");
        assert!(!child_type.active);
        assert_eq!(
            effective_project_settings(&child_id).expect("settings").1,
            child_id
        );
        let task_count: i64 = child_pool
            .read()
            .expect("child read")
            .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
            .expect("tasks");
        assert_eq!(task_count, 0);
        super::super::tasks::save_task_type(
            &parent_pool,
            "custom_work",
            "Custom work",
            "Parent reactivated after detach",
            70,
            true,
        )
        .expect("reactivate parent only");
        assert!(
            !super::super::tasks::list_task_types(&child_pool)
                .expect("child catalogue")
                .into_iter()
                .find(|item| item.type_id == "custom_work")
                .expect("child type")
                .active
        );
    }
}
