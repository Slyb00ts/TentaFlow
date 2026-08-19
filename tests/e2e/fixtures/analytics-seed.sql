-- =============================================================================
-- File: tests/e2e/fixtures/analytics-seed.sql
-- Description: Deterministic, idempotent fixture for the unified "Analityka"
--              dashboard e2e suite. Applied with the sqlite3 CLI against a DB
--              that the binary has already migrated (first start creates the
--              schema). Every time-dependent value is relative to 'now' so the
--              data always lands in the current (and previous) month.
-- =============================================================================
PRAGMA foreign_keys = ON;
BEGIN;

-- ---------------------------------------------------------------------------
-- Users (admin exists after the first start; give it a name + e-mail)
-- ---------------------------------------------------------------------------
UPDATE user_accounts
   SET display_name = 'Administrator', email = 'admin@firma.pl'
 WHERE id = '00000000-0000-4000-8000-000000000002';

INSERT OR REPLACE INTO user_accounts (id, username, password_hash, display_name, email, is_active, is_admin, role)
VALUES
  ('8d1f02aa-4c3e-4b8a-9f21-7a5d3e9bc771', 'marta.k', '!seed', 'Marta Kowalczyk',  'marta.k@firma.pl', 1, 0, 'user'),
  ('3b7e9c12-8a4f-4d6e-b2c1-5f8a7d3e1c42', 'piotr.w', '!seed', 'Piotr Wiśniewski', 'piotr.w@firma.pl', 1, 0, 'user'),
  ('c4a81f6d-2e9b-4f3a-8d7c-1b6e5a9f2d83', 'janek.n', '!seed', 'Janek Nowak',      'janek.n@firma.pl', 1, 0, 'user'),
  ('7f2d4b9e-6c1a-4e8f-a3b5-9d2c7e4f1a64', 'anna.z',  '!seed', 'Anna Zielińska',   'anna.z@firma.pl',  1, 0, 'user');

INSERT OR REPLACE INTO org_memberships (org_id, user_id, role_id, granted_at, granted_by)
SELECT 'org-default', id, 'role-org-viewer', strftime('%Y-%m-%dT%H:%M:%SZ','now'), 'system'
  FROM user_accounts
 WHERE id IN ('8d1f02aa-4c3e-4b8a-9f21-7a5d3e9bc771','3b7e9c12-8a4f-4d6e-b2c1-5f8a7d3e1c42',
              'c4a81f6d-2e9b-4f3a-8d7c-1b6e5a9f2d83','7f2d4b9e-6c1a-4e8f-a3b5-9d2c7e4f1a64');

-- ---------------------------------------------------------------------------
-- Groups: Marketing (4 members), Zarząd (2 members)
-- ---------------------------------------------------------------------------
INSERT OR REPLACE INTO user_groups (id, name, description) VALUES
  ('a6e4d2c8-1f7b-4c9a-b3e5-8d2f6a4c1b17', 'Marketing', 'Zespół marketingu'),
  ('d1f8b3e6-7c2a-4e5d-9b1f-4a8c6e2d7f28', 'Zarząd',    'Zarząd spółki');

INSERT OR REPLACE INTO group_members (group_id, user_id) VALUES
  ('a6e4d2c8-1f7b-4c9a-b3e5-8d2f6a4c1b17', '8d1f02aa-4c3e-4b8a-9f21-7a5d3e9bc771'),
  ('a6e4d2c8-1f7b-4c9a-b3e5-8d2f6a4c1b17', '3b7e9c12-8a4f-4d6e-b2c1-5f8a7d3e1c42'),
  ('a6e4d2c8-1f7b-4c9a-b3e5-8d2f6a4c1b17', 'c4a81f6d-2e9b-4f3a-8d7c-1b6e5a9f2d83'),
  ('a6e4d2c8-1f7b-4c9a-b3e5-8d2f6a4c1b17', '7f2d4b9e-6c1a-4e8f-a3b5-9d2c7e4f1a64'),
  ('d1f8b3e6-7c2a-4e5d-9b1f-4a8c6e2d7f28', '00000000-0000-4000-8000-000000000002'),
  ('d1f8b3e6-7c2a-4e5d-9b1f-4a8c6e2d7f28', '3b7e9c12-8a4f-4d6e-b2c1-5f8a7d3e1c42');

