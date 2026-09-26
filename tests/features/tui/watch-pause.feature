@FR-WD-2 @FR-UI-2
Feature: p pauses and resumes the selected stem's watchdog
  `p` calls `watch_pause` (or `watch_resume` when paused) for the
  selected stem; a paused watchdog shows `⏸` next to the stem's name in
  the table (and on its graph box).

  Scenario: p pauses, p again resumes
    Given the fixture workspace "watch-shop"
    When I run "stems up api --detach --json"
    Then the command succeeds
    When I run "stems attach --view table --headless --script 'wait:stem=api:healthy;p;wait:event=watch.paused:api;frame;p;wait:event=watch.resumed:api;frame'"
    Then the command succeeds
    And frame 1 contains "› api ⏸"
    And frame 1 contains "✓ watch paused: api"
    And frame 2 does not contain "⏸"
    And frame 2 contains "✓ watch resumed: api"
    And within 2s the events stream contains {"kind": "watch.paused", "stem": "api"}
    And within 2s the events stream contains {"kind": "watch.resumed", "stem": "api"}
    When I run "stems events --json --since 0"
    Then the JSON at "$[?@.kind=='watch.paused'].actor" contains "tui:"
