@FR-WS-8
Feature: up --profile starts a profile and its hard dependencies
  A profile is a starting set: `up --profile p` also starts the hard
  dependencies of the profile's stems (soft edges never pull stems in) and
  says so with a `profile.expanded` event. With `strict_profiles: true` a
  profile that omits a hard dependency is refused instead. Fixture:
  tests/fixtures/workspaces/profiles-demo (db <- api <- web, and `tool`,
  which web names with a soft edge; profile `web` lists only the leaf).

  Scenario: a profile listing only a leaf also starts its dependencies
    Given the fixture workspace "profiles-demo"
    When I run "stems up --detach --json --profile web"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["db", "api", "web"]
    And the events stream contains {"kind": "profile.expanded", "data": {"profile": "web", "added": ["db", "api"]}} before {"kind": "up.started"}
    And the events stream contains {"kind": "up.started", "data": {"profile": "web", "stems": ["db", "api", "web"]}} before {"stem": "db", "to": "starting"}
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='web'].state" equals "healthy"
    And the JSON at "$.data.stems[?@.name=='tool'].state" does not equal "healthy"
    When I run "stems down --all --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive

  Scenario: a complete profile is not expanded
    Given the fixture workspace "profiles-demo"
    When I run "stems up --detach --json --profile api-only"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["db", "api"]
    And the events stream does not contain {"kind": "profile.expanded"}

  @error
  Scenario: strict_profiles refuses a profile that misses a hard dependency
    Given the fixture workspace "profiles-demo"
    And a local override setting strict_profiles=true
    When I run "stems up --detach --json --profile web"
    Then the exit code is 2
    And the JSON error has code "PROFILE_MISSING_DEPENDENCY" and path "profiles.web"
    And the JSON at "$.errors[0].details.missing" equals ["db", "api"]
    When I run "stems profiles --json"
    Then the command succeeds
    And the JSON at "$.data.profiles[?@.name=='web'].error.code" equals "PROFILE_MISSING_DEPENDENCY"

  @error
  Scenario: an unknown profile is a config error
    Given the fixture workspace "profiles-demo"
    When I run "stems up --detach --json --profile wbe"
    Then the exit code is 2
    And the JSON error has code "UNKNOWN_PROFILE"
    And the JSON at "$.errors[0].hint" contains "did you mean `web`?"

  @error
  Scenario: --profile with stem names is a usage error
    Given the fixture workspace "profiles-demo"
    When I run "stems up --detach --json --profile web api"
    Then the exit code is 2
    And the JSON error has code "USAGE"
