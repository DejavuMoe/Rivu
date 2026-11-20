-- 由 bin/rivu/tests/schema_snapshot.rs 生成（RIVU_SCHEMA_UPDATE=1），不要手工编辑。
-- 迁移后 PostgreSQL public 模式的结构：表（列、约束、索引、触发器）与函数，按名字排序。

TABLE annotations
  id bigint NOT NULL GENERATED ALWAYS AS IDENTITY
  site_id bigint NOT NULL
  date date NOT NULL
  text text NOT NULL
  created_by bigint
  deleted_at timestamp with time zone
  created_at timestamp with time zone NOT NULL DEFAULT now()
  updated_at timestamp with time zone NOT NULL DEFAULT now()
  CONSTRAINT annotations_created_by_fkey FOREIGN KEY (created_by) REFERENCES users(id) ON DELETE SET NULL
  CONSTRAINT annotations_pkey PRIMARY KEY (id)
  CONSTRAINT annotations_site_id_fkey FOREIGN KEY (site_id) REFERENCES sites(id)
  CONSTRAINT annotations_text_check CHECK (((char_length(text) >= 1) AND (char_length(text) <= 255)))
  CREATE INDEX annotations_site_date ON public.annotations USING btree (site_id, date) WHERE (deleted_at IS NULL)
  CREATE TRIGGER annotations_set_updated_at BEFORE UPDATE ON public.annotations FOR EACH ROW EXECUTE FUNCTION set_updated_at()

TABLE api_keys
  id bigint NOT NULL GENERATED ALWAYS AS IDENTITY
  organization_id bigint NOT NULL
  user_id bigint NOT NULL
  name text NOT NULL
  key_prefix text NOT NULL
  key_hash bytea NOT NULL
  scopes text[] NOT NULL DEFAULT '{stats:read:*}'::text[]
  last_used_at timestamp with time zone
  expires_at timestamp with time zone
  deleted_at timestamp with time zone
  created_at timestamp with time zone NOT NULL DEFAULT now()
  updated_at timestamp with time zone NOT NULL DEFAULT now()
  CONSTRAINT api_keys_key_hash_check CHECK ((octet_length(key_hash) = 32))
  CONSTRAINT api_keys_key_hash_key UNIQUE (key_hash)
  CONSTRAINT api_keys_organization_id_fkey FOREIGN KEY (organization_id) REFERENCES organizations(id)
  CONSTRAINT api_keys_pkey PRIMARY KEY (id)
  CONSTRAINT api_keys_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id)
  CREATE INDEX api_keys_updated ON public.api_keys USING btree (updated_at)
  CREATE INDEX api_keys_user ON public.api_keys USING btree (user_id)
  CREATE TRIGGER api_keys_set_updated_at BEFORE UPDATE ON public.api_keys FOR EACH ROW EXECUTE FUNCTION set_updated_at()

TABLE auth_sessions
  id bigint NOT NULL GENERATED ALWAYS AS IDENTITY
  user_id bigint NOT NULL
  token_hash bytea NOT NULL
  device_label text
  last_used_at timestamp with time zone NOT NULL DEFAULT now()
  expires_at timestamp with time zone NOT NULL
  revoked_at timestamp with time zone
  created_at timestamp with time zone NOT NULL DEFAULT now()
  updated_at timestamp with time zone NOT NULL DEFAULT now()
  CONSTRAINT auth_sessions_pkey PRIMARY KEY (id)
  CONSTRAINT auth_sessions_token_hash_check CHECK ((octet_length(token_hash) = 32))
  CONSTRAINT auth_sessions_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id)
  CREATE INDEX auth_sessions_expires ON public.auth_sessions USING btree (expires_at)
  CREATE UNIQUE INDEX auth_sessions_token ON public.auth_sessions USING btree (token_hash)
  CREATE INDEX auth_sessions_user ON public.auth_sessions USING btree (user_id)
  CREATE TRIGGER auth_sessions_set_updated_at BEFORE UPDATE ON public.auth_sessions FOR EACH ROW EXECUTE FUNCTION set_updated_at()

TABLE auth_tokens
  id bigint NOT NULL GENERATED ALWAYS AS IDENTITY
  user_id bigint NOT NULL
  kind text NOT NULL
  token_hash bytea NOT NULL
  expires_at timestamp with time zone NOT NULL
  used_at timestamp with time zone
  created_at timestamp with time zone NOT NULL DEFAULT now()
  CONSTRAINT auth_tokens_kind_check CHECK ((kind = 'password_reset'::text))
  CONSTRAINT auth_tokens_pkey PRIMARY KEY (id)
  CONSTRAINT auth_tokens_token_hash_check CHECK ((octet_length(token_hash) = 32))
  CONSTRAINT auth_tokens_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id)
  CREATE INDEX auth_tokens_expires ON public.auth_tokens USING btree (expires_at)
  CREATE UNIQUE INDEX auth_tokens_token ON public.auth_tokens USING btree (token_hash)
  CREATE INDEX auth_tokens_user ON public.auth_tokens USING btree (user_id)

