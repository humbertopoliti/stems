@FR-WS-8
Feature: the default profile, STEMS_PROFILE and `stems profiles`
  Without stem names, `up` uses `--profile`, else `STEMS_PROFILE`, else the
  local `profile:` override, else `default_profile`, else a profile named
  `default`, else every enabled stem. `stems profiles --json` lists every
  profile with its resolved members, closure and which one is the default.
  Fixture: tests/fixtures/workspaces/profiles-demo (`default_profile: web`).

  Scenario: default_profile picks the profile
    Given the fixture workspace "profiles-demo"
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["db", "api", "web"]
    And the events stream contains {"kind": "profile.expanded", "data": {"profile": "web"}} before {"kind": "up.started"}

  Scenario: STEMS_PROFILE overrides the default
    Given the fixture workspace "profiles-demo"
    When I run "stems up --detach --json" with env STEMS_PROFILE=api-only
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["db", "api"]

  Scenario: the local profile override wins over default_profile
    Given the fixture workspace "profiles-demo"
    And a local override setting profile=all
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" contains "tool"
    And the JSON at "$.data.ready" contains "web"

  Scenario: stem names win over the default profile
    Given the fixture workspace "profiles-demo"
    When I run "stems up --detach --json tool"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["tool"]

  Scenario: profiles --json lists resolved membership
    Given the fixture workspace "profiles-demo"
    When I run "stems profiles --json"
    Then the command succeeds
    And the JSON at "$.data.default" equals "web"
    And the JSON at "$.data.profiles" equals [{"name": "web", "alias_of": null, "stems": ["web"], "closure": ["db", "api", "web"], "expanded": ["db", "api"], "disabled": [], "default": true, "default_source": "default_profile"}, {"name": "api-only", "alias_of": null, "stems": ["api", "db"], "closure": ["db", "api"], "expanded": [], "disabled": [], "default": false}, {"name": "everything", "alias_of": null, "stems": ["db", "api", "web", "tool"], "closure": ["db", "api", "web", "tool"], "expanded": [], "disabled": [], "default": false}, {"name": "all", "alias_of": "everything", "stems": ["db", "api", "web", "tool"], "closure": ["db", "api", "web", "tool"], "expanded": [], "disabled": [], "default": false}]

  Scenario: profiles reports STEMS_PROFILE
    Given the fixture workspace "profiles-demo"
    When I run "stems profiles --json" with env STEMS_PROFILE=api-only
    Then the command succeeds
    And the JSON at "$.data.env_profile" equals "api-only"
