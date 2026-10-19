@FR-LC-6 @FR-WD-3
Feature: notification settings reload with the config
  The notifier reads the applied config at every flush, so a change of
  `notifications:` in stems.local.yaml takes effect for the next failure
  once applied (`config.reload.auto_apply: true` here; it is a hot,
  workspace-level change: `notifications_changed`).

  Scenario: switching enabled off while the daemon runs silences the next failure
    Given the fixture workspace "notify-storm"
    And notifications go to a fake notifier
    And a local override setting config.reload.auto_apply=true
    And a local override setting notifications.debounce=200ms
    And a local override setting notifications.cooldown=200ms
    When I run "stems up quick --detach --json"
    Then the exit code is 1
    And within 5s the events stream contains {"kind": "notification.sent", "data": {"stems": ["quick"]}}
    Given a local override setting notifications.enabled=false
    Then within 5s the events stream contains {"kind": "config.changed", "data": {"plan": {"workspace": ["notifications_changed"]}}}
    And within 5s the events stream contains {"kind": "config.applied", "data": {"auto": true, "workspace": ["notifications_changed"]}}
    When I run "stems up quick2 --detach --json"
    Then the exit code is 1
    And within 5s the events stream contains {"kind": "notification.skipped", "data": {"reason": "disabled", "stems": ["quick2"]}}
    And there are exactly 1 events matching {"kind": "notification.sent"}
    When I run "stems down --json"
    Then the command succeeds
