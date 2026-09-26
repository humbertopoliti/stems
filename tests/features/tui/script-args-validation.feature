@FR-UI-2
Feature: The script form validates arguments before sending anything
  The form checks the arguments with the same rules as the daemon
  (`parse_args_json`): a required argument left empty is an inline error
  and no `run_script` is sent.

  Scenario: an empty required email is an inline error, no script starts
    Given the fixture workspace "run-scripts"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems attach --headless --script 'wait:healthy;:;type:create-test-user;Enter;Enter;frame'"
    Then the command succeeds
    And the last frame contains "Run create-test-user (shop-api)"
    And the last frame contains "✗ the required argument was not provided"
    And during 2s the events stream never contains {"kind": "script.started"}