TABLE email_changes
  user_id bigint NOT NULL
  old_email citext NOT NULL
  new_email citext NOT NULL
  lang text NOT NULL
  token_hash bytea NOT NULL
  expires_at timestamp with time zone NOT NULL
  created_at timestamp with time zone NOT NULL DEFAULT now()
  CONSTRAINT email_changes_lang_check CHECK ((lang = ANY (ARRAY['zh'::text, 'en'::text])))
  CONSTRAINT email_changes_new_email_check CHECK (((char_length((new_email)::text) >= 3) AND (char_length((new_email)::text) <= 320)))
  CONSTRAINT email_changes_pkey PRIMARY KEY (user_id)
  CONSTRAINT email_changes_token_hash_check CHECK ((octet_length(token_hash) = 32))
  CONSTRAINT email_changes_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id)
  CREATE INDEX email_changes_expires ON public.email_changes USING btree (expires_at)
  CREATE UNIQUE INDEX email_changes_token ON public.email_changes USING btree (token_hash)

TABLE goals
  id bigint NOT NULL GENERATED ALWAYS AS IDENTITY
  site_id bigint NOT NULL
  kind text NOT NULL
  event_name text
  page_path text
  display_name text
  deleted_at timestamp with time zone
  created_at timestamp with time zone NOT NULL DEFAULT now()
  updated_at timestamp with time zone NOT NULL DEFAULT now()
  scroll_threshold smallint
  CONSTRAINT goals_check CHECK ((((kind = 'event'::text) AND (event_name IS NOT NULL) AND (page_path IS NULL) AND (scroll_threshold IS NULL)) OR ((kind = 'page'::text) AND (page_path IS NOT NULL) AND (event_name IS NULL) AND (scroll_threshold IS NULL)) OR ((kind = 'scroll'::text) AND (page_path IS NOT NULL) AND (event_name IS NULL) AND ((scroll_threshold >= 1) AND (scroll_threshold <= 100)))))
  CONSTRAINT goals_kind_check CHECK ((kind = ANY (ARRAY['event'::text, 'page'::text, 'scroll'::text])))
  CONSTRAINT goals_pkey PRIMARY KEY (id)
  CONSTRAINT goals_site_id_fkey FOREIGN KEY (site_id) REFERENCES sites(id)
  CREATE UNIQUE INDEX goals_unique ON public.goals USING btree (site_id, kind, COALESCE(event_name, page_path), COALESCE((scroll_threshold)::integer, 0)) WHERE (deleted_at IS NULL)
  CREATE INDEX goals_updated ON public.goals USING btree (updated_at)
  CREATE TRIGGER goals_set_updated_at BEFORE UPDATE ON public.goals FOR EACH ROW EXECUTE FUNCTION set_updated_at()

TABLE instance_secrets
  name text NOT NULL
  value bytea NOT NULL
  created_at timestamp with time zone NOT NULL DEFAULT now()
  CONSTRAINT instance_secrets_name_check CHECK ((name = 'routing'::text))
  CONSTRAINT instance_secrets_pkey PRIMARY KEY (name)
  CONSTRAINT instance_secrets_value_check CHECK ((octet_length(value) = 16))

