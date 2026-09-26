@FR-UI-2
Feature: S resets the selected stem after a typed confirmation
  `S` asks to type `reset`; anything else clears the field. Then `reset`
  runs: the stem stops, its `reset` script runs and its stamps are
  cleared.

  Scenario: typing reset confirms; the reset script runs, stamps are cleared
    Given the fixture workspace "scripts-stamp"
    And the workspace has a private copy of the repos
    And the file "${tmp}/examples/repos/shop-api/VERSION" is written with "1.0.0"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems stamps --json"
    Then the JSON at "$.data.stamps[*].script" contains "setup"
    When I run "stems attach --headless --script 'wait:healthy;S;type:rese;Enter;frame;type:reset;frame;Enter;wait:event=script.finished:echo-svc;frame'"
    Then the command succeeds
    And frame 1 contains "Type reset to confirm: _"
    And frame 2 contains "Type reset to confirm: reset_"
    And the last frame contains "✓ reset echo-svc"
    And within 5s the events stream contains {"kind": "script.started", "stem": "echo-svc", "data": {"script": "reset"}}
    And within 3s the stem "echo-svc" is "stopped"
    When I run "stems stamps --json"
    Then the JSON at "$.data.stamps[*]" does not exist
