@FR-SC-4
Feature: script output is tagged in the stem's log
  Every line a script prints goes to its stem's log with `stream: script`
  and `tag: <script name>`, next to the process's own `out`/`err` lines;
  `stems logs <stem> --script <name>` selects one script's output.

  Scenario: logs --script returns only that script's lines
    Given the fixture workspace "scripts-env"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems logs api --script setup --json"
    Then the command succeeds
    And the JSON at "$[0].tag" equals "setup"
    And the JSON at "$[0].stream" equals "script"
    And the JSON at "$[?@.tag != 'setup']" does not exist
    And the JSON at "$[?@.stream != 'script']" does not exist
    When I run "stems logs api --json"
    Then the JSON at "$[?@.tag == 'setup']" exists
    When I run "stems down --all --json"
    Then the command succeeds