TABLE invitations
  id bigint NOT NULL GENERATED ALWAYS AS IDENTITY
  organization_id bigint NOT NULL
  email citext NOT NULL
  role text
  site_id bigint
  site_role text
  token_hash bytea NOT NULL
  invited_by bigint
  expires_at timestamp with time zone NOT NULL
  accepted_at timestamp with time zone
  deleted_at timestamp with time zone
  created_at timestamp with time zone NOT NULL DEFAULT now()
  updated_at timestamp with time zone NOT NULL DEFAULT now()
  CONSTRAINT invitations_check CHECK ((((site_id IS NULL) AND (role IS NOT NULL) AND (site_role IS NULL)) OR ((site_id IS NOT NULL) AND (role IS NULL) AND (site_role IS NOT NULL))))
  CONSTRAINT invitations_invited_by_fkey FOREIGN KEY (invited_by) REFERENCES users(id) ON DELETE SET NULL
  CONSTRAINT invitations_organization_id_fkey FOREIGN KEY (organization_id) REFERENCES organizations(id)
  CONSTRAINT invitations_pkey PRIMARY KEY (id)
  CONSTRAINT invitations_role_check CHECK ((role = ANY (ARRAY['admin'::text, 'editor'::text, 'viewer'::text])))
  CONSTRAINT invitations_site_id_fkey FOREIGN KEY (site_id) REFERENCES sites(id)
  CONSTRAINT invitations_site_role_check CHECK ((site_role = ANY (ARRAY['editor'::text, 'viewer'::text])))
  CONSTRAINT invitations_token_hash_check CHECK ((octet_length(token_hash) = 32))
  CREATE INDEX invitations_expires ON public.invitations USING btree (expires_at) WHERE (accepted_at IS NULL)
  CREATE UNIQUE INDEX invitations_pending ON public.invitations USING btree (organization_id, email, COALESCE(site_id, (0)::bigint)) WHERE ((accepted_at IS NULL) AND (deleted_at IS NULL))
  CREATE UNIQUE INDEX invitations_token ON public.invitations USING btree (token_hash)
  CREATE TRIGGER invitations_set_updated_at BEFORE UPDATE ON public.invitations FOR EACH ROW EXECUTE FUNCTION set_updated_at()

TABLE jobs
  id bigint NOT NULL GENERATED ALWAYS AS IDENTITY
  kind text NOT NULL
  payload jsonb NOT NULL DEFAULT '{}'::jsonb
  state text NOT NULL DEFAULT 'queued'::text
  priority smallint NOT NULL DEFAULT 0
  run_at timestamp with time zone NOT NULL DEFAULT now()
  attempts integer NOT NULL DEFAULT 0
  max_attempts integer NOT NULL DEFAULT 3
  locked_by text
  locked_until timestamp with time zone
  unique_key text
  last_error text
  finished_at timestamp with time zone
  created_at timestamp with time zone NOT NULL DEFAULT now()
  updated_at timestamp with time zone NOT NULL DEFAULT now()
  CONSTRAINT jobs_pkey PRIMARY KEY (id)
  CONSTRAINT jobs_state_check CHECK ((state = ANY (ARRAY['queued'::text, 'running'::text, 'succeeded'::text, 'failed'::text, 'cancelled'::text])))
  CREATE INDEX jobs_finished ON public.jobs USING btree (finished_at) WHERE (finished_at IS NOT NULL)
  CREATE INDEX jobs_lease ON public.jobs USING btree (locked_until) WHERE (state = 'running'::text)
  CREATE INDEX jobs_ready ON public.jobs USING btree (priority DESC, run_at) WHERE (state = 'queued'::text)
  CREATE UNIQUE INDEX jobs_unique_active ON public.jobs USING btree (unique_key) WHERE ((unique_key IS NOT NULL) AND (state = ANY (ARRAY['queued'::text, 'running'::text])))
  CREATE TRIGGER jobs_set_updated_at BEFORE UPDATE ON public.jobs FOR EACH ROW EXECUTE FUNCTION set_updated_at()

TABLE memberships
  id bigint NOT NULL GENERATED ALWAYS AS IDENTITY
  organization_id bigint NOT NULL
  user_id bigint NOT NULL
  role text NOT NULL
  deleted_at timestamp with time zone
  created_at timestamp with time zone NOT NULL DEFAULT now()
  updated_at timestamp with time zone NOT NULL DEFAULT now()
  CONSTRAINT memberships_organization_id_fkey FOREIGN KEY (organization_id) REFERENCES organizations(id)
  CONSTRAINT memberships_pkey PRIMARY KEY (id)
  CONSTRAINT memberships_role_check CHECK ((role = ANY (ARRAY['owner'::text, 'admin'::text, 'editor'::text, 'viewer'::text])))
  CONSTRAINT memberships_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id)
  CREATE UNIQUE INDEX memberships_org_user ON public.memberships USING btree (organization_id, user_id) WHERE (deleted_at IS NULL)
  CREATE INDEX memberships_updated ON public.memberships USING btree (updated_at)
  CREATE INDEX memberships_user ON public.memberships USING btree (user_id) WHERE (deleted_at IS NULL)
  CREATE TRIGGER memberships_set_updated_at BEFORE UPDATE ON public.memberships FOR EACH ROW EXECUTE FUNCTION set_updated_at()

