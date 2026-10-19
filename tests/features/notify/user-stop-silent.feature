@FR-LC-6
Feature: stopping stems yourself is never a notification
  `stems stop` and `stems down` are intentional: a stem that goes
  `stopped`, or fails while it is being stopped (`from: stopping`) or while
  a `down` is in flight, posts nothing. The daemon is started explicitly so
  `down` leaves it (and its events) in place for the assertions.

  Scenario: stop and down of running stems emit no notification event
    Given the fixture workspace "restart"
    And notifications go to a fake notifier
    And a local override setting notifications.debounce=200ms
    When I run "stems daemon start --json"
    Then the command succeeds
    When I run "stems up api --detach --json"
    Then the command succeeds
    When I run "stems stop api --json"
    Then the command succeeds
    And within 5s the stem "api" is "stopped"
    When I run "stems start api --json"
    Then the command succeeds
    And within 15s the stem "api" is "healthy"
    When I run "stems down api --json"
    Then the command succeeds
    And within 5s the stem "api" is "stopped"
    And during 3s the events stream never contains {"kind": "notification.sent"}
    And the events stream does not contain {"kind": "notification.skipped"}
    And the file "${tmp}/notify.log" does not exist
    When I run "stems down --all --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
