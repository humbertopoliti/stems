@FR-ST-6
Feature: outputs are re-evaluated on every start
  Outputs belong to one start of a stem: `restart` evaluates them again.
  Fixture: tests/fixtures/workspaces/outputs-demo (`counter` publishes the
  content of `counter.txt`).

  Scenario: a command output changes after restart
    Given the fixture workspace "outputs-demo"
    And the file "counter.txt" is written with "1"
    When I run "stems up --detach --json counter"
    Then the command succeeds
    When I run "stems outputs counter --json"
    Then the JSON at "$.data.stems[0].outputs[0].value" equals "1"
    Given the file "counter.txt" is written with "2"
    When I run "stems restart counter --json"
    Then the command succeeds
    When I run "stems outputs counter --json"
    Then the JSON at "$.data.stems[0].outputs[0].value" equals "2"
    And there are exactly 2 events matching {"kind": "stem.outputs", "stem": "counter"}
    And there are exactly 2 events matching {"kind": "script.finished", "stem": "counter", "data": {"script": "outputs", "output": "COUNT", "exit": 0}}
    When I run "stems down --all --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
