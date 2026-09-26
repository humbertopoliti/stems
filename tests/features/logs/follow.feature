@FR-LG-2
Feature: Following logs
  `stems logs -f` subscribes to the daemon (`subscribe_logs`): it replays
  the last lines, then streams new ones until Ctrl-C/SIGTERM (exit 0) or
  the daemon stops.

  Scenario: new lines reach a follower within 2 s
    Given the "minimal" workspace is up
    When I run "stems logs echo-svc -f --json" in the background
    Then within 5s the background command's output contains [{"stem": "echo-svc", "text": "WARN chaos endpoints enabled under /__chaos/"}] in order
    When the chaos endpoint "logs?n=5&level=warn" is called on "echo-svc"
    Then within 2s the background command's output contains [{"text": "WARN chaos log line 0", "level": "warn"}, {"text": "WARN chaos log line 1"}, {"text": "WARN chaos log line 2"}, {"text": "WARN chaos log line 3"}, {"text": "WARN chaos log line 4"}] in order
    When the background command is stopped
    Then within 2s the background command exits with code 0
    When I run "stems down --json"
    Then the command succeeds

  Scenario: the follower ends when the daemon stops
    Given the "minimal" workspace is up
    When I run "stems logs -f --json --since 1h" in the background
    Then within 5s the background command's output contains [{"text": "WARN chaos endpoints enabled under /__chaos/"}] in order
    When I run "stems down --json"
    Then the command succeeds
    And within 5s the background command exits with code 0

  @slow
  Scenario: an idle follower does not busy-loop
    Given the "minimal" workspace is up
    When I run "stems logs -f --json" in the background
    Then within 5s the background command's output contains [{"stem": "echo-svc"}] in order
    And the background command's CPU is below 2 %
    When the background command is stopped
    Then within 2s the background command exits with code 0
    When I run "stems down --json"
    Then the command succeeds
