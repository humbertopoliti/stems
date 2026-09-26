@FR-WD-1
Feature: a script action runs a script without restarting
  `action: script:on-change` runs the stem's `on-change` script through the
  custom script path (17) as actor `watchdog`: its output is tagged
  `on-change` in the stem's log and the stem keeps its process.

  Scenario: on-change runs, the pid stays
    Given the fixture workspace "watch-shop"
    And the workspace has a private copy of the repos
    When I run "stems up scripted --detach --json"
    Then the command succeeds
    And within 15s the stem "scripted" is "healthy"
    When I run "stems status scripted --json"
    And I save the JSON at "$.data.stems[0].pid" as "pid"
    And I run the shell command "echo '# touched' >> ${tmp}/examples/repos/shop-api/app.py"
    Then within 5s the events stream contains {"kind": "watch.triggered", "stem": "scripted", "data": {"action": "script:on-change"}}
    And within 15s the events stream contains {"kind": "watch.action_finished", "stem": "scripted", "data": {"ok": true}}
    And within 1s the events stream contains {"kind": "script.finished", "stem": "scripted", "actor": "watchdog", "data": {"script": "on-change", "exit": 0}}
    And the events stream does not contain {"kind": "stem.restarting", "stem": "scripted"}
    When I run "stems status scripted --json"
    Then the JSON at "$.data.stems[0].pid" equals ${var:pid}
    And within 5s the JSON at "$.tag" of "stems logs scripted --script on-change --json" equals "on-change"
    And the JSON at "$.text" contains "on-change ran for scripted"
    When I run "stems down --json"
    Then the command succeeds
