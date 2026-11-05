@FR-WS-8
Feature: A disabled stem is a real state; its hard dependants are skipped
  `enabled:` takes a bool or a substituted string (`"${env.WITH_SEARCH}"`;
  an unset variable is false). A disabled stem shows in `status` as
  `disabled` (listed last, counted in `summary.disabled`). A stem with a hard
  edge to it is skipped by `up` (`skipped_reasons.<stem> ==
  "dependency_disabled:<dep>"`) and `validate` warns `DEPENDENCY_DISABLED`;
  an edge marked `if_disabled: start` starts the dependant anyway.
  Fixture: tests/fixtures/workspaces/disabled-demo.

  Scenario: validate warns about the hard edge and notes the unset variable
    Given the fixture workspace "disabled-demo"
    When I run "stems validate --json"
    Then the command succeeds
    And the JSON at "$.data.warnings[?@.code=='DEPENDENCY_DISABLED'].details" equals {"stem": "api", "dependency": "search"}
    And the JSON at "$.data.warnings[?@.code=='ENABLED_VAR_UNSET'].message" equals "search disabled: WITH_SEARCH is unset"

  Scenario: up skips the dependant of a disabled stem and status shows it
    Given the fixture workspace "disabled-demo"
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["postgres", "worker"]
    And the JSON at "$.data.skipped" equals ["api"]
    And the JSON at "$.data.skipped_reasons.api" equals "dependency_disabled:search"
    And the JSON at "$.data.disabled" equals ["search"]
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='search'].state" equals "disabled"
    And the JSON at "$.data.stems[?@.name=='search'].enabled" equals false
    And the JSON at "$.data.stems[?@.name=='search'].reason" equals "enabled: ${env.WITH_SEARCH} → false"
    And the JSON at "$.data.stems[-1].name" equals "search"
    And the JSON at "$.data.stems[?@.name=='api'].state" equals "stopped"
    And the JSON at "$.data.stems[?@.name=='api'].reason" equals "skipped: dependency search is disabled"
    And the JSON at "$.data.stems[?@.name=='worker'].state" equals "healthy"
    And the JSON at "$.data.summary.disabled" equals 1
    When I run "stems status --enabled --json"
    Then the JSON at "$.data.stems[?@.name=='search']" does not exist
    And the JSON at "$.data.summary.disabled" does not exist

  Scenario: with the variable set, search starts and api waits for it
    Given the fixture workspace "disabled-demo"
    When I run "stems up --detach --json" with env WITH_SEARCH=1
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["postgres", "search", "api", "worker"]
    And the JSON at "$.data.skipped" equals []
    And the JSON at "$.data.disabled" does not exist

  @error
  Scenario: an enabled value that is not a bool spelling is a schema error
    Given the fixture workspace "disabled-demo"
    And a local override setting stems.search.enabled=maybe
    When I run "stems validate --json"
    Then the exit code is 2
    And the JSON error has code "SCHEMA_INVALID" and path "stems.search.enabled"
