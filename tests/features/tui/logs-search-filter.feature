@FR-LG-3 @FR-UI-1
Feature: Searching and filtering the Logs view by level
  `/text<Enter>` searches (case-insensitive): matching lines are marked `*`
  in the gutter and highlighted, the search bar shows the count, `n`/`N`
  move between matches. `L` cycles the level filter
  `all → info+ → warn+ → error`.

  Background:
    Given the "minimal" workspace is up

  Scenario: /line 1 marks the matches
    When I run "stems attach --headless --script 'wait:healthy;view:logs;chaos:logs?n=10&level=error;chaos:logs?n=10&level=info;wait:log=INFO chaos log line 9;/line 1<Enter>;frame'"
    Then the command succeeds
    And the last frame contains "/line 1 · 2 matches · 2/2 (n/N)"
    And the last frame contains "›* "
    And the last frame contains "INFO chaos log line 1"

  Scenario: L twice keeps only the errors (golden, times masked)
    When I run "stems attach --headless --script 'wait:healthy;view:logs;chaos:logs?n=10&level=error;chaos:logs?n=10&level=info;wait:log=INFO chaos log line 9;L;L;frame'"
    Then the command succeeds
    And the last frame contains "level warn+"
    And the last frame does not contain "INFO chaos"
    And the frame matches golden "tui-logs-warn-only" masking TIME