TABLE organizations
  id bigint NOT NULL GENERATED ALWAYS AS IDENTITY
  name text NOT NULL
  slug text NOT NULL
  api_hourly_limit integer
  deleted_at timestamp with time zone
  created_at timestamp with time zone NOT NULL DEFAULT now()
  updated_at timestamp with time zone NOT NULL DEFAULT now()
  CONSTRAINT organizations_api_hourly_limit_check CHECK ((api_hourly_limit > 0))
  CONSTRAINT organizations_name_check CHECK (((char_length(name) >= 1) AND (char_length(name) <= 100)))
  CONSTRAINT organizations_pkey PRIMARY KEY (id)
  CREATE UNIQUE INDEX organizations_slug ON public.organizations USING btree (lower(slug)) WHERE (deleted_at IS NULL)
  CREATE TRIGGER organizations_set_updated_at BEFORE UPDATE ON public.organizations FOR EACH ROW EXECUTE FUNCTION set_updated_at()

TABLE pending_stats_deletions
  id bigint NOT NULL GENERATED ALWAYS AS IDENTITY
  site_id bigint NOT NULL
  scope text NOT NULL
  date_from date
  date_to date
  status text NOT NULL DEFAULT 'pending'::text
  not_before timestamp with time zone NOT NULL DEFAULT now()
  partitions_done text[] NOT NULL DEFAULT '{}'::text[]
  attempts integer NOT NULL DEFAULT 0
  last_error text
  completed_at timestamp with time zone
  requested_by bigint
  created_at timestamp with time zone NOT NULL DEFAULT now()
  updated_at timestamp with time zone NOT NULL DEFAULT now()
  timezone text
  utc_start_ms bigint
  utc_end_ms bigint
  rejected_before_write boolean NOT NULL DEFAULT false
  CONSTRAINT pending_stats_deletions_check CHECK (((scope = 'site'::text) OR ((date_from IS NOT NULL) AND (date_to >= date_from))))
  CONSTRAINT pending_stats_deletions_frozen_range CHECK ((((timezone IS NULL) AND (utc_start_ms IS NULL) AND (utc_end_ms IS NULL)) OR ((scope = 'date_range'::text) AND (timezone IS NOT NULL) AND (utc_start_ms IS NOT NULL) AND (utc_end_ms IS NOT NULL) AND (utc_start_ms <= utc_end_ms))))
  CONSTRAINT pending_stats_deletions_pkey PRIMARY KEY (id)
  CONSTRAINT pending_stats_deletions_requested_by_fkey FOREIGN KEY (requested_by) REFERENCES users(id) ON DELETE SET NULL
  CONSTRAINT pending_stats_deletions_scope_check CHECK ((scope = ANY (ARRAY['site'::text, 'date_range'::text])))
  CONSTRAINT pending_stats_deletions_status_check CHECK ((status = ANY (ARRAY['pending'::text, 'running'::text, 'done'::text, 'failed'::text, 'cancelled'::text])))
  CREATE INDEX pending_stats_deletions_ready ON public.pending_stats_deletions USING btree (not_before) WHERE (status = ANY (ARRAY['pending'::text, 'running'::text]))
  CREATE UNIQUE INDEX pending_stats_deletions_site_once ON public.pending_stats_deletions USING btree (site_id) WHERE ((scope = 'site'::text) AND (status <> 'cancelled'::text))
  CREATE TRIGGER pending_stats_deletions_set_updated_at BEFORE UPDATE ON public.pending_stats_deletions FOR EACH ROW EXECUTE FUNCTION set_updated_at()

TABLE salts
  day date NOT NULL
  salt bytea NOT NULL
  created_at timestamp with time zone NOT NULL DEFAULT now()
  CONSTRAINT salts_pkey PRIMARY KEY (day)
  CONSTRAINT salts_salt_check CHECK ((octet_length(salt) = 16))

TABLE schema_migrations
  version bigint NOT NULL
  name text NOT NULL
  checksum text NOT NULL
  applied_at timestamp with time zone NOT NULL DEFAULT now()
  CONSTRAINT schema_migrations_pkey PRIMARY KEY (version)

TABLE segments
  id bigint NOT NULL GENERATED ALWAYS AS IDENTITY
  site_id bigint NOT NULL
  owner_user_id bigint
  name text NOT NULL
  filter jsonb NOT NULL
  deleted_at timestamp with time zone
  created_at timestamp with time zone NOT NULL DEFAULT now()
  updated_at timestamp with time zone NOT NULL DEFAULT now()
  CONSTRAINT segments_name_check CHECK (((char_length(name) >= 1) AND (char_length(name) <= 100)))
  CONSTRAINT segments_owner_user_id_fkey FOREIGN KEY (owner_user_id) REFERENCES users(id) ON DELETE SET NULL
  CONSTRAINT segments_pkey PRIMARY KEY (id)
  CONSTRAINT segments_site_id_fkey FOREIGN KEY (site_id) REFERENCES sites(id)
  CREATE INDEX segments_site ON public.segments USING btree (site_id)
  CREATE UNIQUE INDEX segments_site_name ON public.segments USING btree (site_id, lower(name)) WHERE ((deleted_at IS NULL) AND (owner_user_id IS NULL))
  CREATE TRIGGER segments_set_updated_at BEFORE UPDATE ON public.segments FOR EACH ROW EXECUTE FUNCTION set_updated_at()

