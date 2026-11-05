@FR-WS-8
Feature: A disabled stem is a real state; its dependants start without it
  `enabled:` takes a bool or a substituted string (`"${env.WITH_SEARCH}"`;
  an unset variable is false). A disabled stem shows in `status` as
  `disabled` (listed last, counted in `summary.disabled`). A stem with a hard
  edge to it starts anyway (51: `if_disabled: start` is the default): the
  edge is dropped and `STEMS_<DEP>_PORT` is absent (`${stem.<dep>.port}`
  still resolves to the configured port); `validate` warns
  `DEPENDENCY_DISABLED`. An edge
  marked `if_disabled: skip` makes `up` skip the dependant instead
  (`skipped_reasons.<stem> == "dependency_disabled:<dep>"`).
  Fixture: tests/fixtures/workspaces/disabled-demo.

  Scenario: validate warns about both edges and notes the unset variable
    Given the fixture workspace "disabled-demo"
    When I run "stems validate --json"
    Then the command succeeds
    And the JSON at "$.data.warnings[?@.code=='DEPENDENCY_DISABLED' && @.details.stem=='api'].details" equals {"stem": "api", "dependency": "search", "if_disabled": "start"}
    And the JSON at "$.data.warnings[?@.code=='DEPENDENCY_DISABLED' && @.details.stem=='api'].message" equals "stem `api` depends on `search`, which is disabled; `api` starts without it"
    And the JSON at "$.data.warnings[?@.code=='DEPENDENCY_DISABLED' && @.details.stem=='worker'].details" equals {"stem": "worker", "dependency": "search", "if_disabled": "skip"}
    And the JSON at "$.data.warnings[?@.code=='ENABLED_VAR_UNSET'].message" equals "search disabled: WITH_SEARCH is unset"

  Scenario: up starts the dependant without the disabled stem and status shows it
    Given the fixture workspace "disabled-demo"
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["postgres", "api"]
    And the JSON at "$.data.skipped" equals ["worker"]
    And the JSON at "$.data.skipped_reasons.worker" equals "dependency_disabled:search"
    And the JSON at "$.data.skipped_reasons.api" does not exist
    And the JSON at "$.data.disabled" equals ["search"]
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='search'].state" equals "disabled"
    And the JSON at "$.data.stems[?@.name=='search'].enabled" equals false
    And the JSON at "$.data.stems[?@.name=='search'].reason" equals "enabled: ${env.WITH_SEARCH} → false"
    And the JSON at "$.data.stems[-1].name" equals "search"
    And the JSON at "$.data.stems[?@.name=='api'].state" equals "healthy"
    And the JSON at "$.data.stems[?@.name=='api'].reason" equals null
    And the JSON at "$.data.stems[?@.name=='worker'].state" equals "stopped"
    And the JSON at "$.data.stems[?@.name=='worker'].reason" equals "skipped: dependency search is disabled"
    And the JSON at "$.data.summary.disabled" equals 1
    When I run "stems env api --json"
    Then the JSON at "$.data.env.STEMS_POSTGRES_PORT" equals "${port:18471}"
    And the JSON at "$.data.env.STEMS_SEARCH_PORT" does not exist
    When I run "stems status --enabled --json"
    Then the JSON at "$.data.stems[?@.name=='search']" does not exist
    And the JSON at "$.data.summary.disabled" does not exist

  Scenario: with the variable set, search starts and both dependants wait for it
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
