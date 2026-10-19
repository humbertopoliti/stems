@FR-LC-6
Feature: no notification backend degrades to nothing, once
  Without `notifications.command` and with no `terminal-notifier`,
  `osascript` or `notify-send` on the daemon's PATH, the first batch emits
  `notification.skipped {reason: "no_backend"}`; later batches are silent
  (logged at debug), never retried per failure.

  Scenario: two separate failures, one no_backend event
    Given the fixture workspace "notify-storm"
    And no notification backend is on PATH
    And a local override setting notifications.debounce=200ms
    And a local override setting notifications.cooldown=200ms
    When I run "stems up quick --detach --json"
    Then the exit code is 1
    And within 5s the events stream contains {"kind": "notification.skipped", "data": {"reason": "no_backend", "stems": ["quick"]}}
    When I run "stems up quick2 --detach --json"
    Then the exit code is 1
    And within 5s the events stream contains {"kind": "stem.state", "stem": "quick2", "to": "failed"}
    And during 3s there are at most 1 events matching {"kind": "notification.skipped"}
    And the events stream does not contain {"kind": "notification.sent"}
    When I run "stems down --json"
    Then the command succeeds