TABLE shared_link_grants
  id bigint NOT NULL GENERATED ALWAYS AS IDENTITY
  shared_link_id bigint NOT NULL
  token_hash bytea NOT NULL
  expires_at timestamp with time zone NOT NULL
  created_at timestamp with time zone NOT NULL DEFAULT now()
  CONSTRAINT shared_link_grants_pkey PRIMARY KEY (id)
  CONSTRAINT shared_link_grants_shared_link_id_fkey FOREIGN KEY (shared_link_id) REFERENCES shared_links(id)
  CONSTRAINT shared_link_grants_token_hash_check CHECK ((octet_length(token_hash) = 32))
  CREATE INDEX shared_link_grants_expires ON public.shared_link_grants USING btree (expires_at)
  CREATE INDEX shared_link_grants_link ON public.shared_link_grants USING btree (shared_link_id)
  CREATE UNIQUE INDEX shared_link_grants_token ON public.shared_link_grants USING btree (token_hash)

TABLE shared_links
  id bigint NOT NULL GENERATED ALWAYS AS IDENTITY
  site_id bigint NOT NULL
  slug text NOT NULL
  name text
  password_hash text
  segment_id bigint
  expires_at timestamp with time zone
  created_by bigint
  deleted_at timestamp with time zone
  created_at timestamp with time zone NOT NULL DEFAULT now()
  updated_at timestamp with time zone NOT NULL DEFAULT now()
  CONSTRAINT shared_links_created_by_fkey FOREIGN KEY (created_by) REFERENCES users(id) ON DELETE SET NULL
  CONSTRAINT shared_links_name_check CHECK (((char_length(name) >= 1) AND (char_length(name) <= 100)))
  CONSTRAINT shared_links_pkey PRIMARY KEY (id)
  CONSTRAINT shared_links_segment_id_fkey FOREIGN KEY (segment_id) REFERENCES segments(id)
  CONSTRAINT shared_links_site_id_fkey FOREIGN KEY (site_id) REFERENCES sites(id)
  CONSTRAINT shared_links_slug_check CHECK ((char_length(slug) >= 24))
  CREATE INDEX shared_links_segment ON public.shared_links USING btree (segment_id) WHERE ((segment_id IS NOT NULL) AND (deleted_at IS NULL))
  CREATE INDEX shared_links_site ON public.shared_links USING btree (site_id) WHERE (deleted_at IS NULL)
  CREATE UNIQUE INDEX shared_links_slug ON public.shared_links USING btree (slug)
  CREATE INDEX shared_links_updated ON public.shared_links USING btree (updated_at)
  CREATE TRIGGER shared_links_set_updated_at BEFORE UPDATE ON public.shared_links FOR EACH ROW EXECUTE FUNCTION set_updated_at()

TABLE shield_rules
  id uuid NOT NULL DEFAULT gen_random_uuid()
  site_id bigint NOT NULL
  type text NOT NULL
  action text NOT NULL
  value text NOT NULL
  description text
  created_by bigint
  deleted_at timestamp with time zone
  created_at timestamp with time zone NOT NULL DEFAULT now()
  updated_at timestamp with time zone NOT NULL DEFAULT now()
  CONSTRAINT shield_rules_action_check CHECK ((action = ANY (ARRAY['deny'::text, 'allow'::text])))
  CONSTRAINT shield_rules_check CHECK (((type <> 'country'::text) OR (value ~ '^[A-Z]{2}$'::text)))
  CONSTRAINT shield_rules_created_by_fkey FOREIGN KEY (created_by) REFERENCES users(id) ON DELETE SET NULL
  CONSTRAINT shield_rules_pkey PRIMARY KEY (id)
  CONSTRAINT shield_rules_site_id_fkey FOREIGN KEY (site_id) REFERENCES sites(id)
  CONSTRAINT shield_rules_type_check CHECK ((type = ANY (ARRAY['ip'::text, 'cidr'::text, 'country'::text, 'hostname'::text, 'page'::text])))
  CREATE UNIQUE INDEX shield_rules_unique ON public.shield_rules USING btree (site_id, type, value) WHERE (deleted_at IS NULL)
  CREATE INDEX shield_rules_updated ON public.shield_rules USING btree (updated_at)
  CREATE TRIGGER shield_rules_notify_changed AFTER INSERT OR UPDATE ON public.shield_rules FOR EACH ROW EXECUTE FUNCTION notify_site_changed('site_id')
  CREATE TRIGGER shield_rules_set_updated_at BEFORE UPDATE ON public.shield_rules FOR EACH ROW EXECUTE FUNCTION set_updated_at()