-- ---------------------------------------------------------------------------
-- Sync nodes (64-hex ids): hazai + biuro-mini online, laptop-pw seen 2 h ago.
-- ---------------------------------------------------------------------------
INSERT OR REPLACE INTO sync_nodes (node_id, public_key, public_key_type, display_name, node_kind, trust_status, sync_profile, last_seen_at)
VALUES
  ('d91a7a857611b069a19759c7ca2c5c1b83901ae4fcae86bd7dfe2f1e493f3967',
   'd91a7a857611b069a19759c7ca2c5c1b83901ae4fcae86bd7dfe2f1e493f3967', 'ed25519',
   'hazai', 'server', 'trusted', 'standard', strftime('%Y-%m-%dT%H:%M:%SZ','now')),
  ('7c02be11f4a3d9e86b5c2a1f0e9d8c7b6a5f4e3d2c1b0a9f8e7d6c5b4a3f2e1d',
   '7c02be11f4a3d9e86b5c2a1f0e9d8c7b6a5f4e3d2c1b0a9f8e7d6c5b4a3f2e1d', 'ed25519',
   'biuro-mini', 'desktop', 'trusted', 'standard', strftime('%Y-%m-%dT%H:%M:%SZ','now')),
  ('33f1c904a7b2e5d8c1f4a7b0e3d6c9f2a5b8e1d4c7f0a3b6e9d2c5f8a1b4e7d0',
   '33f1c904a7b2e5d8c1f4a7b0e3d6c9f2a5b8e1d4c7f0a3b6e9d2c5f8a1b4e7d0', 'ed25519',
   'laptop-pw', 'laptop', 'trusted', 'standard', strftime('%Y-%m-%dT%H:%M:%SZ','now','-2 hours'));

-- ---------------------------------------------------------------------------
-- Services + model catalog (model_registry has FK -> services)
-- ---------------------------------------------------------------------------
INSERT OR REPLACE INTO services (id, engine_id, category, display_name, deploy_method, transport, status, config_json)
VALUES
  (9001, 'vllm',    'llm', 'vLLM Qwen 3.8 27B', 'external', 'external_http', 'stopped', '{}'),
  (9002, 'vllm',    'llm', 'vLLM GLM-5-Air',    'external', 'external_http', 'stopped', '{}'),
  (9003, 'whisper', 'stt', 'Whisper large-v3',  'external', 'external_http', 'stopped', '{}');

INSERT OR REPLACE INTO model_registry (id, service_id, model_name, display_name, capabilities, is_default)
VALUES
  (9001, 9001, 'cyankiwi/Qwen3.8-27B-AWQ-INT4', 'Qwen 3.8 27B AWQ', '["chat"]', 1),
  (9002, 9002, 'zai-org/GLM-5-Air',             'GLM-5-Air',        '["chat"]', 1),
  (9003, 9003, 'whisper-large-v3',              'Whisper large-v3', '["stt"]',  1);

-- ---------------------------------------------------------------------------
-- Pricing (PLN): Qwen + GLM priced, whisper deliberately NOT
-- ---------------------------------------------------------------------------
DELETE FROM model_pricing WHERE org_id = 'org-default';
INSERT INTO model_pricing (id, org_id, model_id, prompt_per_1k, completion_per_1k, audio_per_min, image_each)
VALUES
  ('pricing:org-default:cyankiwi/Qwen3.8-27B-AWQ-INT4', 'org-default', 'cyankiwi/Qwen3.8-27B-AWQ-INT4', 0.004, 0.012, 0, 0),
  ('pricing:org-default:zai-org/GLM-5-Air',             'org-default', 'zai-org/GLM-5-Air',             0.006, 0.018, 0, 0);

