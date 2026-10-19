@FR-LC-6
Feature: notifications can be switched off
  `notifications.enabled: false` in the config, or the environment kill
  switch `STEMS_NOTIFICATIONS=0|off|false` on the daemon (CI, scripts, the
  e2e harness), suppresses the desktop notification: the failure is
  `notification.skipped {reason: "disabled"}` and no backend runs.

  Scenario: notifications.enabled=false skips with reason disabled
    Given the fixture workspace "notify-storm"
    And notifications go to a fake notifier
    And a local override setting notifications.enabled=false
    When I run "stems up quick --detach --json"
    Then the exit code is 1
    And within 5s the events stream contains {"kind": "notification.skipped", "data": {"reason": "disabled", "stems": ["quick"]}}
    And the events stream does not contain {"kind": "notification.sent"}
    And the file "${tmp}/notify.log" does not exist
    When I run "stems down --json"
    Then the command succeeds

  Scenario: STEMS_NOTIFICATIONS=0 wins over enabled: true
    Given the fixture workspace "notify-storm"
    And notifications go to a fake notifier
    And a local override setting notifications.enabled=true
    When I run "stems up quick --detach --json" with env STEMS_NOTIFICATIONS=0
    Then the exit code is 1
    And within 5s the events stream contains {"kind": "notification.skipped", "data": {"reason": "disabled", "stems": ["quick"]}}
    And the events stream does not contain {"kind": "notification.sent"}
    And the file "${tmp}/notify.log" does not exist
    When I run "stems down --json"
    Then the command succeeds