TABLE site_hostnames
  id bigint NOT NULL GENERATED ALWAYS AS IDENTITY
  site_id bigint NOT NULL
  pattern text NOT NULL
  deleted_at timestamp with time zone
  created_at timestamp with time zone NOT NULL DEFAULT now()
  updated_at timestamp with time zone NOT NULL DEFAULT now()
  CONSTRAINT site_hostnames_pattern_check CHECK (((pattern = lower(pattern)) AND (pattern ~ '^(\*\.)?([a-z0-9-]+\.)*[a-z0-9-]+$'::text)))
  CONSTRAINT site_hostnames_pkey PRIMARY KEY (id)
  CONSTRAINT site_hostnames_site_id_fkey FOREIGN KEY (site_id) REFERENCES sites(id)
  CREATE UNIQUE INDEX site_hostnames_site_pattern ON public.site_hostnames USING btree (site_id, pattern) WHERE (deleted_at IS NULL)
  CREATE INDEX site_hostnames_updated ON public.site_hostnames USING btree (updated_at)
  CREATE TRIGGER site_hostnames_notify_changed AFTER INSERT OR UPDATE ON public.site_hostnames FOR EACH ROW EXECUTE FUNCTION notify_site_changed('site_id')
  CREATE TRIGGER site_hostnames_set_updated_at BEFORE UPDATE ON public.site_hostnames FOR EACH ROW EXECUTE FUNCTION set_updated_at()

TABLE site_imports
  id bigint NOT NULL GENERATED ALWAYS AS IDENTITY
  site_id bigint NOT NULL
  source text NOT NULL
  status text NOT NULL DEFAULT 'importing'::text
  file_name text NOT NULL
  start_date date
  end_date date
  rows_imported bigint NOT NULL DEFAULT 0
  error_code text
  error text
  imported_by bigint
  created_at timestamp with time zone NOT NULL DEFAULT now()
  completed_at timestamp with time zone
  CONSTRAINT site_imports_check CHECK ((end_date >= start_date))
  CONSTRAINT site_imports_error_code_check CHECK ((error_code = ANY (ARRAY['invalid_zip'::text, 'not_plausible'::text, 'invalid_csv'::text, 'no_days'::text, 'overlap'::text, 'interrupted'::text, 'internal'::text])))
  CONSTRAINT site_imports_imported_by_fkey FOREIGN KEY (imported_by) REFERENCES users(id) ON DELETE SET NULL
  CONSTRAINT site_imports_pkey PRIMARY KEY (id)
  CONSTRAINT site_imports_site_id_fkey FOREIGN KEY (site_id) REFERENCES sites(id) ON DELETE CASCADE
  CONSTRAINT site_imports_source_check CHECK ((source = 'plausible_csv'::text))
  CONSTRAINT site_imports_status_check CHECK ((status = ANY (ARRAY['importing'::text, 'completed'::text, 'failed'::text])))
  CREATE UNIQUE INDEX site_imports_running ON public.site_imports USING btree (site_id) WHERE (status = 'importing'::text)
  CREATE INDEX site_imports_site ON public.site_imports USING btree (site_id)

TABLE site_memberships
  id bigint NOT NULL GENERATED ALWAYS AS IDENTITY
  site_id bigint NOT NULL
  user_id bigint NOT NULL
  role text NOT NULL
  deleted_at timestamp with time zone
  created_at timestamp with time zone NOT NULL DEFAULT now()
  updated_at timestamp with time zone NOT NULL DEFAULT now()
  CONSTRAINT site_memberships_pkey PRIMARY KEY (id)
  CONSTRAINT site_memberships_role_check CHECK ((role = ANY (ARRAY['editor'::text, 'viewer'::text])))
  CONSTRAINT site_memberships_site_id_fkey FOREIGN KEY (site_id) REFERENCES sites(id)
  CONSTRAINT site_memberships_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id)
  CREATE UNIQUE INDEX site_memberships_site_user ON public.site_memberships USING btree (site_id, user_id) WHERE (deleted_at IS NULL)
  CREATE INDEX site_memberships_updated ON public.site_memberships USING btree (updated_at)
  CREATE INDEX site_memberships_user ON public.site_memberships USING btree (user_id) WHERE (deleted_at IS NULL)
  CREATE TRIGGER site_memberships_set_updated_at BEFORE UPDATE ON public.site_memberships FOR EACH ROW EXECUTE FUNCTION set_updated_at()

