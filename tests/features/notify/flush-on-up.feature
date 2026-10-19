@FR-LC-6
Feature: the end of up flushes the pending notification
  A long debounce does not delay the notification past the command that
  caused the failure: `up.finished` (also `cascade.finished` /
  `cascade.aborted`) flushes an open batch at once.

  Scenario: debounce 30s, the notification lands as up returns
    Given the fixture workspace "notify-storm"
    And notifications go to a fake notifier
    And a local override setting notifications.debounce=30s
    When I run "stems up quick --detach --json"
    Then the exit code is 1
    And within 2s the events stream contains {"kind": "notification.sent", "data": {"stems": ["quick"]}}
    And the first event matching {"kind": "up.finished"} is followed by one matching {"kind": "notification.sent"} after 0 to 2000 ms
    When I run "stems down --json"
    Then the command succeeds
