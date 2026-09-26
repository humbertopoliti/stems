@FR-WD-1
Feature: up --no-watch starts no watchdogs
  `stems up --no-watch` turns every watchdog off until the next `up`
  without it: changes do nothing and `stems watch status --json` reports
  `disabled: true` and each stem `active: false`.

  Scenario: a change after up --no-watch triggers nothing
    Given the fixture workspace "watch-shop"
    And the workspace has a private copy of the repos
    When I run "stems up api --detach --no-watch --json"
    Then the command succeeds
    And within 15s the stem "api" is "healthy"
    When I run "stems watch status --json"
    Then the command succeeds
    And the JSON at "$.data.disabled" equals true
    And the JSON at "$.data.stems[?@.name == 'api'].active" equals false
    When I run the shell command "echo '# touched' >> ${tmp}/examples/repos/shop-api/app.py"
    Then during 2s the events stream never contains {"kind": "watch.triggered"}
    When I run "stems down --json"
    Then the command succeeds
