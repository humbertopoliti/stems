@FR-DS-2
Feature: A STEMS_HOME too long for a Unix socket is reported up front
  The daemon socket lives at <home>/<hash>/stemsd.sock and must fit in
  sockaddr_un; the client says so instead of timing out waiting for it.

  @error
  Scenario: an overlong STEMS_HOME fails with SOCKET_PATH_TOO_LONG
    Given the "minimal" workspace
    When I run "stems daemon start --json" with env STEMS_HOME=/tmp/stems-long-home-0123456789012345678901234567890123456789012345678901234567890123456789
    Then the exit code is 4
    And the JSON error has code "SOCKET_PATH_TOO_LONG"
    And the JSON at "$.errors[0].message" contains "STEMS_HOME"
    And the JSON at "$.errors[0].hint" contains "shorter STEMS_HOME"

  @error
  Scenario: a foreground daemon with an overlong STEMS_HOME fails with SOCKET_PATH_TOO_LONG
    Given the "minimal" workspace
    When I run "stems daemon start --foreground --json" with env STEMS_HOME=/tmp/stems-long-home-0123456789012345678901234567890123456789012345678901234567890123456789
    Then the exit code is 4
    And the JSON error has code "SOCKET_PATH_TOO_LONG"