-- ---------------------------------------------------------------------------
-- Hourly rollup: start of the PREVIOUS month .. the current hour, so the
-- current month always has data (daily/hourly views included) and the
-- overview delta has a previous period. Volumes are pseudo-random but
-- derived from the bucket timestamp, so re-applying gives identical rows.
-- ---------------------------------------------------------------------------
DELETE FROM model_metrics_rollup WHERE id LIKE 'seed:%';

WITH RECURSIVE
bounds(h0, h1) AS (
  SELECT CAST(strftime('%s', 'now', 'start of month', '-1 month') AS INTEGER),
         CAST(strftime('%s', 'now') AS INTEGER)
),
hours(h) AS (
  SELECT h0 FROM bounds
  UNION ALL
  SELECT h + 3600 FROM hours, bounds WHERE h + 3600 <= h1
),
combos(k, user_id, model_id, node_id, service_key, backend, modality, weight) AS (
  VALUES
  (1, '8d1f02aa-4c3e-4b8a-9f21-7a5d3e9bc771', 'cyankiwi/Qwen3.8-27B-AWQ-INT4', 'd91a7a857611b069a19759c7ca2c5c1b83901ae4fcae86bd7dfe2f1e493f3967', 'vllm:qwen3.8-27b', 'vllm',    'chat',  40),
  (2, '8d1f02aa-4c3e-4b8a-9f21-7a5d3e9bc771', 'zai-org/GLM-5-Air',             'd91a7a857611b069a19759c7ca2c5c1b83901ae4fcae86bd7dfe2f1e493f3967', 'vllm:glm-5-air',   'vllm',    'chat',  18),
  (3, '8d1f02aa-4c3e-4b8a-9f21-7a5d3e9bc771', 'whisper-large-v3',              '7c02be11f4a3d9e86b5c2a1f0e9d8c7b6a5f4e3d2c1b0a9f8e7d6c5b4a3f2e1d', 'whisper:large-v3', 'whisper', 'audio',  3),
  (4, '3b7e9c12-8a4f-4d6e-b2c1-5f8a7d3e1c42', 'cyankiwi/Qwen3.8-27B-AWQ-INT4', '33f1c904a7b2e5d8c1f4a7b0e3d6c9f2a5b8e1d4c7f0a3b6e9d2c5f8a1b4e7d0', 'vllm:qwen3.8-27b', 'vllm',    'chat',  22),
  (5, '3b7e9c12-8a4f-4d6e-b2c1-5f8a7d3e1c42', 'zai-org/GLM-5-Air',             'd91a7a857611b069a19759c7ca2c5c1b83901ae4fcae86bd7dfe2f1e493f3967', 'vllm:glm-5-air',   'vllm',    'chat',  26),
  (6, 'c4a81f6d-2e9b-4f3a-8d7c-1b6e5a9f2d83', 'cyankiwi/Qwen3.8-27B-AWQ-INT4', 'd91a7a857611b069a19759c7ca2c5c1b83901ae4fcae86bd7dfe2f1e493f3967', 'vllm:qwen3.8-27b', 'vllm',    'chat',  20),
  (7, '00000000-0000-4000-8000-000000000002', 'cyankiwi/Qwen3.8-27B-AWQ-INT4', '7c02be11f4a3d9e86b5c2a1f0e9d8c7b6a5f4e3d2c1b0a9f8e7d6c5b4a3f2e1d', 'vllm:qwen3.8-27b', 'vllm',    'chat',  14),
  (8, '7f2d4b9e-6c1a-4e8f-a3b5-9d2c7e4f1a64', 'zai-org/GLM-5-Air',             '7c02be11f4a3d9e86b5c2a1f0e9d8c7b6a5f4e3d2c1b0a9f8e7d6c5b4a3f2e1d', 'vllm:glm-5-air',   'vllm',    'chat',  16)
),
cells AS (
  SELECT
    h, k, user_id, model_id, node_id, service_key, backend, modality, weight,
    CAST(strftime('%H', h, 'unixepoch') AS INTEGER) AS hod,
    CAST(strftime('%w', h, 'unixepoch') AS INTEGER) AS dow,
    ((h / 3600) * 31 + k * 97) % 1000 AS r1,
    ((h / 3600) * 17 + k * 53) % 100  AS r2
  FROM hours, combos
),
active AS (
  -- Every hour carries some traffic (the hourly view must never be empty);
  -- working hours on weekdays are the peak.
  SELECT *,
    CASE
      WHEN dow BETWEEN 1 AND 5 AND hod BETWEEN 6 AND 21 THEN 1.0
      WHEN dow IN (0, 6) AND hod BETWEEN 8 AND 18 THEN 0.5
      ELSE 0.25
    END AS act
  FROM cells
),
rows_ AS (
  SELECT *,
    CAST(MAX(1, weight * act * (0.5 + r1 / 1000.0)) AS INTEGER) AS req,
    CASE WHEN r2 < 6 THEN 1 WHEN r2 = 42 THEN 2 ELSE 0 END AS err
  FROM active
)
INSERT INTO model_metrics_rollup (
  id, node_id, org_id, user_id, model_id, service_key, backend, modality, hour_bucket,
  histogram_version, request_count, success_count, error_count,
  prompt_tokens, completion_tokens, total_tokens, embedding_tokens, audio_ms, images,
  prefill_secs_sum, decode_secs_sum, e2e_latency_ms_sum, queue_ms_sum,
  ttft_b0, ttft_b1, ttft_b2, ttft_b3, ttft_b4, ttft_b5, ttft_b6, ttft_b7, ttft_b8, ttft_b9, ttft_sample_count,
  decode_tps_b0, decode_tps_b1, decode_tps_b2, decode_tps_b3, decode_tps_b4, decode_tps_b5, decode_tps_b6, decode_tps_b7, decode_tps_sample_count,
  e2e_b0, e2e_b1, e2e_b2, e2e_b3, e2e_b4, e2e_b5, e2e_b6, e2e_b7, e2e_b8, e2e_b9, e2e_sample_count,
  updated_at
)
SELECT
  'seed:' || k || ':' || strftime('%Y%m%d%H', h, 'unixepoch'),
  node_id, 'org-default', user_id, model_id, service_key, backend, modality,
  strftime('%Y-%m-%dT%H:00:00Z', h, 'unixepoch'),
  1, req, req - MIN(err, req), MIN(err, req),
  CASE WHEN modality = 'audio' THEN 0 ELSE req * (600 + r1 / 5) END,
  CASE WHEN modality = 'audio' THEN req * (40 + r2 / 2) ELSE req * (260 + r2) END,
  CASE WHEN modality = 'audio' THEN req * (40 + r2 / 2) ELSE req * (600 + r1 / 5) + req * (260 + r2) END,
  0,
  CASE WHEN modality = 'audio' THEN req * (30000 + r1 * 90) ELSE 0 END,
  0,
  req * 0.18, req * 2.4, req * (900 + r1), req * 12,
  -- TTFT histogram: chat 50..800 ms (b2..b5), whisper slower (b4..b7)
  0, 0,
  CASE WHEN modality = 'audio' THEN 0 ELSE req * 25 / 100 END,
  CASE WHEN modality = 'audio' THEN 0 ELSE req * 40 / 100 END,
  CASE WHEN modality = 'audio' THEN req * 20 / 100 ELSE req * 25 / 100 END,
  CASE WHEN modality = 'audio' THEN req - req * 20 / 100 - req * 30 / 100 - req * 10 / 100 ELSE req - req * 25 / 100 - req * 40 / 100 - req * 25 / 100 - (CASE WHEN r2 < 10 THEN 1 ELSE 0 END) END,
  CASE WHEN modality = 'audio' THEN req * 30 / 100 ELSE (CASE WHEN r2 < 10 THEN 1 ELSE 0 END) END,
  CASE WHEN modality = 'audio' THEN req * 10 / 100 ELSE 0 END,
  0, 0,
  req,
  -- decode tps histogram: 20..160 tps (b3..b5), GLM a bit slower
  0, 0, 0,
  CASE WHEN model_id LIKE 'zai-org/%' THEN req * 50 / 100 ELSE req * 15 / 100 END,
  CASE WHEN model_id LIKE 'zai-org/%' THEN req * 40 / 100 ELSE req * 55 / 100 END,
  CASE WHEN model_id LIKE 'zai-org/%' THEN req - req * 50 / 100 - req * 40 / 100 ELSE req - req * 15 / 100 - req * 55 / 100 END,
  0, 0,
  req,
  -- e2e histogram: 500 ms..8 s (b3..b7)
  0, 0, 0,
  req * 20 / 100, req * 35 / 100, req * 25 / 100,
  req - req * 20 / 100 - req * 35 / 100 - req * 25 / 100 - (CASE WHEN r2 < 5 THEN 1 ELSE 0 END),
  CASE WHEN r2 < 5 THEN 1 ELSE 0 END,
  0, 0,
  req,
  strftime('%Y-%m-%d %H:%M:%f', 'now')
