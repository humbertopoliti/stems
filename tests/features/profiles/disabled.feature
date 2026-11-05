@FR-WS-8
Feature: enabled: false removes a stem from profiles and the graph
  A stem disabled in stems.local.yaml is left out of every profile and
  listed last in `status` as `disabled` (46); a soft edge to it is fine, a
  hard edge to it is a validation warning (DEPENDENCY_DISABLED, 51: `up`
  starts the dependant without it). Fixture:
  tests/fixtures/workspaces/profiles-demo.

  Scenario: a disabled stem is absent from its profile and disabled in status
    Given the fixture workspace "profiles-demo"
    And a local override setting stems.tool.enabled=false
    When I run "stems profiles --json"
    Then the JSON at "$.data.profiles[?@.name=='everything'].stems" equals ["db", "api", "web"]
    And the JSON at "$.data.profiles[?@.name=='everything'].disabled" equals ["tool"]
    When I run "stems up --detach --json --profile everything"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["db", "api", "web"]
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='tool'].state" equals "disabled"
    And the JSON at "$.data.stems[?@.name=='tool'].reason" equals "enabled: false in stems.local.yaml"
    And the JSON at "$.data.stems[?@.name=='web'].state" equals "healthy"

  Scenario: a hard edge to a disabled stem is a validation warning
    Given the fixture workspace "profiles-demo"
    And a local override setting stems.db.enabled=false
    When I run "stems validate --json"
    Then the command succeeds
    And the JSON at "$.data.warnings[?@.code=='DEPENDENCY_DISABLED'].path" equals "stems.api.depends_on"
