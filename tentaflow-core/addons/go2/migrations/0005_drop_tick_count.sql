-- =============================================================================
-- go2 addon — drop robot.tick_count. The tick counter cost a SQLite write on
-- every tick (10/s) only to pace the once-a-second status block; the tick now
-- paces that block by the clock, so nothing reads the column.
-- =============================================================================
ALTER TABLE robot DROP COLUMN tick_count;
