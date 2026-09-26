@FR-HS-3
Feature: stems status --watch redraws until Ctrl-C
  `--watch <interval>` refreshes the table every interval (seconds or a
  duration); on a pipe frames are separated by a blank line. Ctrl-C ends it
  with exit 0.

  Scenario: a stopped stem shows up in the next frame
    Given the "minimal" workspace is up
    When I run "stems status --watch 0.2 --human" in the background
    Then within 5s the background command's stdout contains "OK healthy"
    When I run "stems stop echo-svc --json"
    Then the command succeeds
    And within 1s the background command's stdout contains "- stopped"
    When the background command is stopped
    Then within 5s the background command exits with code 0
    When I run "stems down --all --json"
    Then the command succeeds

  @error
  Scenario: an invalid interval is a usage error
    Given the "minimal" workspace
    When I run "stems status --watch soon --json"
    Then the exit code is 2
    And the JSON error has code "USAGE"
