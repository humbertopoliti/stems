@FR-LG-4 @FR-UI-1
Feature: Structured (JSON) log lines in the Logs view
  A JSON line renders as its level (coloured), its message and its other
  fields collapsed (`▸ k=v ...`); `Enter` on it expands the fields, one per
  line (`key: value`).

  Scenario: Enter on a request line expands request_id
    Given the "minimal" workspace with a local override setting stems.echo-svc.env.SHOP_LOG_JSON="1"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems attach --headless --size 160x30 --script 'wait:healthy;view:logs;chaos:logs?n=2;wait:log=GET /__chaos/logs;/GET /__chaos<Enter>;frame;Enter;frame'"
    Then the command succeeds
    And frame 1 contains "INFO  GET /__chaos/logs 200 ▸"
    And frame 1 contains "request_id="
    And frame 1 does not contain "request_id: "
    And frame 1 contains "INFO  chaos log line 1 ▸ ts="
    And frame 2 contains "INFO  GET /__chaos/logs 200 ▾"
    And frame 2 contains "request_id: "
