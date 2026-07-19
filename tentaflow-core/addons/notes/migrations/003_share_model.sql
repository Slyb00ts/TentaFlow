-- =============================================================================
-- File: addons/notes/migrations/003_share_model.sql
-- Purpose: align persisted shares with the v0.3 share model. v0.2 allowed
--          org-wide WRITE shares; v0.3 restricts subject_type='org' to
--          access='read' (validate_share_entries) — but acl_write_clause
--          honors whatever rows exist, so a leftover org/write row would let
--          EVERY org member edit and delete that note after the upgrade.
--          Downgrading those rows to read closes that hole while keeping the
--          org-wide visibility the owner originally granted.
-- =============================================================================

UPDATE note_shares SET access = 'read'
 WHERE subject_type = 'org' AND access = 'write';