FROM rows_;

-- ---------------------------------------------------------------------------
-- Token quotas (3) + coordinator leases (2). No user-scoped quota for Marta:
-- the spec creates that one through the UI.
-- ---------------------------------------------------------------------------
DELETE FROM token_lease WHERE org_id = 'org-default';
DELETE FROM token_quota WHERE org_id = 'org-default';
INSERT INTO token_quota (id, org_id, scope_type, subject_id, model_id, period, max_total_tokens, is_active)
VALUES
  ('quota:org-default:group:a6e4d2c8-1f7b-4c9a-b3e5-8d2f6a4c1b17:cyankiwi/Qwen3.8-27B-AWQ-INT4:monthly', 'org-default', 'group',
   'a6e4d2c8-1f7b-4c9a-b3e5-8d2f6a4c1b17', 'cyankiwi/Qwen3.8-27B-AWQ-INT4', 'monthly', 50000000, 1),
  ('quota:org-default:org:*:*:monthly', 'org-default', 'org', NULL, NULL, 'monthly', 380000000, 1),
  ('quota:org-default:model:zai-org/GLM-5-Air:*:daily', 'org-default', 'model',
   'zai-org/GLM-5-Air', NULL, 'daily', 3400000, 0);

INSERT INTO token_lease (id, org_id, quota_id, node_id, period_key, base_used, granted_tokens, coordinator_node_id, expires_at)
VALUES
  ('lease:quota:org-default:org:*:*:monthly:d91a7a857611b069a19759c7ca2c5c1b83901ae4fcae86bd7dfe2f1e493f3967:' || strftime('%Y-%m','now'),
   'org-default', 'quota:org-default:org:*:*:monthly',
   'd91a7a857611b069a19759c7ca2c5c1b83901ae4fcae86bd7dfe2f1e493f3967', strftime('%Y-%m','now'),
   29400000, 2100000, 'd91a7a857611b069a19759c7ca2c5c1b83901ae4fcae86bd7dfe2f1e493f3967',
   strftime('%Y-%m-%dT%H:%M:%SZ','now','+1 hour')),
  ('lease:quota:org-default:group:a6e4d2c8-1f7b-4c9a-b3e5-8d2f6a4c1b17:cyankiwi/Qwen3.8-27B-AWQ-INT4:monthly:7c02be11f4a3d9e86b5c2a1f0e9d8c7b6a5f4e3d2c1b0a9f8e7d6c5b4a3f2e1d:' || strftime('%Y-%m','now'),
   'org-default', 'quota:org-default:group:a6e4d2c8-1f7b-4c9a-b3e5-8d2f6a4c1b17:cyankiwi/Qwen3.8-27B-AWQ-INT4:monthly',
   '7c02be11f4a3d9e86b5c2a1f0e9d8c7b6a5f4e3d2c1b0a9f8e7d6c5b4a3f2e1d', strftime('%Y-%m','now'),
   21000000, 4000000, 'd91a7a857611b069a19759c7ca2c5c1b83901ae4fcae86bd7dfe2f1e493f3967',
   strftime('%Y-%m-%dT%H:%M:%SZ','now','+1 hour'));

COMMIT;
