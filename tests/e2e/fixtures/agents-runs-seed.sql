-- =============================================================================
-- File: tests/e2e/fixtures/agents-runs-seed.sql
-- Description: Run history for the Agents UI suite. The seeded roster already
--              ships `generator-scenariuszy` with a stable UUID, so the runs
--              attach to it directly. One run per terminal status plus one
--              still running, so the status filter, the KPI tiles and the
--              cancel/details row action all have something to act on.
--              `user_id` matches the seeded admin, whose display name the
--              Inicjator column resolves through the IAM roster.
-- =============================================================================

-- The agent itself: the factory roster has no QA scenario generator, and the
-- suite needs one agent whose configuration exercises every A02 control
-- (routable, sub-agents, skills, a prompt with a {{var}}) plus tools from both
-- a core group and an addon package.
DELETE FROM agent_runs WHERE agent_id = '11111111-1111-4111-8111-111111111111';
DELETE FROM agents WHERE id = '11111111-1111-4111-8111-111111111111';

INSERT INTO agents
  (id, name, display_name, description, system_prompt, model, tools_json, skills_json,
   params_json, max_iterations, timeout_secs, max_subagents, max_spawn_depth, flow_id,
   routable, is_enabled, on_child_complete)
VALUES
  ('11111111-1111-4111-8111-111111111111', 'generator-scenariuszy', 'Generator scenariuszy',
   'Tworzy przypadki testowe z wymagan i wiedzy projektu. Samodzielnie wyszukuje brakujace informacje.',
   'Jestes generatorem przypadkow testowych dla zespolu QA. Jezyk przypadkow: {{jezyk}}.',
   NULL,
   '["core.project_search","core.project_case_save","core.ask_user","core.agent_spawn","core.agent_wait","deep-research.search_web","deep-research.fetch_url"]',
   '{"names":["pisanie-scenariuszy"],"tags":["qa"]}',
   '{"temperature":0.4}',
   12, 600, 4, 2, NULL, 1, 1, 'notify');

INSERT INTO agent_runs
  (id, agent_id, parent_run_id, flow_execution_id, user_id, status, prompt, result,
   exit_reason, iterations, total_tokens, run_log, started_at, finished_at, created_at)
VALUES
  ('aaaa0001-0000-4000-8000-000000000001', '11111111-1111-4111-8111-111111111111', NULL, 8812,
   '00000000-0000-4000-8000-000000000002', 'completed',
   'Przypadki dla płatności cyklicznych + raport podsumowujący',
   'Zapisano 12 przypadków do generacji GEN-42.',
   'completed', 9, 18412, '[]',
   datetime('now', '-2 hours'), datetime('now', '-2 hours', '+161 seconds'), datetime('now', '-2 hours')),

  ('aaaa0002-0000-4000-8000-000000000002', '11111111-1111-4111-8111-111111111111', NULL, NULL,
   '00000000-0000-4000-8000-000000000002', 'failed',
   'Scenariusze API /orders — limit czasu narzędzia', NULL,
   'error:tool timeout', 2, 1900, '[]',
   datetime('now', '-3 hours'), datetime('now', '-3 hours', '+38 seconds'), datetime('now', '-3 hours')),

  ('aaaa0003-0000-4000-8000-000000000003', '11111111-1111-4111-8111-111111111111', NULL, NULL,
   NULL, 'cancelled',
   'Test promptu po zmianie konfiguracji', NULL,
   'cancelled by operator', 1, 400, '[]',
   datetime('now', '-4 hours'), datetime('now', '-4 hours', '+12 seconds'), datetime('now', '-4 hours')),

  -- Seeded as `running`, but a run nobody supervises is closed as `interrupted`
  -- by `reconcile_orphan_local_run` on the next boot — which is exactly the
  -- state the suite asserts on.
  ('aaaa0004-0000-4000-8000-000000000004', '11111111-1111-4111-8111-111111111111', NULL, NULL,
   '00000000-0000-4000-8000-000000000002', 'running',
   'Scenariusze dla modułu logowania portalu B2B (MFA, blokada konta)', NULL,
   NULL, 4, 6214, '[]',
   datetime('now', '-2 minutes'), NULL, datetime('now', '-2 minutes')),

  -- Outside the 30-day window: proves the period filter actually cuts rows.
  -- 40 days, not 90: the agent-runs retention purge blanks the prompt of a run
  -- older than its window, and the suite reads that column.
  ('aaaa0005-0000-4000-8000-000000000005', '11111111-1111-4111-8111-111111111111', NULL, NULL,
   '00000000-0000-4000-8000-000000000002', 'completed',
   'Stary przebieg spoza okna 30 dni', 'ok',
   'completed', 3, 900, '[]',
   datetime('now', '-40 days'), datetime('now', '-40 days', '+30 seconds'), datetime('now', '-40 days'));
