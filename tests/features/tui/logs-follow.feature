@FR-LG-3 @FR-UI-1
Feature: The Logs view follows the selected stem, Space pauses it
  `view:logs` subscribes to the selected stem's logs (`subscribe_logs`,
  the last lines replayed first) and follows new lines. `Space` pauses:
  lines arriving meanwhile wait (the title says `paused (+N)`) and appear
  when `Space` resumes.

  Scenario: five chaos lines appear; paused lines appear only on resume
    Given the "minimal" workspace is up
    When I run "stems attach --headless --script 'wait:healthy;view:logs;chaos:logs?n=5;wait:lines>=5;wait:log=chaos log line 4;frame;Space;chaos:logs?n=3&level=warn;wait:log=WARN chaos log line 2;frame;Space;frame'"
    Then the command succeeds
    And frame 1 contains "[Logs]"
    And frame 1 contains "Logs: echo-svc · following"
    And frame 1 contains "INFO chaos log line 0"
    And frame 1 contains "INFO chaos log line 4"
    And frame 2 contains "paused (+"
    And frame 2 contains "INFO chaos log line 4"
    And frame 2 does not contain "WARN chaos log line"
    And frame 3 contains "following"
    And frame 3 contains "WARN chaos log line 0"
    And frame 3 contains "WARN chaos log line 2"
