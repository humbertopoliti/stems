@FR-WD-2 @FR-UI-2
Feature: The Detail view shows the stem's watchdog and p pauses it
  A stem with `watch:` rules has a Watchdog section in Detail: `watch on`
  or `⏸ paused`, the last trigger, then each rule (`paths → action ·
  debounce`). `p` (or `Enter` on the section's row) pauses / resumes it;
  the section and the action bar (`p watch ⏸ paused`) follow.

  Scenario: Detail shows api's rule; p toggles ⏸ in the section and the bar
    Given the fixture workspace "watch-shop"
    When I run "stems up api --detach --json"
    Then the command succeeds
    When I run "stems attach --view detail --headless --size 120x40 --script 'wait:stem=api:healthy;frame;p;wait:event=watch.paused:api;frame;p;wait:event=watch.resumed:api;frame'"
    Then the command succeeds
    And frame 1 contains "Detail: api"
    And frame 1 contains "Watchdog · Enter/p pause·resume"
    And frame 1 contains "watch on · last trigger never"
    And frame 1 contains "*.py → restart · debounce 200ms"
    And frame 1 contains "p watch on"
    And frame 2 contains "⏸ paused · last trigger never"
    And frame 2 contains "p watch ⏸ paused"
    And frame 3 contains "watch on · last trigger never"
    And frame 3 does not contain "⏸"
    And within 2s the events stream contains {"kind": "watch.paused", "stem": "api"}
