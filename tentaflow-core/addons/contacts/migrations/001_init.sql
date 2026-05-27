CREATE TABLE IF NOT EXISTS companies (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  display_name TEXT NOT NULL,
  nip TEXT,
  regon TEXT,
  krs TEXT,
  vat_id TEXT,
  address_street TEXT,
  address_city TEXT,
  address_postal TEXT,
  address_country TEXT NOT NULL DEFAULT 'PL',
  website TEXT,
  phone_main TEXT,
  email_main TEXT,
  industry TEXT,
  size_employees INTEGER,
  parent_company_id TEXT,
  parent_share_pct REAL,
  is_active INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  source TEXT NOT NULL DEFAULT 'manual',
  FOREIGN KEY(parent_company_id) REFERENCES companies(id)
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_companies_nip
ON companies(nip)
WHERE nip IS NOT NULL AND nip <> '';

CREATE UNIQUE INDEX IF NOT EXISTS ux_companies_regon
ON companies(regon)
WHERE regon IS NOT NULL AND regon <> '';

CREATE INDEX IF NOT EXISTS ix_companies_name ON companies(name);
CREATE INDEX IF NOT EXISTS ix_companies_parent ON companies(parent_company_id);

CREATE TABLE IF NOT EXISTS persons (
  id TEXT PRIMARY KEY,
  first_name TEXT NOT NULL DEFAULT '',
  last_name TEXT NOT NULL DEFAULT '',
  full_name TEXT NOT NULL,
  email_primary TEXT,
  phone_primary TEXT,
  linkedin_url TEXT,
  kind TEXT NOT NULL DEFAULT 'external',
  user_id TEXT,
  current_employer_company_id TEXT,
  current_position_in_company TEXT,
  language TEXT,
  rodo_consent_at INTEGER,
  notes TEXT,
  is_active INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  source TEXT NOT NULL DEFAULT 'manual',
  FOREIGN KEY(current_employer_company_id) REFERENCES companies(id)
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_persons_email
ON persons(email_primary)
WHERE email_primary IS NOT NULL AND email_primary <> '';

CREATE INDEX IF NOT EXISTS ix_persons_full_name ON persons(full_name);
CREATE INDEX IF NOT EXISTS ix_persons_current_company ON persons(current_employer_company_id);

CREATE TABLE IF NOT EXISTS person_emails (
  id TEXT PRIMARY KEY,
  person_id TEXT NOT NULL,
  value TEXT NOT NULL,
  kind TEXT NOT NULL DEFAULT 'work',
  is_primary INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  FOREIGN KEY(person_id) REFERENCES persons(id) ON DELETE CASCADE
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_person_emails_value ON person_emails(value);

CREATE TABLE IF NOT EXISTS person_phones (
  id TEXT PRIMARY KEY,
  person_id TEXT NOT NULL,
  value TEXT NOT NULL,
  kind TEXT NOT NULL DEFAULT 'work',
  is_primary INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  FOREIGN KEY(person_id) REFERENCES persons(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS company_persons (
  id TEXT PRIMARY KEY,
  person_id TEXT NOT NULL,
  company_id TEXT NOT NULL,
  position_title TEXT,
  department TEXT,
  started_at TEXT,
  ended_at TEXT,
  is_current INTEGER NOT NULL DEFAULT 1,
  is_primary INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  FOREIGN KEY(person_id) REFERENCES persons(id) ON DELETE CASCADE,
  FOREIGN KEY(company_id) REFERENCES companies(id) ON DELETE CASCADE
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_company_person_current
ON company_persons(person_id, company_id)
WHERE is_current = 1;

CREATE INDEX IF NOT EXISTS ix_company_persons_company ON company_persons(company_id);
CREATE INDEX IF NOT EXISTS ix_company_persons_person ON company_persons(person_id);

CREATE TABLE IF NOT EXISTS person_relations (
  id TEXT PRIMARY KEY,
  source_person_id TEXT NOT NULL,
  target_person_id TEXT NOT NULL,
  company_id TEXT,
  relation_type TEXT NOT NULL,
  strength REAL,
  evidence TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  FOREIGN KEY(source_person_id) REFERENCES persons(id) ON DELETE CASCADE,
  FOREIGN KEY(target_person_id) REFERENCES persons(id) ON DELETE CASCADE,
  FOREIGN KEY(company_id) REFERENCES companies(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS ix_person_relations_source ON person_relations(source_person_id);
CREATE INDEX IF NOT EXISTS ix_person_relations_target ON person_relations(target_person_id);
CREATE INDEX IF NOT EXISTS ix_person_relations_company ON person_relations(company_id);

CREATE TABLE IF NOT EXISTS sales_roles (
  person_id TEXT NOT NULL,
  company_id TEXT,
  role_key TEXT NOT NULL,
  label TEXT NOT NULL,
  confidence REAL,
  evidence TEXT,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY(person_id, company_id, role_key),
  FOREIGN KEY(person_id) REFERENCES persons(id) ON DELETE CASCADE,
  FOREIGN KEY(company_id) REFERENCES companies(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS tags (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  color TEXT,
  created_at INTEGER NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_tags_name ON tags(name);

CREATE TABLE IF NOT EXISTS company_tags (
  company_id TEXT NOT NULL,
  tag_id TEXT NOT NULL,
  PRIMARY KEY(company_id, tag_id),
  FOREIGN KEY(company_id) REFERENCES companies(id) ON DELETE CASCADE,
  FOREIGN KEY(tag_id) REFERENCES tags(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS person_tags (
  person_id TEXT NOT NULL,
  tag_id TEXT NOT NULL,
  PRIMARY KEY(person_id, tag_id),
  FOREIGN KEY(person_id) REFERENCES persons(id) ON DELETE CASCADE,
  FOREIGN KEY(tag_id) REFERENCES tags(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS smart_lists (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  kind TEXT NOT NULL,
  query_json TEXT,
  owner_user_id TEXT,
  is_public INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS smart_list_members (
  list_id TEXT NOT NULL,
  resource_kind TEXT NOT NULL,
  resource_id TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY(list_id, resource_kind, resource_id),
  FOREIGN KEY(list_id) REFERENCES smart_lists(id) ON DELETE CASCADE
);
