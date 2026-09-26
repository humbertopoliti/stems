@FR-LG-2
Feature: Log filters and interleaving
  `--grep`, `--level`, `--since`, `--tail` and several stems interleaved by
  timestamp. The `--since 1s` scenario polls until the first burst has
  aged out of the one-second window (no fixed sleep) before the second
  burst, so it proves the relative window rather than a race.

  Scenario: --grep and --tail
    Given the "minimal" workspace is up
    When the chaos endpoint "logs?n=12&level=info" is called on "echo-svc"
    Then the chaos response status is 200
    When I run "stems logs echo-svc --json --grep 'line 1[01]$'"
    Then the command succeeds
    And the JSON nodes at "$[*].text" equal ["INFO chaos log line 10", "INFO chaos log line 11"]
    When I run "stems logs echo-svc --json --grep 'chaos log line' --tail 3"
    Then the command succeeds
    And the JSON nodes at "$[*].text" equal ["INFO chaos log line 9", "INFO chaos log line 10", "INFO chaos log line 11"]
    When I run "stems logs --json --level warn+ --grep 'chaos endpoints'"
    Then the command succeeds
    And the JSON at "$.text" equals "WARN chaos endpoints enabled under /__chaos/"
    When I run "stems down --json"
    Then the command succeeds

  Scenario: --since keeps only the recent window
    Given the "minimal" workspace is up
    When the chaos endpoint "logs?n=3&level=warn" is called on "echo-svc"
    Then within 5s the output of "stems logs echo-svc --json --since 1s --grep 'chaos log line'" has no JSON at "$[?@.level == 'warn']"
    When the chaos endpoint "logs?n=3&level=error" is called on "echo-svc"
    And I run "stems logs echo-svc --json --since 1s --grep 'chaos log line'"
    Then the command succeeds
    And the JSON nodes at "$[*].text" equal ["ERROR chaos log line 0", "ERROR chaos log line 1", "ERROR chaos log line 2"]
    When I run "stems logs echo-svc --json --since 1h --grep 'chaos log line'"
    Then the JSON at "$[0].text" equals "WARN chaos log line 0"
    And the JSON at "$[5].text" equals "ERROR chaos log line 2"
    When I run "stems down --json"
    Then the command succeeds

  Scenario: several stems interleave by timestamp
    Given the fixture workspace "process-chain"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems logs --json"
    Then the command succeeds
    And the JSON at "$[*].ts" is in ascending order
    And the JSON at "$[?@.stem == 'a' && @.text == 'INFO sleeping before bind seconds=0.5']" exists
    And the JSON at "$[?@.stem == 'd' && match(@.text, 'INFO listening on .*')]" exists
    When I run "stems logs a d --json --grep listening"
    Then the JSON nodes at "$[*].stem" equal ["a", "d"]
    When I run "stems down --json"
    Then the command succeeds