TABLE site_settings
  site_id bigint NOT NULL
  collect_enabled boolean NOT NULL DEFAULT true
  ingest_rate_limit_per_min integer
  retention_days integer
  geo_level text NOT NULL DEFAULT 'city'::text
  strip_query_params boolean NOT NULL DEFAULT true
  allowed_event_props text[]
  week_starts_on smallint NOT NULL DEFAULT 1
  tracker jsonb NOT NULL DEFAULT '{}'::jsonb
  created_at timestamp with time zone NOT NULL DEFAULT now()
  updated_at timestamp with time zone NOT NULL DEFAULT now()
  spike_threshold integer
  CONSTRAINT site_settings_geo_level_check CHECK ((geo_level = ANY (ARRAY['country'::text, 'region'::text, 'city'::text])))
  CONSTRAINT site_settings_ingest_rate_limit_per_min_check CHECK ((ingest_rate_limit_per_min > 0))
  CONSTRAINT site_settings_pkey PRIMARY KEY (site_id)
  CONSTRAINT site_settings_retention_days_check CHECK ((retention_days > 0))
  CONSTRAINT site_settings_site_id_fkey FOREIGN KEY (site_id) REFERENCES sites(id)
  CONSTRAINT site_settings_spike_threshold_check CHECK ((spike_threshold >= 1))
  CONSTRAINT site_settings_week_starts_on_check CHECK (((week_starts_on >= 0) AND (week_starts_on <= 6)))
  CREATE INDEX site_settings_updated ON public.site_settings USING btree (updated_at)
  CREATE TRIGGER site_settings_notify_changed AFTER INSERT OR UPDATE ON public.site_settings FOR EACH ROW EXECUTE FUNCTION notify_site_changed('site_id')
  CREATE TRIGGER site_settings_set_updated_at BEFORE UPDATE ON public.site_settings FOR EACH ROW EXECUTE FUNCTION set_updated_at()

TABLE sites
  id bigint NOT NULL GENERATED ALWAYS AS IDENTITY
  organization_id bigint NOT NULL
  site_key text NOT NULL
  name text NOT NULL
  timezone text NOT NULL DEFAULT 'UTC'::text
  is_public boolean NOT NULL DEFAULT false
  deleted_at timestamp with time zone
  created_at timestamp with time zone NOT NULL DEFAULT now()
  updated_at timestamp with time zone NOT NULL DEFAULT now()
  CONSTRAINT sites_organization_id_fkey FOREIGN KEY (organization_id) REFERENCES organizations(id)
  CONSTRAINT sites_pkey PRIMARY KEY (id)
  CONSTRAINT sites_site_key_check CHECK ((site_key ~ '^[a-z2-7]{12}$'::text))
  CREATE INDEX sites_org ON public.sites USING btree (organization_id) WHERE (deleted_at IS NULL)
  CREATE UNIQUE INDEX sites_site_key ON public.sites USING btree (site_key)
  CREATE INDEX sites_updated ON public.sites USING btree (updated_at)
  CREATE TRIGGER sites_notify_changed AFTER INSERT OR UPDATE ON public.sites FOR EACH ROW EXECUTE FUNCTION notify_site_changed('id')
  CREATE TRIGGER sites_set_updated_at BEFORE UPDATE ON public.sites FOR EACH ROW EXECUTE FUNCTION set_updated_at()

TABLE traffic_spikes
  site_id bigint NOT NULL
  started_at timestamp with time zone NOT NULL
  below_since timestamp with time zone
  CONSTRAINT traffic_spikes_pkey PRIMARY KEY (site_id)
  CONSTRAINT traffic_spikes_site_id_fkey FOREIGN KEY (site_id) REFERENCES sites(id)

TABLE user_totp
  user_id bigint NOT NULL
  secret_enc bytea NOT NULL
  enabled_at timestamp with time zone
  last_step bigint NOT NULL DEFAULT 0
  recovery_hashes bytea[] NOT NULL DEFAULT '{}'::bytea[]
  created_at timestamp with time zone NOT NULL DEFAULT now()
  CONSTRAINT user_totp_pkey PRIMARY KEY (user_id)
  CONSTRAINT user_totp_secret_enc_check CHECK ((octet_length(secret_enc) = 48))
  CONSTRAINT user_totp_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id)

