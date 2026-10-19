@FR-LC-6
Feature: a storm of failures is one notification
  Failures are debounced: a failure opens a batch that is posted after
  `notifications.debounce` of quiet (capped at five debounces). Eight
  stems that die at the same instant are one `notification.sent` listing
  all eight. Fixture: tests/fixtures/workspaces/notify-storm (`s1`..`s8`
  live for three seconds, then exit 1; policy `never`).

  Scenario: eight stems fail at once, one notification lists them all
    Given the fixture workspace "notify-storm"
    And notifications go to a fake notifier
    When I run "stems up s1 s2 s3 s4 s5 s6 s7 s8 --detach --json"
    Then the command succeeds
    And within 20s the events stream contains {"kind": "notification.sent", "data": {"stems": ["s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8"], "codes": ["EXITED"], "backend": "command", "folded": 0}}
    And during 5s there are at most 1 events matching {"kind": "notification.sent"}
    And there are exactly 8 events matching {"kind": "stem.state", "to": "failed"}
    And the file "${tmp}/notify.log" contains "STEMS_NOTIFY_BODY=8 stems failed: "
    When I run "stems down --json"
    Then the command succeeds
