@FR-WS-8
Feature: enabled: false removes a stem from profiles and the graph
  A stem disabled in stems.local.yaml is left out of every profile and of
  `status`; a soft edge to it is fine, a hard edge to it is a validation
  error (DEPENDENCY_DISABLED). Fixture: tests/fixtures/workspaces/profiles-demo.

  Scenario: a disabled stem is absent from its profile and from status
    Given the fixture workspace "profiles-demo"
    And a local override setting stems.tool.enabled=false
    When I run "stems profiles --json"
    Then the JSON at "$.data.profiles[?@.name=='everything'].stems" equals ["db", "api", "web"]
    And the JSON at "$.data.profiles[?@.name=='everything'].disabled" equals ["tool"]
    When I run "stems up --detach --json --profile everything"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["db", "api", "web"]
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='tool']" does not exist
    And the JSON at "$.data.stems[?@.name=='web'].state" equals "healthy"

  @error
  Scenario: a hard edge to a disabled stem is a validation error
    Given the fixture workspace "profiles-demo"
    And a local override setting stems.db.enabled=false
    When I run "stems validate --json"
    Then the exit code is 2
    And the JSON error has code "DEPENDENCY_DISABLED" and path "stems.api.depends_on"
