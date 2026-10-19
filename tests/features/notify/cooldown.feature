@FR-LC-6
Feature: a cooldown folds repeated failures into the next notification
  After a notification the workspace is in cooldown
  (`notifications.cooldown`): failures that arrive inside it are held and
  posted together when it ends, with `folded` counting the held failures,
  so a crash loop driven by manual restarts never posts more than two
  notifications a minute at the default 30 s.

  The gap is measured between event timestamps, which are stamped after
  the notifier command ran: a little under the 3 s cooldown is expected.

  Scenario: a second failure inside a 3 s cooldown waits for the cooldown
    Given the fixture workspace "restart"
    And notifications go to a fake notifier
    And a local override setting stems.api.restart.policy=never
    And a local override setting notifications.debounce=200ms
    And a local override setting notifications.cooldown=3s
    When I run "stems up api --detach --json"
    Then the command succeeds
    When the chaos endpoint "crash?code=3" is called on "api"
    Then within 10s the stem "api" is "failed"
    And within 5s the events stream contains {"kind": "notification.sent", "data": {"stems": ["api"], "folded": 0}}
    When I run "stems restart api --json"
    Then the command succeeds
    And within 15s the stem "api" is "healthy"
    When the chaos endpoint "crash?code=3" is called on "api"
    Then within 10s the stem "api" is "failed"
    And within 10s the events stream contains {"kind": "notification.sent", "data": {"stems": ["api"], "folded": 1}}
    And the first event matching {"kind": "notification.sent"} is followed by one matching {"kind": "notification.sent", "data": {"folded": 1}} after 2500 to 15000 ms
    And there are exactly 2 events matching {"kind": "notification.sent"}
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
