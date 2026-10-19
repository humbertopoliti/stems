@FR-LC-6
Feature: one desktop notification when a stem is given up on
  When a stem ends up `failed` and nothing will retry it (here:
  `restart.max` exhausted, `MAX_RESTARTS`), the daemon posts one desktop
  notification saying what failed and why, and emits `notification.sent`.
  The e2e suite routes notifications to a fake notifier
  (`notifications.command`) that logs its `STEMS_NOTIFY_*` environment;
  nothing reaches the developer's Notification Centre.

  Scenario: max 2, the third crash gives up and notifies once
    Given the fixture workspace "restart"
    And notifications go to a fake notifier
    And a local override setting stems.api.restart.max=2
    And a local override setting notifications.debounce=300ms
    When I run "stems up api --detach --json"
    Then the command succeeds
    When the chaos endpoint "crash?code=3" is called on "api"
    Then within 10s the events stream contains {"kind": "stem.restarting", "stem": "api", "data": {"attempt": 1}}
    And within 15s the stem "api" is "healthy"
    When the chaos endpoint "crash?code=3" is called on "api"
    Then within 10s the events stream contains {"kind": "stem.restarting", "stem": "api", "data": {"attempt": 2}}
    And within 15s the stem "api" is "healthy"
    When the chaos endpoint "crash?code=3" is called on "api"
    Then within 10s the events stream contains {"kind": "stem.gave_up", "stem": "api"}
    And within 10s the events stream contains {"kind": "notification.sent", "data": {"stems": ["api"], "codes": ["MAX_RESTARTS"], "backend": "command", "folded": 0}}
    And during 2s there are at most 1 events matching {"kind": "notification.sent"}
    And there are exactly 1 events matching {"kind": "notification.sent"}
    And the events stream does not contain {"kind": "notification.skipped"}
    And the file "${tmp}/notify.log" contains "STEMS_NOTIFY_STEMS=api"
    And the file "${tmp}/notify.log" contains "STEMS_NOTIFY_WORKSPACE=restart"
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