TABLE users
  id bigint NOT NULL GENERATED ALWAYS AS IDENTITY
  email citext NOT NULL
  password_hash text NOT NULL
  name text
  is_instance_admin boolean NOT NULL DEFAULT false
  email_verified_at timestamp with time zone
  disabled_at timestamp with time zone
  deleted_at timestamp with time zone
  created_at timestamp with time zone NOT NULL DEFAULT now()
  updated_at timestamp with time zone NOT NULL DEFAULT now()
  CONSTRAINT users_email_check CHECK (((char_length((email)::text) >= 3) AND (char_length((email)::text) <= 320)))
  CONSTRAINT users_pkey PRIMARY KEY (id)
  CREATE UNIQUE INDEX users_email ON public.users USING btree (email) WHERE (deleted_at IS NULL)
  CREATE TRIGGER users_set_updated_at BEFORE UPDATE ON public.users FOR EACH ROW EXECUTE FUNCTION set_updated_at()

TABLE weekly_report_claims
  user_id bigint NOT NULL
  site_id bigint NOT NULL
  period_end timestamp with time zone NOT NULL
  confirmed_at timestamp with time zone
  unsubscribe_hash bytea
  kind text NOT NULL DEFAULT 'weekly'::text
  CONSTRAINT weekly_report_claims_kind_check CHECK ((kind = ANY (ARRAY['weekly'::text, 'monthly'::text, 'spike'::text])))
  CONSTRAINT weekly_report_claims_pkey PRIMARY KEY (user_id, site_id, kind, period_end)
  CONSTRAINT weekly_report_claims_site_id_fkey FOREIGN KEY (site_id) REFERENCES sites(id)
  CONSTRAINT weekly_report_claims_unsubscribe_hash_check CHECK ((octet_length(unsubscribe_hash) = 32))
  CONSTRAINT weekly_report_claims_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id)
  CREATE UNIQUE INDEX weekly_report_unsubscribe_hash ON public.weekly_report_claims USING btree (unsubscribe_hash) WHERE (unsubscribe_hash IS NOT NULL)

TABLE weekly_report_emails
  user_id bigint NOT NULL
  email text NOT NULL
  lang text NOT NULL
  requested_at timestamp with time zone
  token_hash bytea
  token_expires_at timestamp with time zone
  confirmed_at timestamp with time zone
  CONSTRAINT weekly_report_emails_check CHECK (((token_hash IS NULL) = (token_expires_at IS NULL)))
  CONSTRAINT weekly_report_emails_check1 CHECK (((token_hash IS NULL) OR (confirmed_at IS NULL)))
  CONSTRAINT weekly_report_emails_lang_check CHECK ((lang = ANY (ARRAY['zh'::text, 'en'::text])))
  CONSTRAINT weekly_report_emails_pkey PRIMARY KEY (user_id)
  CONSTRAINT weekly_report_emails_token_hash_check CHECK ((octet_length(token_hash) = 32))
  CONSTRAINT weekly_report_emails_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id)
  CREATE UNIQUE INDEX weekly_report_email_token ON public.weekly_report_emails USING btree (token_hash) WHERE (token_hash IS NOT NULL)

TABLE weekly_report_subscriptions
  user_id bigint NOT NULL
  site_id bigint NOT NULL
  email text NOT NULL
  lang text NOT NULL
  confirmed_at timestamp with time zone NOT NULL
  kind text NOT NULL DEFAULT 'weekly'::text
  CONSTRAINT weekly_report_subscriptions_kind_check CHECK ((kind = ANY (ARRAY['weekly'::text, 'monthly'::text, 'spike'::text])))
  CONSTRAINT weekly_report_subscriptions_lang_check CHECK ((lang = ANY (ARRAY['zh'::text, 'en'::text])))
  CONSTRAINT weekly_report_subscriptions_pkey PRIMARY KEY (user_id, site_id, kind)
  CONSTRAINT weekly_report_subscriptions_site_id_fkey FOREIGN KEY (site_id) REFERENCES sites(id)
  CONSTRAINT weekly_report_subscriptions_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id)
  CREATE INDEX weekly_report_subscriptions_site ON public.weekly_report_subscriptions USING btree (site_id)

EXTENSION citext

CREATE OR REPLACE FUNCTION public.notify_site_changed()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
BEGIN
    PERFORM pg_notify('rivu_site_changed', to_jsonb(NEW) ->> TG_ARGV[0]);
    RETURN NULL;
END;
$function$

CREATE OR REPLACE FUNCTION public.set_updated_at()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
BEGIN
    NEW.updated_at = now();
    RETURN NEW;
END;
$function$
