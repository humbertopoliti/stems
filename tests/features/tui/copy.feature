@FR-UI-1
Feature: Copying a log line (OSC 52)
  `y` copies the selected log line (or the `V` range) to the clipboard
  with an OSC 52 escape sequence (`ESC ] 52 ; c ; <base64> BEL`), which
  works over ssh. Headless mode prints the sequence to stdout.

  Scenario: y on a searched line prints its OSC 52 sequence
    Given the "minimal" workspace is up
    When I run "stems attach --headless --script 'wait:healthy;view:logs;chaos:logs?n=3;wait:log=chaos log line 2;/line 2<Enter>;y;frame'"
    Then the command succeeds
    And stdout contains the OSC 52 sequence for "INFO chaos log line 2"
    And the last frame contains "copied 1 line"

  Scenario: V selects a range, y copies it
    Given the "minimal" workspace is up
    When I run "stems attach --headless --script 'wait:healthy;view:logs;chaos:logs?n=3;wait:log=chaos log line 2;/line 0<Enter>;V;j;j;y;frame'"
    Then the command succeeds
    And stdout contains the OSC 52 sequence for "INFO chaos log line 0\nINFO chaos log line 1\nINFO chaos log line 2"
    And the last frame contains "copied 3 lines"
